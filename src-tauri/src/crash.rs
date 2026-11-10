//! 崩溃记录（3.3.0）：程序 panic 时在数据目录的 `crash.log` 里留一条，报 bug 时有线索。
//!
//! 实现流程：
//! 1. 正式版是 `panic = "abort"`、没有控制台：panic 那句话原来哪儿都看不到，程序直接没了。
//!    启动的第一步装一个 panic 钩子（`install`），钩子在退出之前执行。
//! 2. 记什么：时间（UTC）、版本号、线程名、出事的文件名和行号、panic 信息的前 200 个字符，一次一行。
//!    不记按键、队伍码、密钥、昵称、消息内容：钩子拿不到这些；信息截短是防依赖库把数据带进 panic 信息里。
//! 3. 数据目录要等 Tauri 起来才知道，所以钩子先装、目录后填（`set_dir`）；目录填上之前就 panic 的记不下来。
//!    填目录时顺便把文件截到最近 20 条，文件不会一直变大。
//! 4. 钩子里绝不能再 panic（进程会立刻异常终止，连这一条都记不下）：每一步失败都直接放弃，不拿锁、不碰 Tauri。
//! 5. 测试实例（`LOLGANG_INSTANCE=2/3`）设了 `LOLGANG_TEST_PANIC=1` 时，启动 5 秒后故意 panic 一次，
//!    用来在正式版构建里验证这条路；正式身份不认这个环境变量。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILE: &str = "crash.log";
const MAX_ENTRIES: usize = 20;
const MAX_MESSAGE_CHARS: usize = 200;

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// 装 panic 钩子。原来的钩子（往 stderr 打一行，开发时看得到）照旧执行。
pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = DIR.get() {
            let secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let location = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            let line = entry(
                secs,
                env!("CARGO_PKG_VERSION"),
                std::thread::current().name().unwrap_or("?"),
                &location,
                info.payload_as_str().unwrap_or(""),
            );
            append(dir, &line);
        }
        default(info);
    }));
}

/// 数据目录定下来了：以后的 panic 记到这里。先把旧记录截到最近 20 条。
pub fn set_dir(dir: &Path) {
    trim(dir);
    let _ = DIR.set(dir.to_path_buf());
}

/// 测试实例里按环境变量故意崩一次（文件头第 5 条）。
pub fn arm_test_panic(instance: Option<u16>) {
    let var = std::env::var("LOLGANG_TEST_PANIC").ok();
    if test_panic_wanted(instance, var.as_deref()) {
        let _ = std::thread::Builder::new()
            .name("test-panic".into())
            .spawn(|| {
                std::thread::sleep(Duration::from_secs(5));
                panic!("测试用的崩溃（LOLGANG_TEST_PANIC）");
            });
    }
}

/// 只有测试实例才认这个环境变量。
fn test_panic_wanted(instance: Option<u16>, var: Option<&str>) -> bool {
    instance.is_some() && var == Some("1")
}

fn append(dir: &Path, line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FILE))
    {
        let _ = writeln!(f, "{line}");
    }
}

fn trim(dir: &Path) {
    let path = dir.join(FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    if let Some(kept) = keep_last(&text, MAX_ENTRIES) {
        let _ = std::fs::write(&path, kept);
    }
}

/// 超过 `max` 行时返回最后 `max` 行；没超就是 None（不用重写文件）。
fn keep_last(text: &str, max: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    (lines.len() > max).then(|| {
        let mut kept = lines[lines.len() - max..].join("\n");
        kept.push('\n');
        kept
    })
}

/// 一条记录：一行。信息里的换行等控制字符换成空格，最多 200 个字符。
fn entry(unix_secs: u64, version: &str, thread: &str, location: &str, message: &str) -> String {
    let flat: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_MESSAGE_CHARS)
        .collect();
    format!("{} v{version} [{thread}] {location} {flat}", utc(unix_secs))
}

/// Unix 秒 → `2026-11-10T06:40:05Z`。只为这一处不值得加日期库：按天数算出年月日（公历，400 年一个周期）。
fn utc(unix_secs: u64) -> String {
    let (days, rem) = (unix_secs / 86_400, unix_secs % 86_400);
    // 把起点挪到 0000-03-01：闰日落在一年的最后一天，月份长度就有规律了
    let z = days + 719_468;
    let (era, doe) = (z / 146_097, z % 146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formats_known_moments() {
        assert_eq!(utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(utc(1_709_164_800), "2024-02-29T00:00:00Z"); // 闰日
        assert_eq!(utc(1_709_251_199), "2024-02-29T23:59:59Z");
        assert_eq!(utc(1_709_251_200), "2024-03-01T00:00:00Z");
    }

    #[test]
    fn entry_is_one_line_with_time_version_thread_location_message() {
        let e = entry(
            1_700_000_000,
            "3.3.0",
            "voice",
            "src\\voice\\mod.rs:42",
            "出错了",
        );
        assert_eq!(
            e,
            "2023-11-14T22:13:20Z v3.3.0 [voice] src\\voice\\mod.rs:42 出错了"
        );
    }

    #[test]
    fn entry_flattens_control_characters_and_cuts_the_message() {
        let e = entry(0, "1", "t", "f:1", "第一行\n第二行\t尾");
        assert!(e.ends_with("第一行 第二行 尾"));
        assert_eq!(e.lines().count(), 1);

        let long = "字".repeat(MAX_MESSAGE_CHARS + 50);
        let e = entry(0, "1", "t", "f:1", &long);
        assert_eq!(e.chars().filter(|c| *c == '字').count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn keep_last_only_rewrites_when_over_the_limit() {
        assert_eq!(keep_last("a\nb\n", 2), None);
        assert_eq!(keep_last("", 2), None);
        assert_eq!(keep_last("a\nb\nc\n", 2).as_deref(), Some("b\nc\n"));
        // 最后一行没有换行符也算一行
        assert_eq!(keep_last("a\nb\nc", 1).as_deref(), Some("c\n"));
    }

    #[test]
    fn append_then_trim_keeps_the_newest_entries() {
        let dir = std::env::temp_dir().join(format!("lolgang-crash-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join(FILE));
        for i in 0..MAX_ENTRIES + 3 {
            append(&dir, &format!("line {i}"));
        }
        trim(&dir);
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), MAX_ENTRIES);
        assert_eq!(lines[0], "line 3");
        assert_eq!(lines[MAX_ENTRIES - 1], format!("line {}", MAX_ENTRIES + 2));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_panic_is_only_for_test_instances() {
        assert!(test_panic_wanted(Some(2), Some("1")));
        assert!(test_panic_wanted(Some(3), Some("1")));
        assert!(!test_panic_wanted(None, Some("1"))); // 正式身份不认
        assert!(!test_panic_wanted(Some(2), None));
        assert!(!test_panic_wanted(Some(2), Some("0")));
    }
}
