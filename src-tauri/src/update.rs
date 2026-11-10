//! 自动更新（2.0 架构第 9 节）：用 `tauri-plugin-updater` 从本项目 GitHub Release 的 `latest.json` 查新版本。
//!
//! 实现流程：
//! 1. 什么时候查：启动后 30 秒、之后每 6 小时（设置里「自动检查更新」关了就不查）；托盘和「关于」页可以手动查。
//! 2. 查到新版本只提示：托盘那一项变成「更新到 vX」，透明窗口弹一次提示（同一个版本只弹一次），控制面板「关于」页显示版本号和更新说明。
//! 3. 用户点「更新并重启」才下载。下载的包先用 `tauri.conf.json` 里的公钥校验签名，签名里记的版本也要和清单一致
//!    （`requireSignedVersion`，防止拿旧版本的真包冒充新版本），不对就放弃并显示原因；装好后重启。
//! 4. 状态放在 `Updates` 里（一把锁），每次变化推 "update://changed" 给控制面板。
//! 5. 测试实例（`LOLGANG_INSTANCE=2/3`）不查更新、不装（3.2.0）：测试版的版本号可能比装好的旧，查到「新版本」一装，
//!    安装包会覆盖正式安装、关掉所有 lolgang.exe（2026-11-10 实际发生过）。

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::api::AppState;
use crate::overlay::Msg;

const FIRST_CHECK: Duration = Duration::from_secs(30);
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Installing,
    Failed,
}

#[derive(Default)]
struct Inner {
    phase: Option<Phase>,
    found: Option<Update>,
    error: Option<String>,
    /// 下载进度 0 到 1；不知道总大小时是 None。
    progress: Option<f64>,
    /// 已经弹过提示的版本。
    told: Option<String>,
}

#[derive(Default)]
pub struct Updates {
    inner: Mutex<Inner>,
}

/// 给控制面板的更新状态。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateView {
    pub current: String,
    pub phase: Phase,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub progress: Option<f64>,
    pub error: Option<String>,
}

fn view(app: &AppHandle) -> UpdateView {
    let current = app.package_info().version.to_string();
    let state = app.try_state::<AppState>();
    let inner = state
        .as_ref()
        .map(|s| s.updates.inner.lock().expect("更新锁没有中毒"));
    let (phase, version, notes, progress, error) = match &inner {
        Some(i) => (
            i.phase.unwrap_or(Phase::Idle),
            i.found.as_ref().map(|u| u.version.clone()),
            i.found.as_ref().and_then(|u| u.body.clone()),
            i.progress,
            i.error.clone(),
        ),
        None => (Phase::Idle, None, None, None, None),
    };
    UpdateView {
        current,
        phase,
        version,
        notes,
        progress,
        error,
    }
}

fn set(app: &AppHandle, f: impl FnOnce(&mut Inner)) {
    if let Some(state) = app.try_state::<AppState>() {
        f(&mut state.updates.inner.lock().expect("更新锁没有中毒"));
    }
    let _ = app.emit_to("main", "update://changed", view(app));
}

/// 查一次。`quiet`：自动检查，查到新版本才提示，出错不打扰（错误照样记下，「关于」页能看到）。
pub async fn check(app: &AppHandle, quiet: bool) -> UpdateView {
    let busy = app.try_state::<AppState>().is_some_and(|s| {
        matches!(
            s.updates.inner.lock().expect("更新锁没有中毒").phase,
            Some(Phase::Checking | Phase::Downloading | Phase::Installing)
        )
    });
    if busy {
        return view(app);
    }
    if crate::dev_instance().is_some() {
        set(app, |i| {
            i.phase = Some(Phase::Idle);
            i.error = Some("测试实例不检查更新".into());
        });
        return view(app);
    }
    set(app, |i| {
        i.phase = Some(Phase::Checking);
        i.error = None;
    });
    let result = match app.updater() {
        Ok(u) => u.check().await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    match result {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let mut first_time = false;
            set(app, |i| {
                first_time = i.told.as_deref() != Some(version.as_str());
                i.told = Some(version.clone());
                i.found = Some(update);
                i.phase = Some(Phase::Available);
            });
            crate::tray::set_update_label(app, Some(&version));
            if first_time && let Some(state) = app.try_state::<AppState>() {
                state.overlays.send(Msg::Toast {
                    title: format!("lolPing-rust 有新版本 {version}"),
                    body: "在控制面板的「关于」里更新".into(),
                });
            }
        }
        Ok(None) => {
            set(app, |i| {
                i.found = None;
                i.phase = Some(Phase::UpToDate);
            });
            crate::tray::set_update_label(app, None);
        }
        Err(e) => {
            // 插件对「清单取不到」只给一句英文：说成人话
            let reason = if e.contains("valid release JSON") {
                "GitHub 上还没有发布过新版本，或者连不上 GitHub".to_string()
            } else {
                e
            };
            set(app, |i| {
                i.phase = Some(Phase::Failed);
                i.error = Some(format!("检查更新失败：{reason}"));
            });
            if !quiet {
                tray_show_about(app);
            }
        }
    }
    view(app)
}

