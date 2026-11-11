//! 2.0 设置的测试：默认值、坏值换默认、越界夹回范围、和手势状态机配置的对应。

use serde_json::json;

use super::*;
use crate::input::gesture::{MOD_ALT, MOD_CTRL, Trigger as GTrigger};

#[test]
fn empty_or_broken_input_gives_the_defaults() {
    assert_eq!(normalize(&json!({})), Settings::default());
    assert_eq!(normalize(&json!(null)), Settings::default());
    assert_eq!(normalize(&json!("garbage")), Settings::default());
    let d = Settings::default();
    assert_eq!(d.trigger, TriggerKey::Named(NamedTrigger::Alt));
    assert_eq!(d.wheel, DEFAULT_WHEEL.map(String::from).to_vec());
    assert_eq!(d.click_ping_id, "generic");
    assert_eq!(
        d.toggle_hotkey,
        Hotkey {
            mods: MOD_CTRL | MOD_ALT,
            vk: 0x50
        }
    );
    assert!(d.game_check && !d.click_ping && d.tick_sound && !d.muted);
}

#[test]
fn a_saved_file_reads_back_the_same() {
    let s = Settings {
        trigger: TriggerKey::Custom { vk: 0x47 },
        drag_threshold_px: 12,
        click_ping: true,
        click_ping_id: "danger".into(),
        game_check: false,
        wheel: [
            "push",
            "omw",
            "allin",
            "assist",
            "needvision",
            "missing",
            "enemyvision",
            "bait",
        ]
        .map(String::from)
        .to_vec(),
        wheel_size_px: 450,
        toggle_hotkey: Hotkey {
            mods: MOD_ALT,
            vk: 0x4F,
        },
        ping_size_px: 90,
        ping_duration_s: 2.5,
        volume: 30,
        muted: true,
        tick_sound: false,
        theme: "dark".into(),
        language: "en".into(),
        allow_internet: false,
        relays: vec!["wss://my.relay".into()],
        emote_trigger: None,
        emote_click: true,
        emote_wheel: [
            "facepalm",
            "wahaha",
            "gasp",
            "pain",
            "bee_sad",
            "gg_heart",
            "bee_happy",
            "bee_mad",
        ]
        .map(String::from)
        .to_vec(),
        click_emote: "c:0123456789abcdef0123456789abcdef".into(),
        emote_size_px: 200,
        emote_sound: false,
        custom_emotes: vec![CustomEmote {
            id: "c:0123456789abcdef0123456789abcdef".into(),
            name: "阿钢".into(),
            animated: true,
            sound: false,
            fx: 4,
        }],
        friend_emotes: false,
        team_text: false,
        text_hotkey: Hotkey {
            mods: MOD_CTRL,
            vk: 0x4D,
        },
        text_style: "danmaku".into(),
        text_size_px: 36,
        text_effect: "gold".into(),
        team_votes: false,
        team_panel: false,
        panel_key: 0xC0,
        voice: false,
        talk_key: 0x42,
        voice_volume: 35,
        mic: "wasapi:{abc}".into(),
        launch_at_startup: true,
        auto_update_check: false,
        panel_rect: Some(PanelRect {
            x: -1600,
            y: 120,
            width: 1500,
            height: 1050,
        }),
    };
    assert_eq!(normalize(&serde_json::to_value(&s).unwrap()), s);
}

#[test]
fn numbers_are_clamped_and_snapped_to_their_step() {
    let s = normalize(&json!({
        "dragThresholdPx": 100, "wheelSizePx": 455, "pingSizePx": 10, "pingDurationS": 2.345, "volume": -5
    }));
    assert_eq!(s.drag_threshold_px, 40);
    assert_eq!(s.wheel_size_px, 460);
    assert_eq!(s.ping_size_px, 60);
    assert_eq!(s.ping_duration_s, 2.3);
    assert_eq!(s.volume, 0);
    let s = normalize(&json!({ "volume": "loud", "pingDurationS": f64::NAN }));
    assert_eq!(s.volume, 70);
    assert_eq!(s.ping_duration_s, 3.2);
}

