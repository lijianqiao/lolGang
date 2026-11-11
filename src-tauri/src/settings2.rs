//! 2.0 的设置（架构第 8 节）：信号轮盘、表情、队伍文字、面板键、功能开关来自 lolPing 设计文档 8.1 节和
//! `src/shared/settings.ts`、`features.ts`（MIT，© 2026 Danrui Meng），加上 1.x 的主题、队伍的网络、开机启动。
//!
//! 实现流程：
//! 1. 存的是一份 JSON（`store` 负责读写）。读出来先过 `normalize`：缺的字段补默认，类型不对或不合法的换默认，
//!    数字夹到范围内并对齐步长。所以旧版本、手改坏了的设置文件都能变成合法设置，新加字段只要在这里给默认值。
//! 2. 控制面板改设置时把整份设置交回来，同样过一遍 `normalize` 再存，存下的永远合法。
//! 3. `gesture_config` 把设置换成手势状态机的配置；`overlay` 换成透明窗口画画要用的那部分。
//!    文字快捷键和面板键只在队伍里、对应功能开着时交给手势，别的时候 Alt+T、Tab 照常给别的程序。
//! 4. 键不能两用：表情触发键撞了就关掉；文字快捷键撞上暂停快捷键就换一个备用的；
//!    说话键撞上触发键、表情键、快捷键的键就换成备用列表里第一个没被占的（V、G、Z、X、N、M）；
//!    面板键排在最后，撞上任何一个键就换成备用列表里第一个没被占的（Tab、`、F2、F3、F4、F5）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::emotes::{self, CustomEmote};
use crate::i18n;
use crate::input::gesture::{self, MOD_ALT, MOD_CTRL, MOD_WIN};

/// 全部信号的 id，和前端 `src/shared/pings.ts` 一致。
pub const PING_IDS: [&str; 12] = [
    "danger",
    "push",
    "omw",
    "allin",
    "assist",
    "needvision",
    "missing",
    "enemyvision",
    "generic",
    "bait",
    "visioncleared",
    "missingflip",
];
/// 英雄联盟自带的轮盘，从正上方顺时针。
pub const DEFAULT_WHEEL: [&str; 8] = [
    "danger",
    "push",
    "omw",
    "allin",
    "assist",
    "needvision",
    "missing",
    "enemyvision",
];
pub const DEFAULT_CLICK_PING: &str = "generic";
pub const THEMES: [&str; 3] = ["system", "light", "dark"];
/// 队友文字的显示方式：在说话的位置冒出来，或者弹幕从右往左飘过。
pub const TEXT_STYLES: [&str; 2] = ["spot", "danmaku"];
/// 屏幕上的字的字幕特效（3.0）：描边大字、金色渐变、霓虹发光、逐字弹跳、打字机、抖动强调。
pub const TEXT_EFFECTS: [&str; 6] = ["outline", "gold", "neon", "bounce", "typewriter", "shake"];
/// 文字快捷键的候选：默认 Alt+T，撞了依次换。
const TEXT_HOTKEYS: [Hotkey; 3] = [
    Hotkey {
        mods: MOD_ALT,
        vk: 0x54,
    },
    Hotkey {
        mods: MOD_ALT,
        vk: 0x59,
    },
    Hotkey {
        mods: MOD_CTRL | MOD_ALT,
        vk: 0x54,
    },
];
/// 面板键的候选：Tab，然后 `、F2、F3、F4、F5。最多 5 个键被占（两个触发键、两个快捷键、说话键），所以总有一个空着。
const PANEL_KEYS: [u32; 6] = [0x09, 0xC0, 0x71, 0x72, 0x73, 0x74];
/// 说话键的候选：V，撞了依次换。最多 4 个键被占（两个触发键、两个快捷键），所以总有一个空着。
const TALK_KEYS: [u32; 6] = [0x56, 0x47, 0x5A, 0x58, 0x4E, 0x4D];
/// 麦克风设备 id 的长度上限（cpal 的 id 像 "wasapi:{0.0.1.00000000}.{guid}"）。
const MIC_ID_MAX: usize = 512;
/// 默认中继（1.x 架构 6.4，2026-11-02 实测选定）。
pub const DEFAULT_RELAYS: [&str; 8] = [
    "wss://relay.nostrfeed.com",
    "wss://relay.ditto.pub",
    "wss://purplerelay.com",
    "wss://relay.mostro.network",
    "wss://relay.wisp.talk",
    "wss://data-space.world/r/relais-public-2",
    "wss://relay.dreamith.to",
    "wss://relay.gulugulu.moe",
];
/// 自己填的中继最多这么多个。
const MAX_RELAYS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NamedTrigger {
    Alt,
    Ctrl,
    Shift,
    Win,
    CapsLock,
    Mouse4,
    Mouse5,
}

