//! Windows 的全局键鼠监听（架构 2.3）。移植自 lolPing `native/hook-helper/src/hooks_win.cpp`（MIT，© 2026 Danrui Meng）。
//!
//! 实现流程：
//! 1. 一个专门的线程装 `WH_KEYBOARD_LL`，跑自己的消息循环；`WH_MOUSE_LL` 只在要看鼠标时才装（第 8 条）。系统在这个线程上调钩子回调。
//! 2. 回调里只做纯计算：把事件交给手势状态机，按结果拦下（返回 1）或放行（交给下一个钩子），
//!    要画的东西通过不阻塞的通道交出去。回调慢了 Windows 会悄悄摘掉钩子，所以这里不做 IO、不等锁。
//! 3. 回放（`SendInput`）不能在回调里做：注入的事件要再过一遍钩子链，而处理钩子的正是这个卡在 `SendInput` 里的线程，
//!    全系统的输入都会等到钩子超时、鼠标卡住。所以回放先排队，回调返回后由消息循环发出去。
//!    回放的事件在 `dwExtraInfo` 里带标记，回调认出来就直接放行。
//! 4. 排队的回放还没发出去时来了真实的按键或点击：如果直接放行，它会抢在回放前面（比如补发的按下落在真实的抬起之后，
//!    按键就卡在按下状态）。这时把它也拦下，排在队尾回放。
//! 5. 每秒一次：用 `GetAsyncKeyState` 跟系统对按键状态，再让状态机检查看门狗。3.2.0 起这个计时器只在要的时候开
//!    （`needs_tick`：有按着的键、手势没做完），和鼠标钩子一样在消息循环里开关。
//! 6. 设置和开关通过通道送进来，`PostThreadMessageW` 叫醒消息循环处理。
//! 7. 进程退出或崩溃时钩子随进程消失，键鼠不会被卡住。
//! 8. 鼠标钩子按需装（3.2.0）：低级鼠标钩子让全系统每一次鼠标移动都经过这个线程（量过约 44 微秒一次，游戏鼠标每秒上千次），
//!    没按触发键时一次也用不着。手势状态机说要（`wants_mouse`：按住了触发键、手势还没做完，或者侧键当触发键）才装，不要了就摘。
//!    回调里只投一条消息，装和摘都在消息循环里做。启动时两个钩子先都装一次，确认装得上（装不上照旧报错），再按需要摘掉鼠标钩子。
//!    离开电脑看 `GetLastInputInfo`（系统记的最后一次输入），鼠标位置用 `GetCursorPos`，都不靠钩子。
//!
//! `unsafe` 只在这个文件里：都是 Win32 调用和钩子回调里对系统给的结构体指针的读取，每处写了为什么成立。
//! 整个 crate 禁止 `unsafe`（Cargo.toml 的 `[lints.rust]`），只有这里和 `macos.rs` 放开（3.2.0）。
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    EqualRect, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    MonitorFromWindow,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::{GetTickCount, GetTickCount64};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetLastInputInfo, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE,
    KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, LASTINPUTINFO,
    MAPVK_VK_TO_VSC, MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GWL_STYLE, GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, KillTimer, LLKHF_INJECTED,
    LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetTimer,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SYSKEYDOWN, WM_TIMER, WM_USER, WM_XBUTTONDOWN, WM_XBUTTONUP, WS_CAPTION,
};

use super::Command;
use super::gesture::{Btn, EvKind, Gesture, Inject, InputEvent, Outcome};

/// 回放事件的标记（"LGNG"），放在 `dwExtraInfo` 里。
const SELF_MARKER: usize = 0x4C47_4E47;
const WM_COMMAND_READY: u32 = WM_APP + 1;
const WM_INJECT: u32 = WM_APP + 2;
/// 手势要不要鼠标变了：去装或摘鼠标钩子。
const WM_SYNC_MOUSE: u32 = WM_APP + 3;
const XBUTTON1: u16 = 1;
const XBUTTON2: u16 = 2;

/// 钩子线程自己的状态。回调和消息循环都在这个线程上，所以用线程局部变量，不需要锁。
struct HookState {
    gesture: Gesture,
    /// 要画的东西交给它（只往通道里放，不阻塞）。
    sink: super::Sink,
    /// 等回调返回后再发的回放。
    pending: Vec<Inject>,
    thread_id: u32,
    /// 装着的鼠标钩子；没装是 None（第 8 条）。
    mouse_hook: Option<HHOOK>,
    /// 每秒一次的计时器（第 5 条）；没开是 0。
    timer: usize,
}