#[test]
fn triggers_accept_named_keys_mouse_buttons_and_safe_custom_keys() {
    let t = |v: serde_json::Value| normalize(&json!({ "trigger": v })).trigger;
    assert_eq!(t(json!("ctrl")), TriggerKey::Named(NamedTrigger::Ctrl));
    assert_eq!(t(json!("mouse5")), TriggerKey::Named(NamedTrigger::Mouse5));
    assert_eq!(t(json!({ "vk": 0x56 })), TriggerKey::Custom { vk: 0x56 });
    // 回车、Tab、空格、Esc、方向键当触发键会弄坏全系统的打字，换回默认。
    for vk in [0x0D, 0x09, 0x08, 0x20, 0x1B, 0x25, 0x28] {
        assert_eq!(
            t(json!({ "vk": vk })),
            TriggerKey::Named(NamedTrigger::Alt),
            "vk {vk:#x}"
        );
    }
    // 修饰键有自己的名字；0 和超出范围的键码不是键。
    for vk in [0xA4, 0x11, 0x5B, 0, 255, 1000] {
        assert_eq!(
            t(json!({ "vk": vk })),
            TriggerKey::Named(NamedTrigger::Alt),
            "vk {vk:#x}"
        );
    }
    assert_eq!(t(json!("meta")), TriggerKey::Named(NamedTrigger::Alt));
}

#[test]
fn caps_lock_trigger_is_windows_only() {
    let s = normalize(&json!({ "trigger": "capslock" }));
    if cfg!(windows) {
        assert_eq!(s.trigger, TriggerKey::Named(NamedTrigger::CapsLock));
    } else {
        assert_eq!(s.trigger, TriggerKey::Named(NamedTrigger::Alt));
    }
}

#[test]
fn hotkey_needs_ctrl_alt_or_win() {
    let h = |v: serde_json::Value| normalize(&json!({ "toggleHotkey": v })).toggle_hotkey;
    assert_eq!(
        h(json!({ "mods": 2, "vk": 0x4F })),
        Hotkey { mods: 2, vk: 0x4F }
    );
    assert_eq!(
        h(json!({ "mods": 9, "vk": 0x70 })),
        Hotkey { mods: 9, vk: 0x70 }
    ); // Ctrl+Win+F1
    // 只有 Shift 会把全系统的大写字母吞掉。
    assert_eq!(
        h(json!({ "mods": 4, "vk": 0x4F })),
        Settings::default().toggle_hotkey
    );
    assert_eq!(
        h(json!({ "mods": 0, "vk": 0x4F })),
        Settings::default().toggle_hotkey
    );
    assert_eq!(
        h(json!({ "mods": 2, "vk": 0 })),
        Settings::default().toggle_hotkey
    );
}

#[test]
fn wheel_must_be_eight_known_pings_each_once() {
    let w = |v: serde_json::Value| normalize(&json!({ "wheel": v })).wheel;
    let default = Settings::default().wheel;
    let ok = json!([
        "bait",
        "push",
        "omw",
        "allin",
        "assist",
        "needvision",
        "missing",
        "missingflip"
    ]);
    assert_eq!(
        w(ok.clone()),
        serde_json::from_value::<Vec<String>>(ok).unwrap()
    );
    assert_eq!(w(json!(["bait", "push"])), default);
    assert_eq!(
        w(json!([
            "bait",
            "bait",
            "omw",
            "allin",
            "assist",
            "needvision",
            "missing",
            "danger"
        ])),
        default
    );
    assert_eq!(
        w(json!([
            "nope",
            "push",
            "omw",
            "allin",
            "assist",
            "needvision",
            "missing",
            "danger"
        ])),
        default
    );
}

#[test]
fn unknown_click_ping_and_theme_fall_back() {
    let s = normalize(&json!({ "clickPingId": "nope", "theme": "purple" }));
    assert_eq!(s.click_ping_id, "generic");
    assert_eq!(s.theme, "system");
}

#[test]
fn language_defaults_to_system_for_old_files_and_unknown_values() {
    // 3.3.1 及以前的设置文件里没有 language：跟随系统（所有者定的，老用户也一样）
    assert_eq!(normalize(&json!({ "theme": "dark" })).language, "system");
    assert_eq!(normalize(&json!({ "language": "ja" })).language, "system");
    assert_eq!(normalize(&json!({ "language": 3 })).language, "system");
    for l in ["system", "zh", "en"] {
        assert_eq!(normalize(&json!({ "language": l })).language, l);
    }
}

#[test]
fn gesture_config_follows_the_settings() {
    let mut s = Settings {
        trigger: TriggerKey::Named(NamedTrigger::Mouse4),
        click_ping: true,
        drag_threshold_px: 15,
        toggle_hotkey: Hotkey {
            mods: MOD_ALT,
            vk: 0x4F,
        },
        ..Settings::default()
    };
    let c = s.gesture_config(false, false);
    assert_eq!(c.trigger, GTrigger::Mouse4);
    assert!(c.click_ping && c.game_check && !c.enabled);
    assert_eq!(c.emote_trigger, GTrigger::Ctrl);
    assert!(!c.emote_click);
    assert_eq!(c.drag_threshold_px, 15);
    assert_eq!((c.toggle_mods, c.toggle_vk), (MOD_ALT, 0x4F));
    s.trigger = TriggerKey::Custom { vk: 0x47 };
    assert_eq!(
        s.gesture_config(true, false).trigger,
        GTrigger::Custom(0x47)
    );
}