/// 触发键：有名字的键或侧键（存成 `"alt"`），或自定义按键（存成 `{ "vk": 71 }`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TriggerKey {
    Named(NamedTrigger),
    Custom { vk: u32 },
}

/// 表情触发键：和信号触发键一样，或者 `"off"`（表情轮盘关掉）。
mod emote_key {
    use super::TriggerKey;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(t: &Option<TriggerKey>, s: S) -> Result<S::Ok, S::Error> {
        match t {
            Some(k) => k.serialize(s),
            None => s.serialize_str("off"),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<TriggerKey>, D::Error> {
        let v = serde_json::Value::deserialize(d)?;
        Ok(serde_json::from_value(v).ok())
    }
}

/// 控制面板的位置（外框左上角）和大小（网页区域），物理像素。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 快捷键：修饰键位（Ctrl 1、Alt 2、Shift 4、Win 8）加一个虚拟键码。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub trigger: TriggerKey,
    pub drag_threshold_px: u32,
    pub click_ping: bool,
    /// 触发键加单击放的信号。
    pub click_ping_id: String,
    /// 全屏程序（游戏）占着屏幕时，触发键加点击原样交给它。
    pub game_check: bool,
    /// 轮盘的 8 格，从正上方顺时针。
    pub wheel: Vec<String>,
    pub wheel_size_px: u32,
    pub toggle_hotkey: Hotkey,
    pub ping_size_px: u32,
    pub ping_duration_s: f64,
    pub volume: u32,
    pub muted: bool,
    pub tick_sound: bool,
    pub theme: String,
    /// 界面语言（3.3.2）：`system` 跟随系统、`zh`、`en`，见 `i18n.rs`。老设置文件里没有，当跟随系统。
    pub language: String,
    /// 不在同一局域网时经公共中继转发；关掉就只用局域网。
    pub allow_internet: bool,
    /// 自己填的中继；空表示用默认列表（以后默认列表更新了，没改过的人自动用上新的）。
    pub relays: Vec<String>,
    /// 按住它拖动开表情轮盘；None（存成 "off"）表示不用表情。不会和信号触发键、暂停快捷键的键相同。
    #[serde(with = "emote_key")]
    pub emote_trigger: Option<TriggerKey>,
    /// 表情触发键加单击放 `click_emote`。
    pub emote_click: bool,
    /// 表情轮盘的 8 格：内置表情的 slug 或导入表情的 `c:` 加哈希，从正上方顺时针。
    pub emote_wheel: Vec<String>,
    pub click_emote: String,
    pub emote_size_px: u32,
    pub emote_sound: bool,
    /// 导入的表情，最多 24 个，文件在数据目录的 `emotes/`。
    pub custom_emotes: Vec<CustomEmote>,
    /// 显示队友导入的表情（要传图）；关掉就不收也不发，队友的导入表情显示成问号。
    pub friend_emotes: bool,
    /// 队伍文字：快捷键打开输入框，说的话在队友屏幕上显示。
    pub team_text: bool,
    pub text_hotkey: Hotkey,
    /// spot 或 danmaku。
    pub text_style: String,
    /// 屏幕上的字多大（CSS 像素），默认 28。
    pub text_size_px: u32,
    /// 字幕特效，`TEXT_EFFECTS` 之一。
    pub text_effect: String,
    /// 投票：投降、准备确认、踢人。关掉后不算投票人。
    pub team_votes: bool,
    /// 按住面板键显示队伍面板。
    pub team_panel: bool,
    /// 面板键的虚拟键码，不带修饰键。
    pub panel_key: u32,
    /// 语音：按住说话键说话，队友的话直接播放。关掉后说话键不拦、队友的语音不放。
    pub voice: bool,
    /// 说话键（按住）的虚拟键码，不带修饰键。
    pub talk_key: u32,
    /// 队友语音的音量（0–100），和信号的音量分开。
    pub voice_volume: u32,
    /// 麦克风：cpal 的设备 id；空表示跟随系统默认。
    pub mic: String,
    /// 开机时自动启动（只在改动时去改系统设置）。
    pub launch_at_startup: bool,
    /// 启动后和每 6 小时查一次新版本（只提示，不自己装）。
    pub auto_update_check: bool,
    /// 控制面板上次关掉时的位置和大小（3.1.1）；没有就按默认的居中打开。
    /// 只有 Rust 在关控制面板时写（`tray::remember_main`），控制面板存设置时不改它。
    pub panel_rect: Option<PanelRect>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            trigger: TriggerKey::Named(NamedTrigger::Alt),
            drag_threshold_px: 8,
            click_ping: false,
            click_ping_id: DEFAULT_CLICK_PING.into(),
            game_check: true,
            wheel: DEFAULT_WHEEL.map(String::from).to_vec(),
            wheel_size_px: 600,
            toggle_hotkey: Hotkey {
                mods: MOD_CTRL | MOD_ALT,
                vk: 0x50,
            },
            ping_size_px: 110,
            ping_duration_s: 3.2,
            volume: 70,
            muted: false,
            tick_sound: true,
            theme: THEMES[0].into(),
            language: i18n::LANGUAGES[0].into(),
            allow_internet: true,
            relays: Vec::new(),
            emote_trigger: Some(TriggerKey::Named(NamedTrigger::Ctrl)),
            emote_click: false,
            emote_wheel: emotes::DEFAULT_WHEEL.map(String::from).to_vec(),
            click_emote: emotes::DEFAULT_CLICK.into(),
            emote_size_px: 150,
            emote_sound: true,
            custom_emotes: Vec::new(),
            friend_emotes: true,
            team_text: true,
            text_hotkey: TEXT_HOTKEYS[0],
            text_style: TEXT_STYLES[0].into(),
            text_size_px: 28,
            text_effect: TEXT_EFFECTS[0].into(),
            team_votes: true,
            team_panel: true,
            panel_key: PANEL_KEYS[0],
            voice: true,
            talk_key: TALK_KEYS[0],
            voice_volume: 80,
            mic: String::new(),
            launch_at_startup: false,
            auto_update_check: true,
            panel_rect: None,
        }
    }
}

