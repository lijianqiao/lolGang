//! 透明窗口（架构第 3 节）：每个显示器一个透明、鼠标穿透、不拿焦点、始终置顶的窗口，画轮盘、信号和表情。
//! 窗口管理的做法移植自 lolPing `src/main/overlayManager.ts`（MIT，© 2026 Danrui Meng）。
//!
//! 实现流程：
//! 1. 一个专门的线程拥有全部状态，别处只往它的通道里发消息（`Msg`），所以不需要锁，也不会和主线程互相等。
//!    窗口操作和查显示器都由 Tauri 转到主线程执行，这个线程等结果；主线程从不等这个线程。
//! 2. 显示器：要按位置画新东西（开轮盘、单击信号、队友的信号表情和字）前先查一遍，另外每 10 秒查一遍；
//!    和上次不一样（插拔、改分辨率、改缩放、游戏切了显示模式）就重建：多的窗口关掉、少的补上、其余挪到新位置。
//! 3. 手势事件进来是物理像素：开轮盘时记下中心和所在的显示器，之后每次移动都按那块屏幕的缩放算出偏移、选出高亮的格子，
//!    只在格子变了时通知页面；松开时按同样的算法定格，在中心放那一格的信号或表情。页面只管画，不算规则。
//!    信号轮盘和表情轮盘几何一样，只是格子里的东西不同；同一时间只开一个。
//! 4. 空闲时缩小：页面没东西画 250 毫秒后报告空闲，就把窗口缩成显示器上边的一条（截图工具就不会把它当成要截的窗口，
//!    3.2.0 起窗口真的缩小，不再是整屏大小挪到屏幕外，显卡缓冲跟着变小）；
//!    要画新东西前先挪回来，并重新置顶（Windows 10 偶尔会把置顶窗口压到普通窗口下面）。

pub mod displays;
pub mod geometry;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use serde_json::json;
use tauri::{
    AppHandle, Emitter, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

use crate::crypto::{MemberId, hex};
use crate::input::{Emit, TalkPhase, WheelKind};
use crate::protocol::live::{LiveBody, from_ratio, to_ratio};
use crate::settings2::OverlaySettings;
use displays::{Display, Rect};

/// 所有 WebView2 窗口必须用同一串启动参数（同一数据目录下参数不同会建不起来），主窗口在 `tauri.conf.json` 里写同一串：
/// wry 默认关掉的几项，加上「不要求用户手势就能出声」——透明窗口永远收不到点击。
/// 3.2.0 加了不下组件、不在后台联网：WebView2 会自己下广告拦截列表、语音识别、断字词典这些（新的配置目录 4 分钟就 27 MB），
/// 程序一样都用不到，加上这两个以后只剩 5.6 MB，也不再在后台连微软的服务器。
pub const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required --disable-component-update --disable-background-networking";

/// 定时查显示器的间隔。按位置画的东西（轮盘、信号、表情、字）画之前自己会查（3.1.1），这里只是兜底，3.2.0 从 3 秒放宽到 10 秒。
const POLL: Duration = Duration::from_secs(10);

/// 发给透明窗口线程的消息。
pub enum Msg {
    Input(Emit),
    /// 某个透明窗口报告自己有没有东西在画。
    Idle {
        label: String,
        idle: bool,
    },
    Settings(OverlaySettings),
    /// 设置页的预览：在主显示器中间放一个信号。
    Preview(String),
    /// 主显示器右下角的提示。
    Toast {
        title: String,
        body: String,
    },
    Poll,
    /// 队友的信号：第几号显示器、0–65535 的比例位置。
    RemotePing {
        from: MemberId,
        display: u8,
        x: u16,
        y: u16,
        id: String,
    },
    /// 队友的表情：第几号显示器、比例位置、表情引用和出场效果（已经检查过形状）。
    RemoteEmote {
        from: MemberId,
        display: u8,
        x: u16,
        y: u16,
        emote: String,
        fx: u8,
    },
    /// 设置页的预览：在主显示器中间放一个表情。
    EmotePreview(String),
    /// 功能页的预览：在主显示器中间按给的样子放一句字。
    TextPreview {
        style: String,
        size: u32,
        effect: String,
    },
    /// 一句话：自己的（from 为 None）或队友的，第几号显示器、比例位置，已经清理过。
    Text {
        from: Option<MemberId>,
        display: u8,
        x: u16,
        y: u16,
        text: String,
    },
    /// 队友的名字，信号下面的标签用。
    Names(Vec<(MemberId, String)>),
    /// 语音：自己在录时还剩几秒，哪些队友在说（主显示器右下角）。
    Speaking {
        me: Option<u64>,
        others: Vec<MemberId>,
    },
}

/// 透明窗口线程交出去的事，都在这个线程上调用，只能做不阻塞的事。
pub struct Hooks {
    /// 暂停快捷键切换了开关。
    pub on_toggled: Box<dyn Fn(bool) + Send>,
    /// 本人放了一个信号或表情，交给队伍。
    pub on_live: Box<dyn Fn(LiveBody) + Send>,
    /// 按了队伍文字快捷键。
    pub on_text: Box<dyn Fn() + Send>,
    /// 说话键：开始录、说完了、丢掉。
    pub on_talk: Box<dyn Fn(TalkPhase) + Send>,
    /// 面板键按下或松开。
    pub on_panel: Box<dyn Fn(bool) + Send>,
}

/// 透明窗口线程的把手，可以随便复制。
#[derive(Clone)]
pub struct Overlays {
    tx: Sender<Msg>,
}

impl Overlays {
    /// 启动透明窗口线程。
    pub fn start(app: &AppHandle, settings: OverlaySettings, hooks: Hooks) -> Self {
        let (tx, rx) = mpsc::channel();
        let actor = Actor {
            app: app.clone(),
            settings,
            hooks,
            names: HashMap::new(),
            voice_shown: false,
            displays: Vec::new(),
            windows: HashMap::new(),
            next_id: 0,
            wheel: None,
        };
        std::thread::Builder::new()
            .name("overlays".into())
            .spawn(move || actor.run(rx))
            .expect("能开线程");
        let poll = tx.clone();
        std::thread::Builder::new()
            .name("overlays-poll".into())
            .spawn(move || {
                while poll.send(Msg::Poll).is_ok() {
                    std::thread::sleep(POLL);
                }
            })
            .expect("能开线程");
        Self { tx }
    }

    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
    }
}