#[test]
fn overlay_gets_volume_as_a_fraction_and_zero_when_muted() {
    let mut s = Settings::default();
    assert_eq!(s.overlay().volume, 0.7);
    s.muted = true;
    assert_eq!(s.overlay().volume, 0.0);
}

#[test]
fn relays_keep_valid_unique_wss_addresses() {
    let s = normalize(
        &json!({ "relays": [" wss://a.example ", "ws://b.example", "wss://", "wss://a b", "wss://a.example", "wss://c.example"] }),
    );
    assert_eq!(
        s.relays,
        vec!["wss://a.example".to_string(), "wss://c.example".to_string()]
    );
    assert_eq!(s.relay_list(), s.relays);
    let d = Settings::default();
    assert!(d.allow_internet && d.relays.is_empty());
    assert_eq!(d.relay_list(), DEFAULT_RELAYS.map(String::from).to_vec());
    assert!(!normalize(&json!({ "allowInternet": false })).allow_internet);
}

#[test]
fn emote_key_is_off_or_a_trigger_that_clashes_with_nothing() {
    let d = Settings::default();
    assert_eq!(d.emote_trigger, Some(TriggerKey::Named(NamedTrigger::Ctrl)));
    assert_eq!(
        serde_json::to_value(&d).unwrap()["emoteTrigger"],
        json!("ctrl")
    );
    let off = normalize(&json!({ "emoteTrigger": "off" }));
    assert_eq!(off.emote_trigger, None);
    assert_eq!(
        serde_json::to_value(&off).unwrap()["emoteTrigger"],
        json!("off")
    );
    assert_eq!(
        off.gesture_config(true, false).emote_trigger,
        GTrigger::None
    );
    assert_eq!(
        normalize(&json!({ "emoteTrigger": "mouse5" })).emote_trigger,
        Some(TriggerKey::Named(NamedTrigger::Mouse5))
    );
    assert_eq!(
        normalize(&json!({ "emoteTrigger": 7 })).emote_trigger,
        d.emote_trigger,
        "坏值换 Ctrl"
    );
    // 和信号触发键一样：关掉
    assert_eq!(normalize(&json!({ "trigger": "ctrl" })).emote_trigger, None);
    // 自定义键撞上暂停快捷键的键：关掉
    assert_eq!(
        normalize(&json!({ "emoteTrigger": { "vk": 0x50 } })).emote_trigger,
        None
    );
    assert_eq!(
        normalize(&json!({ "emoteTrigger": { "vk": 0x54 } })).emote_trigger,
        Some(TriggerKey::Custom { vk: 0x54 })
    );
}

#[test]
fn emote_wheel_follows_the_imported_list() {
    let h = "0123456789abcdef0123456789abcdef";
    let mut wheel = emotes::DEFAULT_WHEEL.map(String::from).to_vec();
    wheel[3] = format!("c:{h}");
    let with = normalize(&json!({
        "customEmotes": [{ "id": format!("c:{h}"), "name": "图", "fx": 2 }],
        "emoteWheel": wheel, "clickEmote": format!("c:{h}"), "emoteSizePx": 999
    }));
    assert_eq!(with.emote_wheel[3], format!("c:{h}"));
    assert_eq!(with.click_emote, format!("c:{h}"));
    assert_eq!(with.emote_size_px, 300);
    assert_eq!(with.announced_emotes(), vec![h.to_string()]);
    let mut no_friends = with.clone();
    no_friends.friend_emotes = false;
    assert!(
        no_friends.announced_emotes().is_empty(),
        "不显示队友的表情就也不发自己的"
    );
    // 导入的删了：轮盘和单击表情都换回默认
    let without = normalize(&json!({ "emoteWheel": wheel, "clickEmote": format!("c:{h}") }));
    assert_eq!(
        without.emote_wheel,
        emotes::DEFAULT_WHEEL.map(String::from).to_vec()
    );
    assert_eq!(without.click_emote, emotes::DEFAULT_CLICK);
}

