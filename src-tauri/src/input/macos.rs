//! macOS 的全局键鼠监听（架构 2.3，实验版，未在真机上测试）。移植自 lolPing `native/hook-helper/src/hooks_mac.cpp`
//! （MIT，© 2026 Danrui Meng）。要在「系统设置 → 隐私与安全性 → 辅助功能」里允许钢一下。
//!
//! 实现流程：
//! 1. 一个专门的线程建 CGEventTap（会话级、插在最前面、能拦截），挂到自己的 RunLoop 上。系统在这个线程上调回调。
//! 2. 回调里把事件交给手势状态机：返回空指针就是拦下，原样返回就是放行。回放用 CGEventPost 发到 HID 层，
//!    事件的 user data 字段带标记，回调认出来直接放行（macOS 的回放是异步进 tap 的，可以在回调里发）。
//! 3. 修饰键没有单独的按键事件，只有「修饰键状态变了」：按新旧状态算出松开和按下（`keymap`），当按键交给状态机。
//!    鼠标事件也带着修饰键状态，密码框打开安全输入、键盘事件进不来时，状态也不会错。
//! 4. 系统给的位置是「点」：乘上那块显示器的缩放，换成和 Tauri 的显示器矩形一样的物理像素。
//! 5. RunLoop 每 50 毫秒醒一次，处理送进来的命令；每秒一次和系统对按键状态，再让状态机检查看门狗。
//! 6. tap 因为回调太慢或者用户输入被系统关掉时，马上重新打开，再对一次状态、收起面板（漏掉了按键事件）。
//! 7. 没有全屏检查（Windows 才有），`game_check` 在这里不起作用；面板键的连发看门狗也关掉。
//!
//! `unsafe` 只在这个文件里：都是 CoreGraphics、CoreFoundation 的调用和回调参数的使用，每处写了为什么成立。
//! 整个 crate 禁止 `unsafe`（Cargo.toml 的 `[lints.rust]`），只有这里和 `win32.rs` 放开（3.2.0）。
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use super::Command;
use super::gesture::{Btn, EvKind, Gesture, Inject, InputEvent, Outcome};
use super::keymap::{
    MODIFIER_FLAGS, is_modifier_vk, mac_key_to_vk, mac_modifier_held, mac_modifier_transitions,
    vk_to_mac_key,
};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGDisplayModeRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

type TapCallback =
    unsafe extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventCreate(source: *mut c_void) -> CGEventRef;
    fn CGEventCreateMouseEvent(
        source: *mut c_void,
        mouse_type: u32,
        position: CGPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventCreateKeyboardEvent(source: *mut c_void, keycode: u16, down: bool) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGEventSourceFlagsState(state: i32) -> u64;
    fn CGEventSourceKeyState(state: i32, key: u16) -> bool;
    fn CGGetDisplaysWithPoint(
        point: CGPoint,
        max_displays: u32,
        displays: *mut u32,
        count: *mut u32,
    ) -> i32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayCopyDisplayMode(display: u32) -> CGDisplayModeRef;
    fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source: bool) -> i32;
    fn CFRelease(cf: CFTypeRef);
}

/// 回放事件的标记（"LGNG"），放在 kCGEventSourceUserData 里。
const SELF_MARKER: i64 = 0x4C47_4E47;
const SESSION_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const TAP_DEFAULT: u32 = 0;
const HID_TAP: u32 = 0;
const COMBINED_STATE: i32 = 0;

const LEFT_DOWN: u32 = 1;
const LEFT_UP: u32 = 2;
const RIGHT_DOWN: u32 = 3;
const RIGHT_UP: u32 = 4;
const MOVED: u32 = 5;
const LEFT_DRAGGED: u32 = 6;
const RIGHT_DRAGGED: u32 = 7;
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const OTHER_DOWN: u32 = 25;
const OTHER_UP: u32 = 26;
const OTHER_DRAGGED: u32 = 27;
const DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const DISABLED_BY_USER: u32 = 0xFFFF_FFFF;

const FIELD_CLICK_STATE: u32 = 1;
const FIELD_BUTTON_NUMBER: u32 = 3;
const FIELD_KEYCODE: u32 = 9;
const FIELD_USER_DATA: u32 = 42;

const TICK: Duration = Duration::from_secs(1);

struct TapState {
    gesture: Gesture,
    sink: super::Sink,
    tap: CFMachPortRef,
    /// 状态机最近一次看到的修饰键状态。
    flags: u64,
    /// 每个键按下时的连击次数，回放时带上（双击还是双击）。
    click_states: [i64; 6],
    /// 显示器的缩放（物理像素 / 点），每秒清一次。
    scales: HashMap<u32, f64>,
}

thread_local! {
    static STATE: RefCell<Option<TapState>> = const { RefCell::new(None) };
}

pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

pub fn start(cfg: super::gesture::Config, sink: super::Sink) -> Result<Handle, String> {
    let (tx, rx) = mpsc::channel::<Command>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
    std::thread::Builder::new()
        .name("input-tap".into())
        .spawn(move || run(cfg, sink, rx, ready_tx))
        .map_err(|e| e.to_string())?;
    ready_rx.recv().map_err(|e| e.to_string())??;
    Ok(Handle { tx })
}

fn event_mask() -> u64 {
    [
        KEY_DOWN,
        KEY_UP,
        FLAGS_CHANGED,
        LEFT_DOWN,
        LEFT_UP,
        RIGHT_DOWN,
        RIGHT_UP,
        OTHER_DOWN,
        OTHER_UP,
        MOVED,
        LEFT_DRAGGED,
        RIGHT_DRAGGED,
        OTHER_DRAGGED,
    ]
    .iter()
    .fold(0u64, |m, t| m | (1u64 << t))
}

fn run(
    cfg: super::gesture::Config,
    sink: super::Sink,
    commands: Receiver<Command>,
    ready: Sender<Result<(), String>>,
) {
    let mut gesture = Gesture::new();
    gesture.set_mask_menu_keys(false);
    gesture.set_panel_repeat_gap(0);
    gesture.set_config(cfg);
    // SAFETY: 回调是本文件里签名匹配的 extern "C" 函数，进程活着就一直有效；user_info 不用，传空指针。
    let tap = unsafe {
        CGEventTapCreate(
            SESSION_TAP,
            HEAD_INSERT,
            TAP_DEFAULT,
            event_mask(),
            tap_callback,
            std::ptr::null_mut(),
        )
    };
    if tap.is_null() {
        // SAFETY: 只读查询本进程有没有「辅助功能」权限，没有参数。
        let trusted = unsafe { AXIsProcessTrusted() };
        // 系统说已经允许了、监听还是建不起来：没有开发者签名的程序更新后，系统把它当成新程序，旧的授权不算数。
        let message = if trusted {
            crate::tr!(
                "系统里已经允许了 lolPing，但还是读不到键鼠：多半是更新后系统把它当成了新程序。请在「系统设置 → 隐私与安全性 → 辅助功能」里把 lolPing 删掉再加回来，然后重开",
                "lolPing is already allowed, but keyboard and mouse still can't be read: after an update macOS most likely treats it as a new app. Remove lolPing in System Settings → Privacy & Security → Accessibility, add it back, then restart"
            )
        } else {
            crate::tr!(
                "读不到键鼠：请在「系统设置 → 隐私与安全性 → 辅助功能」里允许 lolPing，然后重开",
                "Can't read keyboard and mouse: allow lolPing in System Settings → Privacy & Security → Accessibility, then restart"
            )
        };
        let _ = ready.send(Err(message.to_string()));
        return;
    }
    STATE.with(|s| {
        *s.borrow_mut() = Some(TapState {
            gesture,
            sink,
            tap,
            flags: 0,
            click_states: [1; 6],
            scales: HashMap::new(),
        })
    });
    // SAFETY: tap 是上面建好的有效端口；RunLoop 是本线程的；kCFRunLoopCommonModes 是系统常量。
    // 建好的 source 加进 RunLoop 后由 RunLoop 持有，本线程一直跑到进程结束，不用释放。
    unsafe {
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
    }
    with_state(resync);
    let _ = ready.send(Ok(()));

    let mut next_tick = Instant::now() + TICK;
    loop {
        // SAFETY: 在本线程的 RunLoop 上跑 50 毫秒（期间系统调 tap 回调）；kCFRunLoopDefaultMode 是系统常量。
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.05, false);
        }
        loop {
            match commands.try_recv() {
                Ok(c) => {
                    with_state(|s| {
                        let r = match c {
                            Command::Config(cfg) => s.gesture.set_config(cfg),
                            Command::Enabled(on) => s.gesture.set_enabled(on),
                            Command::Suspended(on) => s.gesture.set_suspended(on),
                        };
                        apply(s, r);
                    });
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        if Instant::now() >= next_tick {
            next_tick = Instant::now() + TICK;
            with_state(|s| {
                s.scales.clear();
                resync(s);
                let r = s.gesture.tick(now_ms());
                apply(s, r);
            });
        }
    }
}

/// 在 tap 线程上用状态。拿不到（只可能是重入）就什么都不做：回调里绝不能 panic。
fn with_state<T>(f: impl FnOnce(&mut TapState) -> T) -> Option<T> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn is_key_down(vk: u32) -> bool {
    // SAFETY: 读当前会话的修饰键状态和某个键码的状态，参数都是普通整数。
    unsafe {
        if is_modifier_vk(vk) {
            return mac_modifier_held(CGEventSourceFlagsState(COMBINED_STATE), vk);
        }
        vk_to_mac_key(vk).is_some_and(|code| CGEventSourceKeyState(COMBINED_STATE, code))
    }
}

/// 和系统对一次：修饰键状态、按着的键。
fn resync(s: &mut TapState) {
    // SAFETY: 读当前会话的修饰键状态。
    let flags = unsafe { CGEventSourceFlagsState(COMBINED_STATE) };
    feed_flags(s, flags, now_ms());
    s.gesture.sync_key_states(is_key_down);
}

/// 修饰键状态变了：算出松开和按下，当按键交给状态机（修饰键事件一律放行）。
fn feed_flags(s: &mut TapState, flags: u64, t: u64) {
    let flags = flags & MODIFIER_FLAGS;
    if flags == s.flags {
        return;
    }
    for (vk, down) in mac_modifier_transitions(s.flags, flags) {
        let kind = if down { EvKind::KeyDown } else { EvKind::KeyUp };
        let r = s.gesture.on_event(&InputEvent::key(kind, vk, t));
        apply(s, r);
    }
    s.flags = flags;
}

/// 交出要画的、发回放，返回要不要拦下。
fn apply(s: &mut TapState, r: Outcome) -> bool {
    for e in r.emits {
        (s.sink)(e);
    }
    for i in r.injects {
        match i {
            Inject::ButtonDown(b) => post_button(s, b, true),
            Inject::ButtonUp(b) => post_button(s, b, false),
            Inject::KeyDown(vk) => post_key(s.flags, vk, true),
            Inject::KeyUp(vk) => post_key(s.flags, vk, false),
        }
    }
    r.swallow
}

fn cursor_location() -> CGPoint {
    // SAFETY: 建一个空事件只为读当前鼠标位置，读完马上释放；建不出来时用原点。
    unsafe {
        let e = CGEventCreate(std::ptr::null_mut());
        if e.is_null() {
            return CGPoint { x: 0.0, y: 0.0 };
        }
        let p = CGEventGetLocation(e);
        CFRelease(e as CFTypeRef);
        p
    }
}

fn btn_index(b: Btn) -> usize {
    match b {
        Btn::None => 0,
        Btn::Left => 1,
        Btn::Right => 2,
        Btn::Middle => 3,
        Btn::X1 => 4,
        Btn::X2 => 5,
    }
}

fn post_button(s: &TapState, b: Btn, down: bool) {
    let (kind, button) = match b {
        Btn::Left => (if down { LEFT_DOWN } else { LEFT_UP }, 0),
        Btn::Right => (if down { RIGHT_DOWN } else { RIGHT_UP }, 1),
        Btn::Middle => (if down { OTHER_DOWN } else { OTHER_UP }, 2),
        Btn::X1 => (if down { OTHER_DOWN } else { OTHER_UP }, 3),
        Btn::X2 => (if down { OTHER_DOWN } else { OTHER_UP }, 4),
        Btn::None => return,
    };
    // SAFETY: 新建的鼠标事件只在这里用，发出去后释放；建不出来就不发。
    unsafe {
        let e = CGEventCreateMouseEvent(std::ptr::null_mut(), kind, cursor_location(), button);
        if e.is_null() {
            return;
        }
        CGEventSetIntegerValueField(e, FIELD_BUTTON_NUMBER, i64::from(button));
        CGEventSetIntegerValueField(e, FIELD_CLICK_STATE, s.click_states[btn_index(b)]);
        CGEventSetFlags(e, s.flags); // 回放的 Option+单击还带着 Option
        CGEventSetIntegerValueField(e, FIELD_USER_DATA, SELF_MARKER);
        CGEventPost(HID_TAP, e);
        CFRelease(e as CFTypeRef);
    }
}

/// 修饰键不回放：它们真正的状态变化一直放行，回放的键带着当前修饰键状态。
fn post_key(flags: u64, vk: u32, down: bool) {
    if is_modifier_vk(vk) {
        return;
    }
    let Some(code) = vk_to_mac_key(vk) else {
        return;
    };
    // SAFETY: 新建的键盘事件只在这里用，发出去后释放；建不出来就不发。
    unsafe {
        let e = CGEventCreateKeyboardEvent(std::ptr::null_mut(), code, down);
        if e.is_null() {
            return;
        }
        CGEventSetFlags(e, flags); // 回放的 Shift+G 还是大写
        CGEventSetIntegerValueField(e, FIELD_USER_DATA, SELF_MARKER);
        CGEventPost(HID_TAP, e);
        CFRelease(e as CFTypeRef);
    }
}

/// 点 → 物理像素：乘上这个点所在显示器的缩放（物理像素宽 / 点宽）。
fn to_physical(s: &mut TapState, p: CGPoint) -> (i32, i32) {
    let mut id = 0u32;
    let mut count = 0u32;
    // SAFETY: id、count 是本地变量，最多写一个显示器。
    let found = unsafe { CGGetDisplaysWithPoint(p, 1, &mut id, &mut count) } == 0 && count > 0;
    let scale = if found {
        *s.scales.entry(id).or_insert_with(|| {
            // SAFETY: id 是系统刚给的显示器；拿到的显示模式用完马上释放；拿不到时按 1 倍。
            unsafe {
                let bounds = CGDisplayBounds(id);
                let mode = CGDisplayCopyDisplayMode(id);
                if mode.is_null() {
                    return 1.0;
                }
                let pixels = CGDisplayModeGetPixelWidth(mode) as f64;
                CGDisplayModeRelease(mode); // 先放掉再看宽度，宽度不对提前返回也不漏
                if bounds.size.width <= 0.0 {
                    return 1.0;
                }
                (pixels / bounds.size.width).max(1.0)
            }
        })
    } else {
        1.0
    };
    ((p.x * scale).round() as i32, (p.y * scale).round() as i32)
}

unsafe extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    kind: u32,
    event: CGEventRef,
    _user: *mut c_void,
) -> CGEventRef {
    if kind == DISABLED_BY_TIMEOUT || kind == DISABLED_BY_USER {
        with_state(|s| {
            // SAFETY: tap 是本线程建的有效端口。
            unsafe { CGEventTapEnable(s.tap, true) };
            resync(s);
            // tap 关着时漏了按键事件，按着的面板键可能已经松开了：收起面板。
            let r = s.gesture.release_panel();
            apply(s, r);
        });
        return event;
    }
    // SAFETY: 回调期间 event 是系统给的有效事件，只读它的字段。
    let (marker, flags, location) = unsafe {
        (
            CGEventGetIntegerValueField(event, FIELD_USER_DATA),
            CGEventGetFlags(event),
            CGEventGetLocation(event),
        )
    };
    if marker == SELF_MARKER {
        return event;
    }
    let swallow = with_state(|s| {
        let t = now_ms();
        feed_flags(s, flags, t);
        match kind {
            FLAGS_CHANGED => false,
            KEY_DOWN | KEY_UP => {
                super::touch();
                // SAFETY: 同上，读键码字段。
                let code = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) } as u16;
                let vk = mac_key_to_vk(code);
                if vk == 0 {
                    return false;
                }
                let ev = if kind == KEY_DOWN {
                    EvKind::KeyDown
                } else {
                    EvKind::KeyUp
                };
                let r = s.gesture.on_event(&InputEvent::key(ev, vk, t));
                apply(s, r)
            }
            _ => {
                super::touch();
                let (x, y) = to_physical(s, location);
                let (ev, btn) = match kind {
                    // 拖动事件一律放行：在会话 tap 丢掉拖动会让光标卡住；没见过按下的程序会忽略它。
                    MOVED | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED => {
                        (EvKind::MouseMove, Btn::None)
                    }
                    LEFT_DOWN => (EvKind::MouseDown, Btn::Left),
                    LEFT_UP => (EvKind::MouseUp, Btn::Left),
                    RIGHT_DOWN => (EvKind::MouseDown, Btn::Right),
                    RIGHT_UP => (EvKind::MouseUp, Btn::Right),
                    OTHER_DOWN | OTHER_UP => {
                        // SAFETY: 同上，读按键编号字段。
                        let n = unsafe { CGEventGetIntegerValueField(event, FIELD_BUTTON_NUMBER) };
                        let btn = match n {
                            2 => Btn::Middle,
                            3 => Btn::X1,
                            4 => Btn::X2,
                            _ => return false,
                        };
                        (
                            if kind == OTHER_DOWN {
                                EvKind::MouseDown
                            } else {
                                EvKind::MouseUp
                            },
                            btn,
                        )
                    }
                    _ => return false,
                };
                if ev == EvKind::MouseMove {
                    super::set_cursor(x, y);
                    let r = s.gesture.on_event(&InputEvent::mouse(ev, btn, x, y, t));
                    apply(s, r);
                    return false;
                }
                if ev == EvKind::MouseDown {
                    // SAFETY: 同上，读连击次数字段。
                    s.click_states[btn_index(btn)] =
                        unsafe { CGEventGetIntegerValueField(event, FIELD_CLICK_STATE) };
                    s.gesture.sync_key_states(is_key_down);
                }
                let r = s.gesture.on_event(&InputEvent::mouse(ev, btn, x, y, t));
                apply(s, r)
            }
        }
    })
    .unwrap_or(false);
    if swallow { std::ptr::null_mut() } else { event }
}