struct Win {
    label: String,
    window: WebviewWindow,
    display: Display,
    idle: bool,
}

/// 开着的轮盘：哪一种、在哪块显示器、中心在窗口里的哪儿、当前高亮哪格。
struct OpenWheel {
    kind: WheelKind,
    key: String,
    center: (f64, f64),
    hot: Option<usize>,
}

struct Actor {
    app: AppHandle,
    settings: OverlaySettings,
    hooks: Hooks,
    names: HashMap<MemberId, String>,
    /// 主显示器上正画着语音的提示。
    voice_shown: bool,
    /// 按编号排好的显示器（第 0 个是 1 号，主显示器）。
    displays: Vec<Display>,
    windows: HashMap<String, Win>,
    next_id: u32,
    wheel: Option<OpenWheel>,
}

impl Actor {
    fn run(mut self, rx: Receiver<Msg>) {
        while let Ok(m) = rx.recv() {
            match m {
                Msg::Input(e) => self.input(e),
                Msg::Idle { label, idle } => self.idle(&label, idle),
                Msg::Settings(s) => {
                    self.settings = s;
                    for w in self.windows.values() {
                        let _ =
                            w.window
                                .emit_to(w.label.as_str(), "overlay:settings", &self.settings);
                    }
                }
                // 预览、提示、语音的提示只画在主显示器上，不用先查显示器（3.1.1）：
                // 显示器变了，最多 3 秒后的轮询就会重建窗口；要按位置放的东西（轮盘、信号、表情、冒字）才先查一遍
                Msg::Preview(id) => {
                    if let Some(d) = self.displays.first().cloned() {
                        let (w, h) = (
                            f64::from(d.rect.width) / d.scale,
                            f64::from(d.rect.height) / d.scale,
                        );
                        self.show(
                            &d.key,
                            "ping:spawn",
                            json!({ "id": id, "x": w / 2.0, "y": h / 2.0 }),
                        );
                    }
                }
                Msg::Toast { title, body } => {
                    if let Some(d) = self.displays.first().cloned() {
                        self.show(
                            &d.key,
                            "toast:show",
                            json!({ "title": title, "body": body }),
                        );
                    }
                }
                Msg::TextPreview {
                    style,
                    size,
                    effect,
                } => {
                    if let Some(d) = self.displays.first().cloned() {
                        let (w, h) = css_size(&d);
                        self.show(
                            &d.key,
                            "text:spawn",
                            json!({
                                "owner": "preview", "text": "大龙刷了，集合打团！", "name": "我", "color": "#c8aa6e",
                                "style": style, "size": size, "effect": effect, "x": w / 2.0, "y": h / 2.0,
                            }),
                        );
                    }
                }
                Msg::EmotePreview(emote) => {
                    if let Some(d) = self.displays.first().cloned() {
                        let (w, h) = css_size(&d);
                        let fx = self.own_fx(&emote);
                        self.show(
                            &d.key,
                            "emote:spawn",
                            json!({ "owner": "preview", "emote": emote, "fx": fx, "mine": true, "x": w / 2.0, "y": h / 2.0 }),
                        );
                    }
                }
                Msg::RemoteEmote {
                    from,
                    display,
                    x,
                    y,
                    emote,
                    fx,
                } => self.remote_emote(from, display, (x, y), emote, fx),
                Msg::Text {
                    from,
                    display,
                    x,
                    y,
                    text,
                } => self.text(from, display, (x, y), text),
                Msg::Poll => self.refresh(),
                Msg::Names(list) => self.names = list.into_iter().collect(),
                Msg::Speaking { me, others } => self.speaking(me, &others),
                Msg::RemotePing {
                    from,
                    display,
                    x,
                    y,
                    id,
                } => self.remote_ping(from, display, x, y, id),
            }
        }
    }