#[test]
fn team_keys_reach_the_gesture_only_inside_a_team() {
    let d = Settings::default();
    assert_eq!(
        d.text_hotkey,
        Hotkey {
            mods: MOD_ALT,
            vk: 0x54
        }
    );
    assert_eq!(d.panel_key, 0x09);
    assert!(d.team_text && d.team_votes && d.team_panel && !d.launch_at_startup);
    assert!(d.auto_update_check, "默认自动检查更新");
    let out = d.gesture_config(true, false);
    assert_eq!(
        (out.text_vk, out.panel_vk),
        (0, 0),
        "不在队伍里：Alt+T 和 Tab 照常给别的程序"
    );
    let inside = d.gesture_config(true, true);
    assert_eq!(
        (inside.text_mods, inside.text_vk, inside.panel_vk),
        (MOD_ALT, 0x54, 0x09)
    );
    let mut off = d.clone();
    off.team_text = false;
    off.team_panel = false;
    let c = off.gesture_config(true, true);
    assert_eq!((c.text_vk, c.panel_vk), (0, 0));
    use crate::protocol::live::{OFF_TEXT, OFF_VOTES};
    assert_eq!(d.features_off(), 0);
    off.team_votes = false;
    assert_eq!(off.features_off(), OFF_TEXT | OFF_VOTES);
}

#[test]
fn team_keys_never_share_a_key() {
    // 文字快捷键撞上暂停快捷键：换成 Alt+Y
    let s = normalize(
        &json!({ "toggleHotkey": { "mods": 2, "vk": 0x54 }, "textHotkey": { "mods": 2, "vk": 0x54 } }),
    );
    assert_eq!(
        s.text_hotkey,
        Hotkey {
            mods: MOD_ALT,
            vk: 0x59
        }
    );
    // 面板键：Esc、修饰键、被占的键都换掉
    assert_eq!(normalize(&json!({ "panelKey": 0x1B })).panel_key, 0x09);
    assert_eq!(normalize(&json!({ "panelKey": 0xA2 })).panel_key, 0x09);
    assert_eq!(normalize(&json!({ "panelKey": 0x71 })).panel_key, 0x71);
    let s = normalize(&json!({ "toggleHotkey": { "mods": 2, "vk": 0x09 }, "panelKey": 0x09 }));
    assert_eq!(s.panel_key, 0xC0, "Tab 被暂停快捷键占了，换成 `");
    let s = normalize(&json!({ "trigger": { "vk": 0x71 }, "panelKey": 0x71 }));
    assert_eq!(s.panel_key, 0x09, "F2 被自定义触发键占了");
    let s = normalize(&json!({ "emoteTrigger": { "vk": 0x72 }, "panelKey": 0x72 }));
    assert_eq!(s.panel_key, 0x09);
    assert_eq!(
        normalize(&json!({ "textStyle": "danmaku" })).text_style,
        "danmaku"
    );
    assert_eq!(
        normalize(&json!({ "textStyle": "marquee" })).text_style,
        "spot"
    );
}

#[test]
fn voice_defaults_to_on_with_v_as_the_talk_key() {
    let d = Settings::default();
    assert!(d.voice);
    assert_eq!((d.talk_key, d.voice_volume, d.mic.as_str()), (0x56, 80, ""));
    let s = normalize(
        &json!({ "voiceVolume": 150, "mic": "wasapi:{0.0.1.00000000}.{abc}", "voice": false }),
    );
    assert_eq!(
        (s.voice_volume, s.mic.as_str(), s.voice),
        (100, "wasapi:{0.0.1.00000000}.{abc}", false)
    );
    assert_eq!(
        normalize(&json!({ "mic": "x".repeat(600) })).mic,
        "",
        "太长的设备名不要"
    );
}

#[test]
fn talk_key_is_a_plain_safe_key_that_clashes_with_nothing() {
    assert_eq!(normalize(&json!({ "talkKey": 0x42 })).talk_key, 0x42);
    // 修饰键、空格、回车、Esc、方向键这些会弄坏打字的不要
    for bad in [0x10, 0xA2, 0x20, 0x0D, 0x1B, 0x25, 0, 300] {
        assert_eq!(
            normalize(&json!({ "talkKey": bad })).talk_key,
            0x56,
            "键 {bad:#x}"
        );
    }
    // 撞上触发键、表情键、暂停和文字快捷键的键：换成候选里第一个没被占的
    let s = normalize(&json!({ "trigger": { "vk": 0x56 } }));
    assert_eq!(s.talk_key, 0x47, "V 被自定义信号触发键占了，换成 G");
    let s = normalize(
        &json!({ "emoteTrigger": { "vk": 0x56 }, "toggleHotkey": { "mods": 3, "vk": 0x47 } }),
    );
    assert_eq!(s.talk_key, 0x5A);
    let s = normalize(&json!({ "talkKey": 0x54 }));
    assert_eq!(s.talk_key, 0x56, "T 是文字快捷键的键");
    // 面板键排在说话键后面：面板键选了说话键的键，换的是面板键
    let s = normalize(&json!({ "panelKey": 0x56 }));
    assert_eq!((s.talk_key, s.panel_key), (0x56, 0x09));
}