/// 启动后 30 秒查一次，之后每 6 小时查一次（设置关了就跳过这一次）。
pub fn start_schedule(app: &AppHandle) {
    if crate::dev_instance().is_some() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            let on = app
                .try_state::<AppState>()
                .is_some_and(|s| s.settings.lock().expect("设置锁没有中毒").auto_update_check);
            if on {
                check(&app, true).await;
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

/// 打开控制面板的「关于」页。
pub fn tray_show_about(app: &AppHandle) {
    crate::tray::show_page(app, Some("about"));
}

#[tauri::command]
pub fn update_view(app: AppHandle) -> UpdateView {
    view(&app)
}

#[tauri::command]
pub async fn update_check(app: AppHandle) -> UpdateView {
    check(&app, false).await
}

/// 下载、校验、安装，然后重启。出错时留在原版本，「关于」页显示原因。
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    let update = app
        .try_state::<AppState>()
        .and_then(|s| {
            s.updates
                .inner
                .lock()
                .expect("更新锁没有中毒")
                .found
                .clone()
        })
        .ok_or("还没有查到新版本")?;
    set(&app, |i| {
        i.phase = Some(Phase::Downloading);
        i.progress = Some(0.0);
        i.error = None;
    });
    let mut got: u64 = 0;
    let progress_app = app.clone();
    let done_app = app.clone();
    let result = update
        .download_and_install(
            move |chunk, total| {
                got += chunk as u64;
                let p = total
                    .filter(|t| *t > 0)
                    .map(|t| (got as f64 / t as f64).min(1.0));
                set(&progress_app, |i| i.progress = p);
            },
            move || set(&done_app, |i| i.phase = Some(Phase::Installing)),
        )
        .await;
    match result {
        Ok(()) => app.restart(),
        Err(e) => {
            let message = format!("更新没装上：{e}");
            set(&app, |i| {
                i.phase = Some(Phase::Failed);
                i.progress = None;
                i.error = Some(message.clone());
            });
            Err(message)
        }
    }
}

/// 验收第 10 条后半：签名不对的更新包不装。用的是真的更新插件和我们发版用的 `plugins.updater` 设置
/// （包括 requireSignedVersion），只把公钥换成测试用的临时密钥、允许本机的 http 地址；
/// 一个本机 HTTP 服务给 latest.json 和更新包。先确认签名对的能下载（证明这套测试是通的），再看三种不对的都被拒绝。
#[cfg(test)]
mod signature_tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use base64::Engine;
    use minisign::KeyPair;
    use serde_json::{Value, json};
    use tauri_plugin_updater::UpdaterExt;

    const PACKAGE: &[u8] = b"lolgang update package for the test";
    const TAMPERED: &[u8] = b"lolgang update package for the tesT";
    const TARGET: &str = "test-target";

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    /// Tauri 的签名格式：minisign 签名文件的文本再 base64；可信注释里记版本。
    fn sign(keys: &KeyPair, data: &[u8], version: &str) -> String {
        let trusted = format!("timestamp:1\tfile:pkg\tversion:{version}");
        let sig = minisign::sign(
            Some(&keys.pk),
            &keys.sk,
            data,
            Some(&trusted),
            Some("signature from tauri secret key"),
        )
        .unwrap();
        b64(&sig.into_string())
    }

    /// 本机 HTTP 服务：/latest.json 给清单（版本 `announced`、签名 `signature`），其他路径给 `package`。返回清单地址。
    fn serve(announced: &str, signature: String, package: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let manifest = json!({
            "version": announced,
            "notes": "测试",
            "pub_date": "2026-11-04T00:00:00Z",
            "platforms": { TARGET: { "url": format!("{base}/pkg"), "signature": signature } }
        })
        .to_string();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let body: Vec<u8> = if buf[..n].starts_with(b"GET /latest.json") {
                    manifest.clone().into_bytes()
                } else {
                    package.to_vec()
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        format!("{base}/latest.json")
    }

    /// 装着更新插件、用发版设置的模拟程序（版本 0.1.0，公钥换成 `trusted`）查一次、下载一次。
    fn download(
        trusted: &KeyPair,
        announced: &str,
        signature: String,
        package: &'static [u8],
    ) -> Result<Vec<u8>, String> {
        let shipped: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let mut updater = shipped["plugins"]["updater"].clone();
        assert_eq!(
            updater["requireSignedVersion"],
            json!(true),
            "发版设置要求签名里带版本"
        );
        updater["pubkey"] = json!(b64(&trusted.pk.to_box().unwrap().into_string()));
        updater["dangerousInsecureTransportProtocol"] = json!(true);
        let mut ctx = tauri::test::mock_context(tauri::test::noop_assets());
        ctx.config_mut().plugins.0.insert("updater".into(), updater);
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(ctx)
            .unwrap();
        let url = serve(announced, signature, package);
        tauri::async_runtime::block_on(async move {
            let update = app
                .updater_builder()
                .target(TARGET)
                .no_proxy()
                .endpoints(vec![url.parse().unwrap()])
                .map_err(|e| e.to_string())?
                .build()
                .map_err(|e| e.to_string())?
                .check()
                .await
                .map_err(|e| e.to_string())?
                .ok_or("没查到新版本")?;
            update
                .download(|_, _| {}, || {})
                .await
                .map_err(|e| e.to_string())
        })
    }

    #[test]
    fn only_correctly_signed_updates_are_accepted() {
        let ours = KeyPair::generate_unencrypted_keypair().unwrap();
        let theirs = KeyPair::generate_unencrypted_keypair().unwrap();

        let good = download(&ours, "9.9.9", sign(&ours, PACKAGE, "9.9.9"), PACKAGE);
        assert_eq!(good.as_deref(), Ok(PACKAGE), "签名对的要能下载：{good:?}");

        let tampered = download(&ours, "9.9.9", sign(&ours, PACKAGE, "9.9.9"), TAMPERED);
        assert!(tampered.is_err(), "包被改过：要拒绝");

        let old = download(&ours, "9.9.9", sign(&ours, PACKAGE, "2.0.0"), PACKAGE);
        assert!(old.is_err(), "拿旧版本的真包冒充新版本：要拒绝");

        let stranger = download(&ours, "9.9.9", sign(&theirs, PACKAGE, "9.9.9"), PACKAGE);
        assert!(stranger.is_err(), "别人的私钥签的：要拒绝");
    }
}