    fn input(&mut self, e: Emit) {
        match e {
            Emit::WheelOpen { x, y, wheel } => {
                self.refresh();
                let Some(d) = self.display_at(x, y) else {
                    return;
                };
                let center = displays::to_local(&d, x, y);
                self.wheel = Some(OpenWheel {
                    kind: wheel,
                    key: d.key.clone(),
                    center,
                    hot: None,
                });
                let kind = if wheel == WheelKind::Emote {
                    "emote"
                } else {
                    "ping"
                };
                self.show(
                    &d.key,
                    "wheel:open",
                    json!({ "x": center.0, "y": center.1, "kind": kind }),
                );
            }
            Emit::WheelMove { x, y, .. } => {
                let Some(slot) = self.slot_at(x, y) else {
                    return;
                };
                let Some(w) = self.wheel.as_mut() else { return };
                if w.hot != slot {
                    w.hot = slot;
                    let key = w.key.clone();
                    self.emit(&key, "wheel:hot", json!({ "slot": slot }));
                }
            }
            Emit::WheelRelease { x, y, .. } => {
                let slot = self.slot_at(x, y).flatten();
                let Some(w) = self.wheel.take() else { return };
                self.emit(&w.key, "wheel:close", json!(null));
                if w.kind == WheelKind::Emote {
                    if let Some(emote) =
                        slot.and_then(|i| self.settings.emote_wheel.get(i)).cloned()
                    {
                        self.own_emote(&w.key, w.center, emote);
                    }
                } else if let Some(id) = slot.and_then(|i| self.settings.wheel.get(i)).cloned() {
                    self.emit(
                        &w.key,
                        "ping:spawn",
                        json!({ "id": id, "x": w.center.0, "y": w.center.1 }),
                    );
                    self.share_ping(&w.key, w.center, id);
                }
            }
            Emit::Click { x, y, wheel } => {
                self.refresh();
                let Some(d) = self.display_at(x, y) else {
                    return;
                };
                let (lx, ly) = displays::to_local(&d, x, y);
                if wheel == WheelKind::Emote {
                    let emote = self.settings.click_emote.clone();
                    self.own_emote(&d.key, (lx, ly), emote);
                    return;
                }
                let id = self.settings.click_ping_id.clone();
                self.show(&d.key, "ping:spawn", json!({ "id": id, "x": lx, "y": ly }));
                self.share_ping(&d.key, (lx, ly), id);
            }
            Emit::Cancel => self.cancel_wheel(),
            Emit::TextRequested => (self.hooks.on_text)(),
            Emit::Talk(p) => (self.hooks.on_talk)(p),
            Emit::Panel { show } => (self.hooks.on_panel)(show),
            Emit::Toggled { enabled } => {
                if !enabled {
                    self.cancel_wheel();
                }
                (self.hooks.on_toggled)(enabled);
            }
        }
    }