#[test]
fn talk_key_reaches_the_gesture_only_in_a_team_with_voice_on() {
    let s = Settings::default();
    assert_eq!(s.gesture_config(true, true).talk_vk, 0x56);
    assert_eq!(s.gesture_config(true, false).talk_vk, 0, "不在队伍里不拦");
    let off = Settings {
        voice: false,
        ..Settings::default()
    };
    assert_eq!(off.gesture_config(true, true).talk_vk, 0, "关了语音不拦");
}

#[test]
fn overlay_text_is_bigger_by_default_with_a_known_effect() {
    let d = Settings::default();
    assert_eq!((d.text_size_px, d.text_effect.as_str()), (28, "outline"));
    for e in TEXT_EFFECTS {
        assert_eq!(normalize(&json!({ "textEffect": e })).text_effect, e);
    }
    assert_eq!(
        normalize(&json!({ "textEffect": "rainbow" })).text_effect,
        "outline"
    );
    assert_eq!(normalize(&json!({ "textSizePx": 200 })).text_size_px, 56);
    assert_eq!(normalize(&json!({ "textSizePx": 3 })).text_size_px, 16);
    let o = normalize(&json!({ "textSizePx": 40, "textEffect": "neon" })).overlay();
    assert_eq!((o.text_size_px, o.text_effect.as_str()), (40, "neon"));
}

#[test]
fn replaced_keys_say_what_they_clashed_with() {
    let clashes = |v: serde_json::Value| normalize_keys(&v).1;
    let clash = |key, with| KeyClash { key, with };
    // 没给的行用默认值，不算换掉；存好的设置原样读回来也不算
    assert!(clashes(json!({})).is_empty());
    assert!(clashes(serde_json::to_value(Settings::default()).unwrap()).is_empty());
    assert_eq!(
        clashes(json!({ "trigger": { "vk": 0x56 }, "talkKey": 0x56 })),
        vec![clash("talk", Some("trigger"))],
        "V 被自定义信号触发键占了"
    );
    assert_eq!(
        clashes(json!({ "talkKey": 0x20 })),
        vec![clash("talk", None)],
        "空格本身不能当说话键"
    );
    assert_eq!(
        clashes(json!({ "toggleHotkey": { "mods": MOD_CTRL, "vk": 0x09 }, "panelKey": 0x09 })),
        vec![clash("panel", Some("toggle"))]
    );
    assert_eq!(
        clashes(json!({ "talkKey": 0x42, "panelKey": 0x42 })),
        vec![clash("panel", Some("talk"))],
        "说话键排在面板键前面"
    );
    assert_eq!(
        clashes(json!({ "trigger": "ctrl", "emoteTrigger": "ctrl" })),
        vec![clash("emote", Some("trigger"))]
    );
    let s = json!({ "emoteTrigger": "capslock", "toggleHotkey": { "mods": MOD_ALT, "vk": 0x14 } });
    let (settings, c) = normalize_keys(&s);
    if cfg!(windows) {
        assert_eq!(c, vec![clash("emote", Some("toggle"))]);
        assert_eq!(settings.emote_trigger, None, "撞了就关掉表情轮盘");
    }
    let toggle = Settings::default().toggle_hotkey;
    assert_eq!(
        clashes(json!({ "textHotkey": toggle })),
        vec![clash("text", Some("toggle"))]
    );
    assert_eq!(
        clashes(json!({ "trigger": { "vk": 0x0D } })),
        vec![clash("trigger", None)]
    );
}

#[test]
fn panel_rect_reads_back_only_when_sane() {
    let rect = |v: serde_json::Value| normalize(&json!({ "panelRect": v })).panel_rect;
    assert_eq!(Settings::default().panel_rect, None, "没记过就按默认的居中");
    assert_eq!(
        rect(json!({ "x": -1920, "y": 40, "width": 1500, "height": 1050 })),
        Some(PanelRect {
            x: -1920,
            y: 40,
            width: 1500,
            height: 1050
        }),
        "左边的副屏是负坐标"
    );
    assert_eq!(
        rect(json!({ "x": 0, "y": 0, "width": 10, "height": 700 })),
        None
    );
    assert_eq!(rect(json!({ "x": 0, "y": 0, "width": 1000 })), None);
    assert_eq!(rect(json!("garbage")), None);
}