/// 透明窗口画画要的设置。
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverlaySettings {
    pub wheel_size_px: u32,
    pub ping_size_px: u32,
    pub ping_duration_s: f64,
    /// 0 到 1，静音时是 0。
    pub volume: f64,
    pub tick_sound: bool,
    pub wheel: Vec<String>,
    pub click_ping_id: String,
    pub emote_wheel: Vec<String>,
    pub click_emote: String,
    pub emote_size_px: u32,
    pub emote_sound: bool,
    pub custom_emotes: Vec<CustomEmote>,
    pub friend_emotes: bool,
    pub text_style: String,
    pub text_size_px: u32,
    pub text_effect: String,
}

/// 数字设置的范围和步长（同 lolPing 的 LIMITS）。
struct Limit {
    min: f64,
    max: f64,
    step: f64,
}
const DRAG: Limit = Limit {
    min: 2.0,
    max: 40.0,
    step: 1.0,
};
const WHEEL_SIZE: Limit = Limit {
    min: 300.0,
    max: 900.0,
    step: 10.0,
};
const PING_SIZE: Limit = Limit {
    min: 60.0,
    max: 220.0,
    step: 1.0,
};
const DURATION: Limit = Limit {
    min: 1.0,
    max: 8.0,
    step: 0.1,
};
const VOLUME: Limit = Limit {
    min: 0.0,
    max: 100.0,
    step: 1.0,
};
const TEXT_SIZE: Limit = Limit {
    min: 16.0,
    max: 56.0,
    step: 1.0,
};
const EMOTE_SIZE: Limit = Limit {
    min: 80.0,
    max: 300.0,
    step: 1.0,
};

/// 自定义触发键不能用的键：触发键在全系统被拦下，这些键会弄坏打字、提交表单、切换焦点、移动光标。
const UNSAFE_TRIGGER_VKS: [u32; 9] = [0x0D, 0x09, 0x08, 0x20, 0x1B, 0x25, 0x26, 0x27, 0x28];
/// 记下的控制面板宽、高（物理像素）在这个范围里才用，别的当没记过。
const PANEL_SIDE: std::ops::RangeInclusive<u32> = 200..=16_000;
/// 修饰键有自己的名字，不当自定义键。
const MODIFIER_VKS: [u32; 11] = [
    0x10, 0x11, 0x12, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C,
];

