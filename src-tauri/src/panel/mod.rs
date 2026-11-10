//! 队伍面板、投票、文字的运行时（2.0 架构第 9 节）。窗口和投票流程移植自 lolPing `src/main/panelWindows.ts`、
//! `roomVotes.ts`、`textWindow.ts`（MIT，© 2026 Danrui Meng）。
//!
//! 实现流程：
//! 1. 一个专门的线程拥有投票和三个小窗口（队伍面板 roster、投票 vote、输入框 text），别处只往它的通道里发消息。
//!    有投票时每秒检查一次结果，每 3 秒重发一次自己的回答（实时帧可能丢）；结果出来后投票窗口再留 3 秒。
//!    没有投票时只等消息，不定时醒（3.2.0）。
//!    窗口第一次用到时才建，用完藏起来；藏了 2 分钟还没再用就销毁（3.2.0，每个窗口占一个网页进程，约 35–40 MB），
//!    下次用时再建。命令要的投票状态放在共享的 `Mutex` 里，命令不等这个线程。
//! 2. 队伍面板：按住面板键时在鼠标所在的显示器中间显示，松开收起。面板和投票窗口能点但不拿焦点，
//!    游戏或正在打字的程序不丢键盘。面板页面每次显示时自己取一次队伍信息（team_view）。
//! 3. 投票：发起时定下投票人（本人加上在线、没关投票的队友，`team::voters`），发一帧 Vote；收到队友的 Vote 同样开一个。
//!    回答发 Ballot 帧。规则在 `votes`。踢人通过时，被踢的人自己的程序退出队伍：队伍记录里没有「踢人」，靠被踢的人配合。
//! 4. 聊天框（3.0-M4，原来的输入框）：文字快捷键打开、再按一次收起，放在鼠标所在显示器的左下角（要拿焦点才能打字）；
//!    上面是历史记录（页面自己取 `history_list`），下面是输入框。记下按键时鼠标的位置；回车后清理、认指令（`team_text`），
//!    普通文字画在自己的透明窗口上、发给队伍、记进历史记录，落到队友同号显示器的同一比例处。发完不关，可以接着说。
//!
//! 文件（3.2.0 从一个 panel.rs 拆开，只搬代码）：`vote.rs` 投票（第 3 条），`text.rs` 聊天框（第 4 条）。
//! 这里留线程、消息、队伍面板（第 2 条）和建窗口、摆位置的小工具。

pub mod text;
pub mod vote;

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewWindow};
use tokio::sync::oneshot;

use crate::api::AppState;
use crate::crypto::{self, MemberId};
use crate::overlay::displays::{self, Display};
use crate::protocol::live::LiveBody;
use crate::votes::{Tally, VoteKind};
use vote::VoteDto;

const CHECK: Duration = Duration::from_secs(1);
/// 小窗口藏起来这么久没再用就销毁（文件头第 1 条，所有者定的 2 分钟）。
const DESTROY_AFTER: Duration = Duration::from_secs(120);
const BALLOT_REPEAT: Duration = Duration::from_secs(3);
/// 结果出来后投票窗口还留多久。
const RESULT_SHOW: Duration = Duration::from_secs(3);
/// 窗口大小（CSS 像素）。
const ROSTER_SIZE: (f64, f64) = (720.0, 470.0);
const VOTE_SIZE: (f64, f64) = (260.0, 136.0);
const READY_SIZE: (f64, f64) = (400.0, 230.0);
/// 聊天框的大小，和离显示器左边、下边的距离（CSS 像素）。
const CHAT_SIZE: (f64, f64) = (420.0, 520.0);
const CHAT_MARGIN: (f64, f64) = (24.0, 140.0);