thread_local! {
    static STATE: RefCell<Option<HookState>> = const { RefCell::new(None) };
}

/// 输入线程的把手：发命令进去。
pub struct Handle {
    thread_id: u32,
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, c: Command) {
        if self.tx.send(c).is_ok() {
            // SAFETY: 只是给钩子线程的消息队列投一条消息，参数是普通整数；线程已退出时调用失败，忽略即可。
            let _ = unsafe {
                PostThreadMessageW(self.thread_id, WM_COMMAND_READY, WPARAM(0), LPARAM(0))
            };
        }
    }
}

/// 启动钩子线程，等钩子装好再返回。装不上（比如被安全软件拦）就返回错误说明。
pub fn start(cfg: super::gesture::Config, sink: super::Sink) -> Result<Handle, String> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
    std::thread::Builder::new()
        .name("input-hooks".into())
        .spawn(move || run(cfg, sink, cmd_rx, ready_tx))
        .map_err(|e| e.to_string())?;
    let thread_id = ready_rx.recv().map_err(|e| e.to_string())??;
    Ok(Handle {
        thread_id,
        tx: cmd_tx,
    })
}

fn run(
    cfg: super::gesture::Config,
    sink: super::Sink,
    commands: Receiver<Command>,
    ready: Sender<Result<u32, String>>,
) {
    let mut gesture = Gesture::new();
    gesture.set_config(cfg);
    gesture.set_fullscreen_probe(Box::new(click_is_on_fullscreen_app));
    // SAFETY: 取当前线程的 ID，没有前置条件。
    let thread_id = unsafe { GetCurrentThreadId() };
    STATE.with(|s| {
        *s.borrow_mut() = Some(HookState {
            gesture,
            sink,
            pending: Vec::new(),
            thread_id,
            mouse_hook: None,
            timer: 0,
        })
    });

    let mut msg = MSG::default();
    // SAFETY: msg 是本线程栈上的有效结构体。这一次 Peek 只是让系统给本线程建好消息队列，之后别的线程才能投消息进来。
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
    }
    // SAFETY: 回调是本文件里签名匹配的 extern "system" 函数，进程活着就一直有效；
    // 模块句柄取的是本进程的主模块；dwThreadId 为 0 表示全局低级钩子，由系统在本线程上调回调。
    let hooks = unsafe {
        let module = GetModuleHandleW(None).ok();
        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module.map(Into::into), 0);
        let key = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_proc), module.map(Into::into), 0);
        match (mouse, key) {
            (Ok(m), Ok(k)) => Ok((m, k)),
            (m, k) => {
                for h in [m.ok(), k.ok()].into_iter().flatten() {
                    let _ = UnhookWindowsHookEx(h);
                }
                Err(crate::tr!(
                    "装不上键鼠钩子（SetWindowsHookEx 失败）",
                    "Couldn't install the keyboard and mouse hooks (SetWindowsHookEx failed)"
                )
                .to_string())
            }
        }
    };
    let (mouse_hook, key_hook): (HHOOK, HHOOK) = match hooks {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    with_state(|s| s.mouse_hook = Some(mouse_hook));
    sync_mouse_hook();
    let _ = ready.send(Ok(thread_id));

    // SAFETY: msg 是有效的本地结构体；GetMessageW 返回 0（WM_QUIT）或 -1（出错）时退出循环。
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        match msg.message {
            WM_TIMER => {
                with_state(|s| {
                    s.gesture.sync_key_states(is_key_down);
                    let r = s.gesture.tick(now_ms());
                    deliver(s, r);
                });
            }
            WM_INJECT => drain_injects(),
            WM_SYNC_MOUSE => {}
            WM_COMMAND_READY => {
                while let Ok(c) = commands.try_recv() {
                    with_state(|s| {
                        let r = match c {
                            Command::Config(cfg) => s.gesture.set_config(cfg),
                            Command::Enabled(on) => s.gesture.set_enabled(on),
                            Command::Suspended(on) => s.gesture.set_suspended(on),
                        };
                        deliver(s, r);
                    });
                }
                drain_injects();
            }
            _ => {}
        }
        sync_mouse_hook();
    }
    // SAFETY: 两个钩子都是在本线程装上的有效句柄（鼠标钩子此刻可能已经摘了）。
    unsafe {
        if let Some(h) = with_state(|s| s.mouse_hook.take()).flatten() {
            let _ = UnhookWindowsHookEx(h);
        }
        let _ = UnhookWindowsHookEx(key_hook);
    }
}