fn is_vk(v: u32) -> bool {
    (1..=254).contains(&v)
}

fn num(raw: Option<&Value>, l: &Limit, default: f64) -> f64 {
    let Some(v) = raw.and_then(Value::as_f64).filter(|v| v.is_finite()) else {
        return default;
    };
    let snapped = (v.clamp(l.min, l.max) / l.step).round() * l.step;
    // 去掉 0.1 步长的浮点尾巴（2.3000000000000003）
    (snapped * 10.0).round() / 10.0
}

fn bool_or(raw: Option<&Value>, default: bool) -> bool {
    raw.and_then(Value::as_bool).unwrap_or(default)
}

/// 快捷键页的一行要的键被换掉了：哪一行（trigger、emote、toggle、text、talk、panel），和哪一行撞了；
/// `with` 为 None 是这个键本身不能用。设置里没给这一行（用默认值）时不算。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct KeyClash {
    pub key: &'static str,
    pub with: Option<&'static str>,
}

/// 要的键被换掉的原因：和哪一行撞了；None 是这个键本身不能用。
type Why = Option<&'static str>;

/// 合法就用，否则用 `default`；设置里给了这一行却不合法时，原因是「不能用」。
fn valid_or<T>(valid: Option<T>, raw: Option<&Value>, default: T) -> (T, Option<Why>) {
    match valid {
        Some(v) => (v, None),
        None => (default, raw.map(|_| None)),
    }
}

/// 合法的触发键；不合法的是 None。
fn valid_trigger(raw: Option<&Value>) -> Option<TriggerKey> {
    match raw.and_then(|v| serde_json::from_value::<TriggerKey>(v.clone()).ok())? {
        // macOS 拦不住 Caps Lock 的切换，只有 Windows 提供（同 lolPing）。
        TriggerKey::Named(NamedTrigger::CapsLock) if !cfg!(windows) => None,
        TriggerKey::Custom { vk }
            if !is_vk(vk) || UNSAFE_TRIGGER_VKS.contains(&vk) || MODIFIER_VKS.contains(&vk) =>
        {
            None
        }
        t => Some(t),
    }
}

/// 表情触发键：`"off"` 是关，其余按触发键的规矩（不合法的换成 Ctrl）。和信号触发键相同、
/// 或者是自定义键、Caps Lock 而键码撞上暂停快捷键的键时关掉：同一个键不能两用。
fn emote_trigger(
    raw: Option<&Value>,
    ping: TriggerKey,
    toggle: Hotkey,
) -> (Option<TriggerKey>, Option<Why>) {
    if raw.and_then(Value::as_str) == Some("off") {
        return (None, None);
    }
    let (t, why) = valid_or(
        valid_trigger(raw),
        raw,
        TriggerKey::Named(NamedTrigger::Ctrl),
    );
    let clash = if t == ping {
        Some("trigger")
    } else if trigger_vk(t) == Some(toggle.vk) {
        Some("toggle")
    } else {
        None
    };
    match clash {
        None => (Some(t), why),
        // 要的键不合法时说它不能用（换上的 Ctrl 又撞了，只好关掉）
        Some(with) => (None, why.or(raw.map(|_| Some(with)))),
    }
}

/// 文字快捷键：撞上暂停快捷键时换成候选里第一个不撞的。
fn text_hotkey(raw: Option<&Value>, toggle: Hotkey) -> (Hotkey, Option<Why>) {
    let (want, why) = valid_or(valid_hotkey(raw), raw, TEXT_HOTKEYS[0]);
    if want != toggle {
        return (want, why);
    }
    let other = TEXT_HOTKEYS
        .into_iter()
        .find(|h| *h != toggle)
        .unwrap_or(TEXT_HOTKEYS[0]);
    (other, why.or(raw.map(|_| Some("toggle"))))
}