pub enum PanelMsg {
    /// 面板键按下或松开。
    Show(bool),
    /// 文字快捷键：打开输入框。
    TextRequested,
    TextSubmit(String, oneshot::Sender<&'static str>),
    TextCancel,
    VoteStart {
        kind: VoteKind,
        target: Option<MemberId>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Answer(bool),
    RemoteVote {
        from: MemberId,
        id: u32,
        kind: VoteKind,
        target: Option<MemberId>,
    },
    RemoteBallot {
        from: MemberId,
        id: u32,
        yes: bool,
    },
    /// 队员名字；不在名单里的投票人算走了。
    Names(Vec<(MemberId, String)>),
    /// 队员退出程序或离开队伍。
    Gone(MemberId),
    /// 进了队伍或不在队伍里了。
    InTeam(bool),
    /// 正在说话的队员（队伍面板里亮起）。
    Speaking(Vec<MemberId>),
}

#[derive(Clone)]
pub struct Panel {
    tx: Sender<PanelMsg>,
    vote: Arc<Mutex<Option<VoteDto>>>,
}

impl Panel {
    pub fn start(app: &AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();
        let vote = Arc::new(Mutex::new(None));
        let runner = Runner {
            app: app.clone(),
            shared: vote.clone(),
            tally: Tally::default(),
            me: None,
            names: HashMap::new(),
            in_team: false,
            result_until: None,
            last_ballot: Instant::now(),
            text_at: None,
            roster: None,
            speaking: Vec::new(),
            vote: None,
            text: None,
            hidden: HashMap::new(),
        };
        std::thread::Builder::new()
            .name("panel".into())
            .spawn(move || runner.run(rx))
            .expect("能开线程");
        Self { tx, vote }
    }

    pub fn send(&self, m: PanelMsg) {
        let _ = self.tx.send(m);
    }
}

struct Runner {
    app: AppHandle,
    shared: Arc<Mutex<Option<VoteDto>>>,
    tally: Tally,
    me: Option<MemberId>,
    names: HashMap<MemberId, String>,
    in_team: bool,
    /// 结果出来后投票窗口显示到这个时候。
    result_until: Option<Instant>,
    last_ballot: Instant,
    /// 按文字快捷键时鼠标在第几号显示器、0–65535 的比例位置。
    text_at: Option<(u8, u16, u16)>,
    roster: Option<WebviewWindow>,
    /// 正在说话的队员（公钥的十六进制），队伍面板里亮起。
    speaking: Vec<String>,
    vote: Option<WebviewWindow>,
    text: Option<WebviewWindow>,
    /// 藏起来的小窗口和藏起来的时候。
    hidden: HashMap<Which, Instant>,
}

impl Runner {
    fn run(mut self, rx: Receiver<PanelMsg>) {
        let mut next_check = Instant::now() + CHECK;
        loop {
            let voting = self.tally.view().is_some() || self.result_until.is_some();
            // 下一次要醒：有投票时每秒一步，有藏着的窗口时到它该销毁的时候；都没有就只等消息
            let destroy = self.hidden.values().min().map(|t| *t + DESTROY_AFTER);
            let wake = [voting.then_some(next_check), destroy]
                .into_iter()
                .flatten()
                .min();
            let got = match wake {
                Some(t) => rx.recv_timeout(t.saturating_duration_since(Instant::now())),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match got {
                Ok(m) => self.handle(m),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.destroy_unused();
            if !voting {
                // 刚开了投票：从现在起每秒走一步
                next_check = Instant::now() + CHECK;
                continue;
            }
            if Instant::now() >= next_check {
                next_check = Instant::now() + CHECK;
                self.tick();
            }
        }
    }

    fn state(&self) -> Option<State<'_, AppState>> {
        self.app.try_state::<AppState>()
    }

    fn me(&mut self) -> Option<MemberId> {
        if self.me.is_none() {
            let node = self.state()?.node.clone();
            self.me =
                tauri::async_runtime::block_on(node.call(|core, _| core.identity().member_id));
        }
        self.me
    }

    fn settings(&self) -> Option<crate::settings2::Settings> {
        Some(
            self.state()?
                .settings
                .lock()
                .expect("设置锁没有中毒")
                .clone(),
        )
    }

    fn handle(&mut self, m: PanelMsg) {
        match m {
            PanelMsg::Show(on) => self.show_roster(on),
            PanelMsg::TextRequested => {
                if self.text_at.is_some() {
                    self.close_text();
                } else {
                    self.open_text();
                }
            }
            PanelMsg::TextSubmit(raw, reply) => {
                let _ = reply.send(self.submit_text(&raw));
            }
            PanelMsg::TextCancel => self.close_text(),
            PanelMsg::VoteStart {
                kind,
                target,
                reply,
            } => {
                let _ = reply.send(self.start_vote(kind, target));
            }
            PanelMsg::Answer(yes) => self.answer(yes),
            PanelMsg::RemoteVote {
                from,
                id,
                kind,
                target,
            } => self.remote_vote(from, id, kind, target),
            PanelMsg::RemoteBallot { from, id, yes } => {
                if self.tally.ballot(from, id, yes) {
                    self.settle();
                    self.push();
                }
            }
            PanelMsg::Names(list) => {
                self.names = list.into_iter().collect();
                let gone: Vec<MemberId> = self
                    .tally
                    .view()
                    .map(|o| {
                        o.voters
                            .iter()
                            .filter(|v| !self.names.contains_key(*v))
                            .copied()
                            .collect()
                    })
                    .unwrap_or_default();
                for v in &gone {
                    self.tally.left(v);
                }
                if !gone.is_empty() {
                    self.settle();
                    self.push();
                }
            }
            PanelMsg::Gone(m) => {
                self.tally.left(&m);
                self.settle();
                self.push();
            }
            PanelMsg::Speaking(list) => {
                self.speaking = list.iter().map(|m| crypto::hex(m)).collect();
                if let Some(w) = &self.roster {
                    let _ = w.emit_to(w.label(), "roster:speaking", &self.speaking);
                }
            }
            PanelMsg::InTeam(on) => {
                self.in_team = on;
                if !on {
                    self.tally.clear();
                    self.result_until = None;
                    self.publish(None);
                    self.text_at = None;
                    for which in [Which::Roster, Which::Vote, Which::Text] {
                        self.hide(which);
                    }
                }
            }
        }
    }

    fn send_live(&self, body: LiveBody) {
        if let Some(state) = self.state() {
            state.node.send_live(body);
        }
    }

    fn name(&self, m: &MemberId) -> String {
        if Some(*m) == self.me {
            return "我".into();
        }
        self.names.get(m).cloned().unwrap_or_else(|| "队友".into())
    }

    fn window(&mut self, which: Which) -> Option<WebviewWindow> {
        let slot = match which {
            Which::Roster => &mut self.roster,
            Which::Vote => &mut self.vote,
            Which::Text => &mut self.text,
        };
        self.hidden.remove(&which);
        if let Some(w) = slot {
            return Some(w.clone());
        }
        let (label, page) = match which {
            Which::Roster => ("roster", "roster.html"),
            Which::Vote => ("vote", "vote.html"),
            Which::Text => ("text", "text.html"),
        };
        // 面板和投票：能点、不拿焦点；输入框要打字，得拿焦点。
        let built = crate::overlay::floating_window(
            &self.app,
            label,
            page,
            "lolPing",
            which == Which::Text,
        );
        match built {
            Ok(w) => {
                *slot = Some(w.clone());
                Some(w)
            }
            Err(e) => {
                eprintln!("{label} 窗口建不起来：{e}");
                None
            }
        }
    }

    /// 藏起一个小窗口，记下时候，2 分钟没再用就销毁（文件头第 1 条）。
    fn hide(&mut self, which: Which) {
        let slot = match which {
            Which::Roster => &self.roster,
            Which::Vote => &self.vote,
            Which::Text => &self.text,
        };
        if let Some(w) = slot {
            let _ = w.hide();
            self.hidden.entry(which).or_insert_with(Instant::now);
        }
    }

    /// 藏了 2 分钟还没再用的小窗口销毁掉，网页进程随之退出；下次用时 `window` 再建。
    fn destroy_unused(&mut self) {
        let due: Vec<Which> = self
            .hidden
            .iter()
            .filter(|(_, t)| t.elapsed() >= DESTROY_AFTER)
            .map(|(w, _)| *w)
            .collect();
        for which in due {
            self.hidden.remove(&which);
            let slot = match which {
                Which::Roster => &mut self.roster,
                Which::Vote => &mut self.vote,
                Which::Text => &mut self.text,
            };
            if let Some(w) = slot.take() {
                let _ = w.destroy();
            }
        }
    }

    fn show_roster(&mut self, on: bool) {
        if !on {
            self.hide(Which::Roster);
            return;
        }
        if !self.in_team || !self.settings().is_some_and(|s| s.team_panel) {
            return;
        }
        let Some(d) = cursor_display(&self.app) else {
            return;
        };
        let Some(w) = self.window(Which::Roster) else {
            return;
        };
        place(&w, centred(&d, ROSTER_SIZE));
        reveal(&w);
        let dto = self.shared.lock().expect("投票锁没有中毒").clone();
        let _ = w.emit_to(w.label(), "roster:show", &dto);
        let _ = w.emit_to(w.label(), "roster:speaking", &self.speaking);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Which {
    Roster,
    Vote,
    Text,
}

fn physical(d: &Display, (w, h): (f64, f64)) -> (i32, i32) {
    ((w * d.scale).round() as i32, (h * d.scale).round() as i32)
}

fn centred(d: &Display, size: (f64, f64)) -> (i32, i32, i32, i32) {
    let (w, h) = physical(d, size);
    (
        d.rect.x + (d.rect.width - w) / 2,
        d.rect.y + (d.rect.height - h) / 2,
        w,
        h,
    )
}

fn cursor_display(app: &AppHandle) -> Option<Display> {
    let list = crate::overlay::current_displays(app)?;
    let (x, y) = crate::input::cursor().unwrap_or((0, 0));
    let i = displays::display_at(&list, x, y).unwrap_or(0);
    list.get(i).cloned()
}

fn place(w: &WebviewWindow, (x, y, width, height): (i32, i32, i32, i32)) {
    let _ = w.set_position(PhysicalPosition::new(x, y));
    let _ = w.set_size(PhysicalSize::new(width.max(1) as u32, height.max(1) as u32));
}

/// 显示并重新放到所有置顶窗口最上面（Windows 偶尔会把置顶窗口压下去；透明窗口也是置顶的）。
fn reveal(w: &WebviewWindow) {
    let _ = w.show();
    let _ = w.set_always_on_top(false);
    let _ = w.set_always_on_top(true);
}

// ---------- 命令 ----------

fn panel(app: &AppHandle) -> Result<Panel, String> {
    app.try_state::<AppState>()
        .map(|s| s.panel.clone())
        .ok_or_else(|| "程序还没启动好".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_centred_and_scaled() {
        let d = Display {
            key: "a".into(),
            primary: true,
            rect: displays::Rect {
                x: 100,
                y: 0,
                width: 2560,
                height: 1600,
            },
            scale: 1.5,
        };
        assert_eq!(physical(&d, ROSTER_SIZE), (1080, 705));
        assert_eq!(centred(&d, ROSTER_SIZE), (100 + 740, 447, 1080, 705));
    }
}