/// 按手势现在的需要装上或摘掉鼠标钩子（第 8 条）、开关每秒的计时器（第 5 条）。只在消息循环里调用，不在钩子回调里。
fn sync_mouse_hook() {
    let Some((want, have, tick, timer)) = with_state(|s| {
        (
            s.gesture.wants_mouse(),
            s.mouse_hook,
            s.gesture.needs_tick(),
            s.timer,
        )
    }) else {
        return;
    };
    if tick && timer == 0 {
        // SAFETY: 不带窗口的线程计时器，每秒往本线程队列投一条 WM_TIMER，没有回调函数。
        let id = unsafe { SetTimer(None, 0, 1000, None) };
        with_state(|s| s.timer = id);
    } else if !tick && timer != 0 {
        // SAFETY: timer 是上面在本线程开的计时器；关掉以后马上清零，不再用它。
        let _ = unsafe { KillTimer(None, timer) };
        with_state(|s| s.timer = 0);
    }
    match (want, have) {
        (true, None) => {
            // SAFETY: 同启动时：回调是本文件里签名匹配的 extern "system" 函数，进程活着就一直有效；
            // 模块句柄取本进程的主模块；dwThreadId 为 0 表示全局低级钩子，由系统在本线程上调回调。
            let hook = unsafe {
                let module = GetModuleHandleW(None).ok();
                SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module.map(Into::into), 0)
            };
            match hook {
                Ok(h) => {
                    with_state(|s| s.mouse_hook = Some(h));
                }
                Err(e) => eprintln!("鼠标钩子装不上：{e}"),
            }
        }
        (false, Some(h)) => {
            // SAFETY: h 是本线程装上的有效钩子句柄；摘掉以后马上从状态里清掉，不再用它。
            let _ = unsafe { UnhookWindowsHookEx(h) };
            with_state(|s| s.mouse_hook = None);
        }
        _ => {}
    }
}

/// 回调里用：手势要不要鼠标、要不要计时器，和现在装没装、开没开对不上了，就叫醒消息循环去改。
fn request_mouse_sync() {
    let stale = with_state(|s| {
        (s.gesture.wants_mouse() != s.mouse_hook.is_some()
            || s.gesture.needs_tick() != (s.timer != 0))
            .then_some(s.thread_id)
    });
    if let Some(Some(id)) = stale {
        // SAFETY: 给自己的线程队列投一条消息，参数是普通整数。
        let _ = unsafe { PostThreadMessageW(id, WM_SYNC_MOUSE, WPARAM(0), LPARAM(0)) };
    }
}

/// 鼠标现在的位置（物理像素）。鼠标钩子不再一直装着，位置直接问系统。
pub fn cursor() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    // SAFETY: p 是本地的有效结构体，GetCursorPos 只往里写。
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some((p.x, p.y))
}

/// 多久没碰键鼠了：系统记的最后一次输入（整个登录会话的键鼠都算），不需要钩子。
pub fn idle_ms() -> Option<u64> {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: info 是本地的有效结构体，cbSize 按要求填好；GetLastInputInfo 只往里写。
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return None;
    }
    // SAFETY: 读开机以来的毫秒数，没有前置条件。它和 dwTime 是同一个 32 位时钟（约 49.7 天回绕一次），所以用回绕减法。
    let now = unsafe { GetTickCount() };
    Some(u64::from(now.wrapping_sub(info.dwTime)))
}