    /// 本人的信号交给队伍：位置换成「第几号显示器 + 比例」，落到队友同号显示器的同一比例处。
    fn share_ping(&self, key: &str, at: (f64, f64), id: String) {
        if let Some((display, x, y)) = self.shared_pos(key, at) {
            (self.hooks.on_live)(LiveBody::Ping { display, x, y, id });
        }
    }

    fn shared_pos(&self, key: &str, (lx, ly): (f64, f64)) -> Option<(u8, u16, u16)> {
        let i = self.displays.iter().position(|d| d.key == key)?;
        let (w, h) = css_size(&self.displays[i]);
        let display = u8::try_from(i + 1).unwrap_or(u8::MAX);
        Some((display, to_ratio(lx, w), to_ratio(ly, h)))
    }

    /// 导入表情自己选的出场效果；内置表情是 0。
    fn own_fx(&self, emote: &str) -> u8 {
        self.settings
            .custom_emotes
            .iter()
            .find(|c| c.id == emote)
            .map_or(0, |c| c.fx)
    }

    /// 本人的表情：画在这里，同时交给队伍。
    fn own_emote(&mut self, key: &str, (lx, ly): (f64, f64), emote: String) {
        let fx = self.own_fx(&emote);
        self.show(
            key,
            "emote:spawn",
            json!({ "owner": "self", "emote": emote, "fx": fx, "mine": true, "x": lx, "y": ly }),
        );
        if let Some((display, x, y)) = self.shared_pos(key, (lx, ly)) {
            (self.hooks.on_live)(LiveBody::Emote {
                display,
                x,
                y,
                emote,
                fx,
            });
        }
    }

    /// 队友的表情：和信号一样落到同号显示器，下面带名字；每人同时只留一个。
    fn remote_emote(
        &mut self,
        from: MemberId,
        display: u8,
        (x, y): (u16, u16),
        emote: String,
        fx: u8,
    ) {
        self.refresh();
        let Some(d) = self.numbered(display) else {
            return;
        };
        let (w, h) = css_size(&d);
        let name = self
            .names
            .get(&from)
            .cloned()
            .unwrap_or_else(|| "队友".into());
        let owner = hex(&from[..6]);
        self.show(
            &d.key,
            "emote:spawn",
            json!({
                "owner": owner, "emote": emote, "fx": fx, "mine": false,
                "x": from_ratio(x, w), "y": from_ratio(y, h),
                "tag": { "name": name, "color": tag_color(&from) },
            }),
        );
    }

    /// 队友的信号：落到本机同号显示器（没有这一号就落到 1 号）的同一比例处，下面带他的名字。
    fn remote_ping(&mut self, from: MemberId, display: u8, x: u16, y: u16, id: String) {
        self.refresh();
        let Some(d) = self.numbered(display) else {
            return;
        };
        let (w, h) = css_size(&d);
        let name = self
            .names
            .get(&from)
            .cloned()
            .unwrap_or_else(|| "队友".into());
        let tag = json!({ "name": name, "color": tag_color(&from) });
        self.show(
            &d.key,
            "ping:spawn",
            json!({ "id": id, "x": from_ratio(x, w), "y": from_ratio(y, h), "tag": tag }),
        );
    }