/// 面板键、说话键（3.1.1 合成一个）：普通键——合法的键码、不是修饰键、不在 `forbidden` 里，
/// 也不是别的键占着的（`taken`：键码和占着它的那一行）；否则换 `candidates` 里第一个没被占的。
fn plain_key(
    raw: Option<&Value>,
    forbidden: &[u32],
    candidates: [u32; 6],
    taken: &[(u32, &'static str)],
) -> (u32, Option<Why>) {
    let owner = |vk: u32| taken.iter().find(|(k, _)| *k == vk).map(|(_, row)| *row);
    let asked = raw
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .filter(|&vk| is_vk(vk) && !forbidden.contains(&vk) && !MODIFIER_VKS.contains(&vk));
    let why = match asked {
        Some(vk) => match owner(vk) {
            None => return (vk, None),
            Some(row) => Some(Some(row)),
        },
        None => raw.map(|_| None),
    };
    let free = candidates
        .into_iter()
        .find(|&k| owner(k).is_none())
        .unwrap_or(candidates[0]);
    (free, why)
}

/// 自定义触发键和 Caps Lock 的键码；其他触发方式没有。
fn trigger_vk(t: TriggerKey) -> Option<u32> {
    match t {
        TriggerKey::Custom { vk } => Some(vk),
        TriggerKey::Named(NamedTrigger::CapsLock) => Some(0x14),
        _ => None,
    }
}

/// 合法的快捷键；不合法的是 None。必须带 Ctrl、Alt 或 Win：只有 Shift 的快捷键会把全系统的大写字母吞掉。
fn valid_hotkey(raw: Option<&Value>) -> Option<Hotkey> {
    raw.and_then(|v| serde_json::from_value::<Hotkey>(v.clone()).ok())
        .filter(|h| {
            (1..=15).contains(&h.mods)
                && h.mods & (MOD_CTRL | MOD_ALT | MOD_WIN) != 0
                && is_vk(h.vk)
        })
}

fn is_ping_id(s: &str) -> bool {
    PING_IDS.contains(&s)
}

fn wheel(raw: Option<&Value>) -> Vec<String> {
    let list = raw.and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok());
    match list {
        Some(w)
            if w.len() == DEFAULT_WHEEL.len()
                && w.iter().all(|id| is_ping_id(id))
                && w.iter().enumerate().all(|(i, id)| !w[..i].contains(id)) =>
        {
            w
        }
        _ => Settings::default().wheel,
    }
}

/// 只留 wss:// 开头、不含空白的地址，去掉重复的，最多 16 个。
fn relays(raw: Option<&Value>) -> Vec<String> {
    let list = raw
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for url in list {
        let url = url.trim();
        let host = url.strip_prefix("wss://").unwrap_or("");
        if !host.is_empty()
            && !host.chars().any(char::is_whitespace)
            && !out.iter().any(|u| u == url)
        {
            out.push(url.to_string());
        }
    }
    out.truncate(MAX_RELAYS);
    out
}

fn one_of(raw: Option<&Value>, ok: impl Fn(&str) -> bool, default: &str) -> String {
    raw.and_then(Value::as_str)
        .filter(|s| ok(s))
        .unwrap_or(default)
        .to_string()
}

/// 任何 JSON → 合法设置。
pub fn normalize(raw: &Value) -> Settings {
    normalize_keys(raw).0
}

/// 和 `normalize` 一样，另外说出哪些按键被换掉了、和谁撞了（快捷键页在那一行下面说明；
/// 3.1.1 以前前端自己又算了一遍撞键规则）。同一个键不能两用，排在前面的留下：
/// 信号触发键、暂停快捷键、表情触发键、文字快捷键、说话键、面板键。
pub fn normalize_keys(raw: &Value) -> (Settings, Vec<KeyClash>) {
    let d = Settings::default();
    let get = |k: &str| raw.get(k);
    let (trigger, trigger_why) = valid_or(valid_trigger(get("trigger")), get("trigger"), d.trigger);
    let (toggle_hotkey, toggle_why) = valid_or(
        valid_hotkey(get("toggleHotkey")),
        get("toggleHotkey"),
        d.toggle_hotkey,
    );
    let custom = emotes::normalize_custom(get("customEmotes"));
    let (emote_trigger, emote_why) = emote_trigger(get("emoteTrigger"), trigger, toggle_hotkey);
    let (text_hotkey, text_why) = text_hotkey(get("textHotkey"), toggle_hotkey);
    let mut taken: Vec<(u32, &'static str)> = [
        trigger_vk(trigger).map(|vk| (vk, "trigger")),
        emote_trigger.and_then(trigger_vk).map(|vk| (vk, "emote")),
    ]
    .into_iter()
    .flatten()
    .chain([(toggle_hotkey.vk, "toggle"), (text_hotkey.vk, "text")])
    .collect();
    // 说话键：不能是空格、回车这些会弄坏打字的键
    let (talk_key, talk_why) = plain_key(get("talkKey"), &UNSAFE_TRIGGER_VKS, TALK_KEYS, &taken);
    taken.push((talk_key, "talk"));
    // 面板键：不能是 Esc
    let (panel_key, panel_why) = plain_key(get("panelKey"), &[0x1B], PANEL_KEYS, &taken);
    let clashes = [
        ("trigger", trigger_why),
        ("emote", emote_why),
        ("toggle", toggle_why),
        ("text", text_why),
        ("talk", talk_why),
        ("panel", panel_why),
    ]
    .into_iter()
    .filter_map(|(key, why)| why.map(|with| KeyClash { key, with }))
    .collect();
    let mic = get("mic")
        .and_then(Value::as_str)
        .filter(|m| m.len() <= MIC_ID_MAX)
        .unwrap_or_default()
        .to_string();
    let settings = Settings {
        trigger,
        drag_threshold_px: num(
            get("dragThresholdPx"),
            &DRAG,
            f64::from(d.drag_threshold_px),
        ) as u32,
        click_ping: bool_or(get("clickPing"), d.click_ping),
        click_ping_id: one_of(get("clickPingId"), is_ping_id, DEFAULT_CLICK_PING),
        game_check: bool_or(get("gameCheck"), d.game_check),
        wheel: wheel(get("wheel")),
        wheel_size_px: num(get("wheelSizePx"), &WHEEL_SIZE, f64::from(d.wheel_size_px)) as u32,
        toggle_hotkey,
        ping_size_px: num(get("pingSizePx"), &PING_SIZE, f64::from(d.ping_size_px)) as u32,
        ping_duration_s: num(get("pingDurationS"), &DURATION, d.ping_duration_s),
        volume: num(get("volume"), &VOLUME, f64::from(d.volume)) as u32,
        muted: bool_or(get("muted"), d.muted),
        tick_sound: bool_or(get("tickSound"), d.tick_sound),
        theme: one_of(get("theme"), |t| THEMES.contains(&t), THEMES[0]),
        language: one_of(
            get("language"),
            |l| i18n::LANGUAGES.contains(&l),
            i18n::LANGUAGES[0],
        ),
        allow_internet: bool_or(get("allowInternet"), d.allow_internet),
        relays: relays(get("relays")),
        emote_trigger,
        emote_click: bool_or(get("emoteClick"), d.emote_click),
        emote_wheel: emotes::normalize_wheel(get("emoteWheel"), &custom),
        click_emote: emotes::normalize_click(get("clickEmote"), &custom),
        emote_size_px: num(get("emoteSizePx"), &EMOTE_SIZE, f64::from(d.emote_size_px)) as u32,
        emote_sound: bool_or(get("emoteSound"), d.emote_sound),
        custom_emotes: custom,
        friend_emotes: bool_or(get("friendEmotes"), d.friend_emotes),
        team_text: bool_or(get("teamText"), d.team_text),
        text_hotkey,
        text_style: one_of(
            get("textStyle"),
            |t| TEXT_STYLES.contains(&t),
            TEXT_STYLES[0],
        ),
        text_size_px: num(get("textSizePx"), &TEXT_SIZE, f64::from(d.text_size_px)) as u32,
        text_effect: one_of(
            get("textEffect"),
            |t| TEXT_EFFECTS.contains(&t),
            TEXT_EFFECTS[0],
        ),
        team_votes: bool_or(get("teamVotes"), d.team_votes),
        team_panel: bool_or(get("teamPanel"), d.team_panel),
        panel_key,
        voice: bool_or(get("voice"), d.voice),
        talk_key,
        voice_volume: num(get("voiceVolume"), &VOLUME, f64::from(d.voice_volume)) as u32,
        mic,
        launch_at_startup: bool_or(get("launchAtStartup"), d.launch_at_startup),
        auto_update_check: bool_or(get("autoUpdateCheck"), d.auto_update_check),
        panel_rect: get("panelRect")
            .and_then(|v| serde_json::from_value::<PanelRect>(v.clone()).ok())
            .filter(|r| PANEL_SIDE.contains(&r.width) && PANEL_SIDE.contains(&r.height)),
    };
    (settings, clashes)
}

impl Settings {
    /// 节点实际要连的中继：关了「允许互联网」就一个都不连。
    pub fn active_relays(&self) -> Vec<String> {
        if self.allow_internet {
            self.relay_list()
        } else {
            Vec::new()
        }
    }

    /// 自己填了就用自己的，否则用默认列表。
    pub fn relay_list(&self) -> Vec<String> {
        if self.relays.is_empty() {
            DEFAULT_RELAYS.map(String::from).to_vec()
        } else {
            self.relays.clone()
        }
    }

    /// 手势状态机的配置。`enabled` 是当前的信号开关（托盘、暂停快捷键），不存在设置里；
    /// `in_team` 是现在在不在一个已加入的队伍里（文字快捷键和面板键只在队伍里拦）。
    pub fn gesture_config(&self, enabled: bool, in_team: bool) -> gesture::Config {
        gesture::Config {
            trigger: gesture_trigger(self.trigger),
            click_ping: self.click_ping,
            game_check: self.game_check,
            emote_trigger: self
                .emote_trigger
                .map_or(gesture::Trigger::None, gesture_trigger),
            emote_click: self.emote_click,
            drag_threshold_px: self.drag_threshold_px as i32,
            toggle_mods: self.toggle_hotkey.mods,
            toggle_vk: self.toggle_hotkey.vk,
            text_mods: self.text_hotkey.mods,
            text_vk: if in_team && self.team_text {
                self.text_hotkey.vk
            } else {
                0
            },
            panel_vk: if in_team && self.team_panel {
                self.panel_key
            } else {
                0
            },
            talk_vk: if in_team && self.voice {
                self.talk_key
            } else {
                0
            },
            enabled,
        }
    }

    pub fn overlay(&self) -> OverlaySettings {
        OverlaySettings {
            wheel_size_px: self.wheel_size_px,
            ping_size_px: self.ping_size_px,
            ping_duration_s: self.ping_duration_s,
            volume: if self.muted {
                0.0
            } else {
                f64::from(self.volume) / 100.0
            },
            tick_sound: self.tick_sound,
            wheel: self.wheel.clone(),
            click_ping_id: self.click_ping_id.clone(),
            emote_wheel: self.emote_wheel.clone(),
            click_emote: self.click_emote.clone(),
            emote_size_px: self.emote_size_px,
            emote_sound: self.emote_sound,
            custom_emotes: self.custom_emotes.clone(),
            friend_emotes: self.friend_emotes,
            text_style: self.text_style.clone(),
            text_size_px: self.text_size_px,
            text_effect: self.text_effect.clone(),
        }
    }

    /// 语音线程要的那部分设置。
    pub fn voice_config(&self) -> crate::voice::VoiceConfig {
        crate::voice::VoiceConfig {
            enabled: self.voice,
            volume: self.voice_volume as f32 / 100.0,
            mic: self.mic.clone(),
        }
    }

    /// 在线状态里「关掉了的功能」。
    pub fn features_off(&self) -> u8 {
        use crate::protocol::live::{OFF_TEXT, OFF_VOTES};
        (if self.team_text { 0 } else { OFF_TEXT }) | (if self.team_votes { 0 } else { OFF_VOTES })
    }

    /// 在线状态里报的导入表情哈希（关了「显示队友的表情」就不报，也就没人来要）。
    pub fn announced_emotes(&self) -> Vec<String> {
        if self.emote_trigger.is_none() || !self.friend_emotes {
            return Vec::new();
        }
        emotes::announced(&self.emote_wheel, &self.click_emote)
    }
}

fn gesture_trigger(t: TriggerKey) -> gesture::Trigger {
    match t {
        TriggerKey::Named(NamedTrigger::Alt) => gesture::Trigger::Alt,
        TriggerKey::Named(NamedTrigger::Ctrl) => gesture::Trigger::Ctrl,
        TriggerKey::Named(NamedTrigger::Shift) => gesture::Trigger::Shift,
        TriggerKey::Named(NamedTrigger::Win) => gesture::Trigger::Win,
        TriggerKey::Named(NamedTrigger::CapsLock) => gesture::Trigger::CapsLock,
        TriggerKey::Named(NamedTrigger::Mouse4) => gesture::Trigger::Mouse4,
        TriggerKey::Named(NamedTrigger::Mouse5) => gesture::Trigger::Mouse5,
        TriggerKey::Custom { vk } => gesture::Trigger::Custom(vk),
    }
}

#[cfg(test)]
mod tests;