/// 在钩子线程上用状态。拿不到（只可能是重入）就什么都不做：回调里绝不能 panic。
fn with_state<T>(f: impl FnOnce(&mut HookState) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn now_ms() -> u64 {
    // SAFETY: 读系统开机以来的毫秒数，没有前置条件。
    unsafe { GetTickCount64() }
}

fn is_key_down(vk: u32) -> bool {
    // SAFETY: 读一个虚拟键码的当前状态，任何整数都是合法参数。
    (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0
}

/// 把要画的交出去，把回放排进队，叫醒消息循环来发。
fn deliver(s: &mut HookState, r: Outcome) {
    for e in r.emits {
        (s.sink)(e);
    }
    queue_injects(s, r.injects);
}

fn queue_injects(s: &mut HookState, injects: Vec<Inject>) {
    if injects.is_empty() {
        return;
    }
    s.pending.extend(injects);
    // SAFETY: 给自己的线程队列投一条消息，参数是普通整数。
    let _ = unsafe { PostThreadMessageW(s.thread_id, WM_INJECT, WPARAM(0), LPARAM(0)) };
}

fn drain_injects() {
    // 先把队列拿出来再发：发的时候不能占着状态（注入的事件会再进回调）。
    let batch = with_state(|s| std::mem::take(&mut s.pending)).unwrap_or_default();
    send_injects(&batch);
}

/// 处理一个真实的按键或点击，返回是否拦下。见文件头第 4 条：队里还有回放时，放行的事件改成拦下再排队回放。
fn apply_in_order(e: &InputEvent, replay: Inject) -> bool {
    with_state(|s| {
        let behind = !e.self_injected && !s.pending.is_empty();
        let r = s.gesture.on_event(e);
        let swallow = r.swallow;
        deliver(s, r);
        if swallow {
            return true;
        }
        if !behind {
            return false;
        }
        queue_injects(s, vec![replay]);
        true
    })
    .unwrap_or(false)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: WH_MOUSE_LL 在 HC_ACTION 时，lParam 指向系统提供的 MSLLHOOKSTRUCT，回调期间有效、只读。
        let m = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let injected = m.flags & LLMHF_INJECTED != 0 && m.dwExtraInfo == SELF_MARKER;
        let side = if (m.mouseData >> 16) as u16 == XBUTTON1 {
            Btn::X1
        } else {
            Btn::X2
        };
        let event = match wparam.0 as u32 {
            WM_MOUSEMOVE => Some((EvKind::MouseMove, Btn::None)),
            WM_LBUTTONDOWN => Some((EvKind::MouseDown, Btn::Left)),
            WM_LBUTTONUP => Some((EvKind::MouseUp, Btn::Left)),
            WM_RBUTTONDOWN => Some((EvKind::MouseDown, Btn::Right)),
            WM_RBUTTONUP => Some((EvKind::MouseUp, Btn::Right)),
            WM_MBUTTONDOWN => Some((EvKind::MouseDown, Btn::Middle)),
            WM_MBUTTONUP => Some((EvKind::MouseUp, Btn::Middle)),
            WM_XBUTTONDOWN => Some((EvKind::MouseDown, side)),
            WM_XBUTTONUP => Some((EvKind::MouseUp, side)),
            _ => None, // 滚轮等：不看
        };
        if let Some((kind, btn)) = event {
            let mut e = InputEvent::mouse(kind, btn, m.pt.x, m.pt.y, now_ms());
            e.self_injected = injected;
            let swallow = match kind {
                // 移动可以抢在排队的回放前面：补发的按下只是落在更新的光标位置。
                EvKind::MouseMove => with_state(|s| {
                    let r = s.gesture.on_event(&e);
                    let swallow = r.swallow;
                    deliver(s, r);
                    swallow
                })
                .unwrap_or(false),
                _ => {
                    if kind == EvKind::MouseDown {
                        with_state(|s| s.gesture.sync_key_states(is_key_down));
                    }
                    let replay = if kind == EvKind::MouseDown {
                        Inject::ButtonDown(btn)
                    } else {
                        Inject::ButtonUp(btn)
                    };
                    apply_in_order(&e, replay)
                }
            };
            request_mouse_sync();
            if swallow {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: 原样交给下一个钩子，参数是系统给的。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn key_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: WH_KEYBOARD_LL 在 HC_ACTION 时，lParam 指向系统提供的 KBDLLHOOKSTRUCT，回调期间有效、只读。
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let mut e = InputEvent::key(
            if down { EvKind::KeyDown } else { EvKind::KeyUp },
            k.vkCode,
            now_ms(),
        );
        e.self_injected =
            k.flags & LLKHF_INJECTED != Default::default() && k.dwExtraInfo == SELF_MARKER;
        let replay = if down {
            Inject::KeyDown(k.vkCode)
        } else {
            Inject::KeyUp(k.vkCode)
        };
        let swallow = apply_in_order(&e, replay);
        request_mouse_sync();
        if swallow {
            return LRESULT(1);
        }
    }
    // SAFETY: 原样交给下一个钩子，参数是系统给的。
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 需要带「扩展键」标志才能被正确识别的键（右 Alt、右 Ctrl、Win、方向键和编辑键区）。
fn is_extended_vk(vk: u32) -> bool {
    matches!(
        vk,
        0xA5 | 0xA3 | 0x5B | 0x5C | 0x2D | 0x2E | 0x24 | 0x23 | 0x21 | 0x22 | 0x25..=0x28
    )
}

fn send_injects(injects: &[Inject]) {
    let inputs: Vec<INPUT> = injects.iter().filter_map(|&i| to_input(i)).collect();
    if inputs.is_empty() {
        return;
    }
    // SAFETY: inputs 是本地构造的有效 INPUT 数组，cbsize 是 INPUT 的真实大小。这里在消息循环里调用，不在钩子回调里。
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn to_input(i: Inject) -> Option<INPUT> {
    let mouse = |flags: MOUSE_EVENT_FLAGS, data: u16| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                mouseData: u32::from(data),
                dwFlags: flags,
                dwExtraInfo: SELF_MARKER,
                ..Default::default()
            },
        },
    };
    let key = |vk: u32, up: bool| {
        let mut flags = KEYBD_EVENT_FLAGS(0);
        if up {
            flags |= KEYEVENTF_KEYUP;
        }
        if is_extended_vk(vk) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        // SAFETY: 查虚拟键码对应的扫描码，任何整数都是合法参数。游戏读的是扫描码。
        let scan = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC) } as u16;
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk as u16),
                    wScan: scan,
                    dwFlags: flags,
                    dwExtraInfo: SELF_MARKER,
                    ..Default::default()
                },
            },
        }
    };
    Some(match i {
        Inject::ButtonDown(b) | Inject::ButtonUp(b) => {
            let down = matches!(i, Inject::ButtonDown(_));
            let (flags, data) = match b {
                Btn::Left => (
                    if down {
                        MOUSEEVENTF_LEFTDOWN
                    } else {
                        MOUSEEVENTF_LEFTUP
                    },
                    0,
                ),
                Btn::Right => (
                    if down {
                        MOUSEEVENTF_RIGHTDOWN
                    } else {
                        MOUSEEVENTF_RIGHTUP
                    },
                    0,
                ),
                Btn::Middle => (
                    if down {
                        MOUSEEVENTF_MIDDLEDOWN
                    } else {
                        MOUSEEVENTF_MIDDLEUP
                    },
                    0,
                ),
                Btn::X1 => (
                    if down {
                        MOUSEEVENTF_XDOWN
                    } else {
                        MOUSEEVENTF_XUP
                    },
                    XBUTTON1,
                ),
                Btn::X2 => (
                    if down {
                        MOUSEEVENTF_XDOWN
                    } else {
                        MOUSEEVENTF_XUP
                    },
                    XBUTTON2,
                ),
                Btn::None => return None,
            };
            mouse(flags, data)
        }
        Inject::KeyDown(vk) => key(vk, false),
        Inject::KeyUp(vk) => key(vk, true),
    })
}