    /// 一句话：落到同号显示器的同一比例处，带说话人的名字；按设置在原处冒出来或者当弹幕飘过。
    fn text(&mut self, from: Option<MemberId>, display: u8, (x, y): (u16, u16), text: String) {
        self.refresh();
        let Some(d) = self.numbered(display) else {
            return;
        };
        let (w, h) = css_size(&d);
        let (owner, name, color) = match from {
            Some(m) => (
                hex(&m[..6]),
                self.names.get(&m).cloned().unwrap_or_else(|| "队友".into()),
                tag_color(&m),
            ),
            None => ("self".into(), "我".into(), "#c8aa6e"),
        };
        self.show(
            &d.key,
            "text:spawn",
            json!({
                "owner": owner, "text": text, "name": name, "color": color, "style": self.settings.text_style,
                "size": self.settings.text_size_px, "effect": self.settings.text_effect,
                "x": from_ratio(x, w), "y": from_ratio(y, h),
            }),
        );
    }

    /// 第 `display` 号显示器（从 1 数）；本机没有这一号就用 1 号。
    fn numbered(&self, display: u8) -> Option<Display> {
        usize::from(display)
            .checked_sub(1)
            .and_then(|i| self.displays.get(i))
            .or(self.displays.first())
            .cloned()
    }

    /// 开着的轮盘里，物理像素的点对应哪格。外层 None 表示没有开着的轮盘，内层 None 表示在中心的死区里。
    fn slot_at(&self, x: i32, y: i32) -> Option<Option<usize>> {
        let w = self.wheel.as_ref()?;
        let d = self.displays.iter().find(|d| d.key == w.key)?;
        let (lx, ly) = displays::to_local(d, x, y);
        let dead = geometry::dead_zone(f64::from(self.settings.wheel_size_px));
        Some(geometry::pick_slice(lx - w.center.0, ly - w.center.1, dead))
    }

    fn display_at(&self, x: i32, y: i32) -> Option<Display> {
        displays::display_at(&self.displays, x, y).map(|i| self.displays[i].clone())
    }

    fn cancel_wheel(&mut self) {
        if let Some(w) = self.wheel.take() {
            self.emit(&w.key, "wheel:close", json!(null));
        }
    }

    fn emit(&self, key: &str, event: &str, payload: serde_json::Value) {
        if let Some(w) = self.windows.get(key) {
            let _ = w.window.emit_to(w.label.as_str(), event, payload);
        }
    }

    /// 语音的提示画在主显示器右下角。从没有变成有时才挪回、置顶窗口，之后（比如每秒的倒计时）只通知页面。
    fn speaking(&mut self, me: Option<u64>, others: &[MemberId]) {
        let Some(d) = self.displays.first().cloned() else {
            return;
        };
        let list: Vec<serde_json::Value> = others
            .iter()
            .map(|m| {
                let name = self.names.get(m).cloned().unwrap_or_else(|| "队友".into());
                json!({ "name": name, "color": tag_color(m) })
            })
            .collect();
        let has = me.is_some() || !list.is_empty();
        let payload = json!({ "me": me, "others": list });
        if has && !self.voice_shown {
            self.show(&d.key, "voice:state", payload);
        } else {
            self.emit(&d.key, "voice:state", payload);
        }
        self.voice_shown = has;
    }

    /// 画新东西：先把窗口挪回显示器上、重新置顶，再通知页面。
    fn show(&mut self, key: &str, event: &str, payload: serde_json::Value) {
        if let Some(w) = self.windows.get_mut(key) {
            if w.idle {
                w.idle = false;
                place(w);
            }
            // tao 在已经置顶时不会再调 SetWindowPos，先取消再设，等于重新放到所有置顶窗口的最上面。
            let _ = w.window.set_always_on_top(false);
            let _ = w.window.set_always_on_top(true);
        }
        self.emit(key, event, payload);
    }

    fn idle(&mut self, label: &str, idle: bool) {
        if let Some(w) = self.windows.values_mut().find(|w| w.label == label)
            && w.idle != idle
        {
            w.idle = idle;
            place(w);
        }
    }

