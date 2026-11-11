//! 界面语言（3.3.2）：中文、English 两种，默认跟随系统。
//!
//! 1. 设置里的 `language` 是 `system`、`zh`、`en`；这里把它换成实际用的语言 [`Lang`]，存一个全局值，Rust 里给用户看的文字按它出。
//! 2. 「跟随系统」看的是 Windows 的**显示语言**（`sys-locale` 调 `GetUserPreferredUILanguages`，不是区域格式），
//!    macOS 是系统的首选语言。中文（简体、繁体都算）→ 中文，其他所有语言 → English。系统语言改了，下次启动才跟着变。
//! 3. 各个网页窗口怎么知道语言：透明窗口和三个小窗口建窗口时塞一段初始化脚本（`window.__LOLPING_LANG__`，`overlay::floating_window`），
//!    控制面板从 `status_get` 拿；之后语言变了发 `lang://changed`（内容是 `"zh"` 或 `"en"`）给所有窗口。都不用给窗口加新权限。
//! 4. 只写进日志的文字（`eprintln!`、崩溃报告）不翻译，照旧中文。

use std::sync::atomic::{AtomicU8, Ordering};

use serde::Serialize;

/// 设置里 `language` 能取的值，第一个是默认值。
pub const LANGUAGES: [&str; 3] = ["system", "zh", "en"];

/// 实际用的语言。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    /// 发给网页的写法，也是设置里的写法。
    pub fn code(self) -> &'static str {
        match self {
            Lang::Zh => "zh",
            Lang::En => "en",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0); // 0 中文、1 English；设置读出来以前先当中文

/// 现在用的语言。
pub fn current() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Zh
    }
}

/// 换语言；返回是不是真的变了。
pub fn set(lang: Lang) -> bool {
    let v = match lang {
        Lang::Zh => 0,
        Lang::En => 1,
    };
    CURRENT.swap(v, Ordering::Relaxed) != v
}

/// 系统语言的标签（比如 `zh-CN`、`en-US`）→ 中文或 English。
pub fn from_locale(tag: &str) -> Lang {
    let primary = tag.split(['-', '_']).next().unwrap_or("");
    if primary.eq_ignore_ascii_case("zh") {
        Lang::Zh
    } else {
        Lang::En
    }
}

/// 设置里的值 + 系统语言 → 实际用的语言。`system`（以及不认识的值）跟随系统，读不到系统语言当 English。
pub fn resolve(setting: &str, system: Option<&str>) -> Lang {
    match setting {
        "zh" => Lang::Zh,
        "en" => Lang::En,
        _ => system.map_or(Lang::En, from_locale),
    }
}

/// 按设置换语言（读系统语言）；返回是不是真的变了。
pub fn apply(setting: &str) -> bool {
    set(resolve(setting, sys_locale::get_locale().as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_system_languages_give_chinese_everything_else_english() {
        for tag in [
            "zh-CN",
            "zh-TW",
            "zh-Hant-HK",
            "zh-Hans",
            "zh",
            "ZH-cn",
            "zh_CN",
        ] {
            assert_eq!(from_locale(tag), Lang::Zh, "{tag}");
        }
        for tag in ["en-US", "en", "ja-JP", "ko-KR", "fr-FR", "", "zhx", "c"] {
            assert_eq!(from_locale(tag), Lang::En, "{tag}");
        }
    }

    #[test]
    fn explicit_choice_wins_and_system_follows_the_system_language() {
        assert_eq!(resolve("zh", Some("en-US")), Lang::Zh);
        assert_eq!(resolve("en", Some("zh-CN")), Lang::En);
        assert_eq!(resolve("system", Some("zh-CN")), Lang::Zh);
        assert_eq!(resolve("system", Some("de-DE")), Lang::En);
        // 读不到系统语言、设置里是不认识的值：都当跟随系统
        assert_eq!(resolve("system", None), Lang::En);
        assert_eq!(resolve("klingon", Some("zh-TW")), Lang::Zh);
    }

    #[test]
    fn set_reports_whether_the_language_changed() {
        set(Lang::Zh);
        assert!(!set(Lang::Zh));
        assert!(set(Lang::En));
        assert_eq!(current(), Lang::En);
        assert!(set(Lang::Zh));
        assert_eq!(current(), Lang::Zh);
    }
}