/// (x, y) 所在的显示器是不是被前台窗口整个盖住：游戏或其他全屏程序占着这块屏幕，触发键加点击归它，不开轮盘。
/// 别的显示器上照样能开。「盖住」指窗口矩形包含显示器矩形：无边框全屏正好相等，CS2 的全屏是带标题栏、超出屏幕的最大化窗口。
/// 普通最大化窗口只到工作区为止；但显示器没有任务栏（自动隐藏，或这块屏幕没有）时工作区就是整块屏幕，
/// 这时只认没有标题栏的窗口，所以 CS2 那种会漏判。桌面和任务栏不算。不跨进程调用，在钩子回调里调用是安全的。
fn click_is_on_fullscreen_app(x: i32, y: i32) -> bool {
    // SAFETY: 都是只读的查询；句柄来自系统，查询失败时按「不是全屏」处理；结构体都是本地变量。
    unsafe {
        let fg: HWND = GetForegroundWindow();
        if fg.is_invalid() {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(fg, &mut cls);
        if n > 0 {
            let name = String::from_utf16_lossy(&cls[..n as usize]);
            if ["Progman", "WorkerW", "Shell_TrayWnd"]
                .iter()
                .any(|c| name.eq_ignore_ascii_case(c))
            {
                return false;
            }
        }
        let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        if mon.is_invalid() || mon != MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) {
            return false;
        }
        let mut wr = RECT::default();
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetWindowRect(fg, &mut wr).is_err() || !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        if wr.left > m.left || wr.top > m.top || wr.right < m.right || wr.bottom < m.bottom {
            return false;
        }
        if EqualRect(&mi.rcWork, &m).as_bool() {
            return (GetWindowLongPtrW(fg, GWL_STYLE) as u32 & WS_CAPTION.0) != WS_CAPTION.0;
        }
        true
    }
}