    /// 查一遍显示器，变了就重建窗口。
    fn refresh(&mut self) {
        let Some(now) = current_displays(&self.app) else {
            return;
        };
        if now == self.displays {
            return;
        }
        self.cancel_wheel();
        self.displays = now;
        let keys: Vec<String> = self.displays.iter().map(|d| d.key.clone()).collect();
        self.windows.retain(|k, w| {
            let keep = keys.contains(k);
            if !keep {
                let _ = w.window.destroy();
            }
            keep
        });
        for d in self.displays.clone() {
            if let Some(w) = self.windows.get_mut(&d.key) {
                w.display = d;
                place(w);
                continue;
            }
            let label = format!("overlay-{}", self.next_id);
            self.next_id += 1;
            match create_window(&self.app, &label) {
                Ok(window) => {
                    let mut w = Win {
                        label,
                        window,
                        display: d.clone(),
                        idle: false,
                    };
                    place(&mut w);
                    let _ = w.window.show();
                    place(&mut w); // 再放一次：不同缩放的屏幕上第一次可能算错大小
                    self.windows.insert(d.key, w);
                }
                Err(e) => eprintln!("透明窗口建不起来：{e}"),
            }
        }
    }
}

/// 显示器在透明窗口里的大小（CSS 像素）。
fn css_size(d: &Display) -> (f64, f64) {
    (
        f64::from(d.rect.width) / d.scale,
        f64::from(d.rect.height) / d.scale,
    )
}

/// 队友名字标签的颜色：按公钥固定挑一种，同一个人在所有人屏幕上颜色一样。
pub fn tag_color(m: &MemberId) -> &'static str {
    const PALETTE: [&str; 10] = [
        "#4fc3f7", "#ffb74d", "#81c784", "#f06292", "#ba68c8", "#fff176", "#4db6ac", "#ff8a65",
        "#9575cd", "#aed581",
    ];
    PALETTE[usize::from(m[0]) % PALETTE.len()]
}

fn create_window(app: &AppHandle, label: &str) -> tauri::Result<WebviewWindow> {
    let w = floating_window(app, label, "overlay.html", "lolPing overlay", false)?;
    w.set_ignore_cursor_events(true)?;
    Ok(w)
}

/// 透明窗口和三个小窗口（队伍面板、投票、聊天框，`panel.rs`）共用的建法：透明、无边框、没有阴影、不能拉大小、
/// 不进任务栏、置顶、先藏着、同一串浏览器参数，不拿焦点。`focusable`：要打字的聊天框才能拿焦点。
pub fn floating_window(
    app: &AppHandle,
    label: &str,
    page: &str,
    title: &str,
    focusable: bool,
) -> tauri::Result<WebviewWindow> {
    WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        // 页面一开始就知道用哪种语言（3.3.2，见 i18n.rs 第 3 条）；之后变了收 lang://changed
        .initialization_script(format!(
            "window.__LOLPING_LANG__ = {:?};",
            crate::i18n::current().code()
        ))
        .title(title)
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .always_on_top(true)
        // macOS：切换桌面（Spaces）时跟着走；Windows 上不起作用
        .visible_on_all_workspaces(true)
        .focusable(focusable)
        .focused(false)
        .visible(false)
        .additional_browser_args(BROWSER_ARGS)
        .build()
}

/// 把窗口放到它该在的位置：有东西画时盖住显示器，空闲时缩成显示器上边的一条。
fn place(w: &mut Win) {
    let r = if w.idle {
        displays::parked_rect(&w.display)
    } else {
        displays::overlay_rect(&w.display)
    };
    let _ = w.window.set_position(PhysicalPosition::new(r.x, r.y));
    let _ = w.window.set_size(PhysicalSize::new(
        r.width.max(1) as u32,
        r.height.max(1) as u32,
    ));
    if !w.idle && !w.window.is_visible().unwrap_or(true) {
        let _ = w.window.show();
    }
}

/// 系统现在的显示器，按编号排好。查不到时返回 None（保持原样）。
pub fn current_displays(app: &AppHandle) -> Option<Vec<Display>> {
    let monitors = app.available_monitors().ok()?;
    let primary = app
        .primary_monitor()
        .ok()
        .flatten()
        .and_then(|m| m.name().cloned());
    let list: Vec<Display> = monitors
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let key = m.name().cloned().unwrap_or_else(|| format!("display-{i}"));
            let (p, s) = (m.position(), m.size());
            Display {
                primary: primary.as_ref() == Some(&key),
                key,
                rect: Rect {
                    x: p.x,
                    y: p.y,
                    width: s.width as i32,
                    height: s.height as i32,
                },
                scale: m.scale_factor(),
            }
        })
        .collect();
    Some(displays::numbered(&list))
}
