//! 自动更新（2.0 架构第 9 节）：用 `tauri-plugin-updater` 查新版本、下载、安装。
//!
//! 实现流程：
//! 1. 什么时候查：启动后 30 秒、之后每 6 小时（设置里「自动检查更新」关了就不查）；托盘和「关于」页可以手动查。
//! 2. 去哪查（3.3.1，`docs/v3.3.1/requirements.md` 第 2 节）：两个更新源。
//!    GitHub：本项目 Releases 的 `latest.json`（`GITHUB_MANIFEST`，和 `tauri.conf.json` 里写的一样，测试钉住）。
//!    Gitee（国内，不用代理）：所有者把 GitHub Releases 的四个文件原样传到 Gitee 的同名标签下。Gitee 没有「最新版本」的固定下载地址，
//!    所以先问接口 `GITEE_LATEST_API` 最新的发行版是哪个、取它附件里的 `latest.json`；清单里的下载地址是 GitHub 的，
//!    查到新版本后换成 Gitee 上的同名文件（`to_gitee`）。这两个地址编译进了程序，以后不能改。
//!    「关于」页的「检查更新」只查 GitHub、「国内检查更新」只查 Gitee（所有者的思路）；自动检查和托盘先查 GitHub，
//!    查不到（连不上、超时、出错）才查 Gitee。GitHub 拿到了清单（哪怕说没有新版本）就以它为准、不再问 Gitee：Gitee 是手动传的，可能还没传。
//!    每一边最多等 15 秒。记下这次实际来自哪边，「关于」页显示。
//! 3. 查到新版本只提示：托盘那一项变成「更新到 vX」，透明窗口弹一次提示（同一个版本只弹一次），「关于」页显示版本号、更新说明和来源。
//! 4. 用户点「更新并重启」才下载，从查到它的那边下。下载最多 10 分钟（插件自己不给下载设超时，卡住会一直等）：
//!    到时停下，留在原版本，提示可以改用 Gitee。下载完用 `tauri.conf.json` 里的公钥校验签名，签名里记的版本也要和清单一致
//!    （`requireSignedVersion`，防止拿旧版本的真包冒充新版本）。哪边下载的都一样校验，Gitee 上的文件被换掉也装不进来。装好后重启。
//! 5. 状态放在 `Updates` 里（一把锁），每次变化推 "update://changed" 给控制面板。
//! 6. 测试实例（`LOLGANG_INSTANCE=2/3`）不查更新、不装（3.2.0）：测试版的版本号可能比装好的旧，查到「新版本」一装，
//!    安装包会覆盖正式安装（2026-11-10 实际发生过）。3.3.1 起为了对着真实的 GitHub、Gitee 实测：测试实例设了
//!    `LOLGANG_TEST_UPDATE=1` 就查、下载、校验签名，但下载完就停、**永远不装**；再设 `LOLGANG_TEST_UPDATE_NO_GITHUB=1`，
//!    GitHub 换成本机连不上的地址，用来在开着代理的电脑上测「GitHub 查不到 → Gitee」。正式身份不认这两个变量。

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime, Url};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::api::AppState;
use crate::overlay::Msg;

const FIRST_CHECK: Duration = Duration::from_secs(30);
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// GitHub 的更新清单（和 `tauri.conf.json` 的 `plugins.updater.endpoints` 一样）。
const GITHUB_MANIFEST: &str =
    "https://github.com/lijianqiao/lolPing-rust/releases/latest/download/latest.json";
/// Gitee 接口：本项目最新的发行版。编译进程序，以后不能改。
const GITEE_LATEST_API: &str =
    "https://gitee.com/api/v5/repos/lijianqiao/lolPing-rust/releases/latest";
/// 清单里 GitHub 的下载地址前缀，换成 Gitee 的（文件名不变）。
const GITHUB_DOWNLOADS: &str = "https://github.com/lijianqiao/lolPing-rust/releases/download/";
const GITEE_DOWNLOADS: &str = "https://gitee.com/lijianqiao/lolPing-rust/releases/download/";
/// 测试实例模拟「连不上 GitHub」用的地址：本机的 9 号端口没人听，连接直接被拒。
const NO_GITHUB: &str = "https://127.0.0.1:9/latest.json";
/// 查每一边最多等多久。
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// 下载最多多久：43 MB 在每秒 73 KB 以上的网速下载得完，比这还慢就该换一边了。
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

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

/// 这次的结果来自哪个更新源。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Github,
    Gitee,
}

/// 去哪查（文件头第 2 条）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 「关于」页的「检查更新」。
    Github,
    /// 「关于」页的「国内检查更新」。
    Gitee,
    /// 自动检查和托盘：先 GitHub，查不到才 Gitee。
    GithubThenGitee,
}

#[derive(Default)]
struct Inner {
    phase: Option<Phase>,
    found: Option<Update>,
    /// 查到的结果（有新版本或已是最新）来自哪边。
    source: Option<Source>,
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
    /// 这次实际来自哪边（GitHub、Gitee）；没查过、查失败时是 null。
    pub source: Option<Source>,
}

fn view(app: &AppHandle) -> UpdateView {
    let current = app.package_info().version.to_string();
    let state = app.try_state::<AppState>();
    let inner = state
        .as_ref()
        .map(|s| s.updates.inner.lock().expect("更新锁没有中毒"));
    match &inner {
        Some(i) => UpdateView {
            current,
            phase: i.phase.unwrap_or(Phase::Idle),
            version: i.found.as_ref().map(|u| u.version.clone()),
            notes: i.found.as_ref().and_then(|u| u.body.clone()),
            progress: i.progress,
            error: i.error.clone(),
            source: i.source,
        },
        None => UpdateView {
            current,
            phase: Phase::Idle,
            version: None,
            notes: None,
            progress: None,
            error: None,
            source: None,
        },
    }
}

fn set(app: &AppHandle, f: impl FnOnce(&mut Inner)) {
    if let Some(state) = app.try_state::<AppState>() {
        f(&mut state.updates.inner.lock().expect("更新锁没有中毒"));
    }
    let _ = app.emit_to("main", "update://changed", view(app));
}

/// 去哪查、怎么查。程序里用 `shipped`；测试换成本机地址、缩短超时。
struct Sources {
    github: Url,
    gitee_api: Url,
    timeout: Duration,
    /// 测试用：指定平台，不走系统代理。
    test_target: Option<String>,
}

impl Sources {
    fn shipped() -> Self {
        // 测试实例加 LOLGANG_TEST_UPDATE_NO_GITHUB=1 时，GitHub 换成本机一个连不上的地址（文件头第 6 条）
        let github = if test_update().is_some_and(|t| t.no_github) {
            NO_GITHUB
        } else {
            GITHUB_MANIFEST
        };
        Sources {
            github: Url::parse(github).expect("常量地址合法"),
            gitee_api: Url::parse(GITEE_LATEST_API).expect("常量地址合法"),
            timeout: CHECK_TIMEOUT,
            test_target: None,
        }
    }
}

/// 插件对常见失败只给一句英文：说成人话。
fn plain(e: &str) -> String {
    if e.contains("valid release JSON") {
        crate::tr!(
            "没有拿到可用的更新清单",
            "Didn't get a usable update manifest"
        )
        .into()
    } else if e.contains("timed out") || e.contains("operation timed out") {
        crate::tr!("等了 15 秒没有回应", "No response after 15 seconds").into()
    } else if e.contains("error sending request") {
        crate::tr!("连接失败", "Connection failed").into()
    } else {
        e.to_string()
    }
}

/// 测试实例实测更新（文件头第 6 条）：`LOLGANG_TEST_UPDATE=1` 时测试实例也查更新、下载、校验签名，但永远不装；
/// 再加 `LOLGANG_TEST_UPDATE_NO_GITHUB=1` 时 GitHub 换成连不上的地址。正式身份不认这两个变量。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TestUpdate {
    no_github: bool,
}

fn test_update() -> Option<TestUpdate> {
    test_update_from(
        crate::dev_instance(),
        std::env::var("LOLGANG_TEST_UPDATE").ok().as_deref(),
        std::env::var("LOLGANG_TEST_UPDATE_NO_GITHUB")
            .ok()
            .as_deref(),
    )
}

fn test_update_from(
    instance: Option<u16>,
    update: Option<&str>,
    no_github: Option<&str>,
) -> Option<TestUpdate> {
    (instance.is_some() && update == Some("1")).then_some(TestUpdate {
        no_github: no_github == Some("1"),
    })
}

/// 用一份清单查一次。
async fn check_manifest<R: Runtime>(
    app: &AppHandle<R>,
    src: &Sources,
    manifest: Url,
) -> Result<Option<Update>, String> {
    let mut builder = app.updater_builder().timeout(src.timeout);
    if let Some(target) = &src.test_target {
        builder = builder.target(target.clone()).no_proxy();
    }
    builder
        .endpoints(vec![manifest])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| plain(&e.to_string()))
}

/// Gitee 接口返回的发行版里，`latest.json` 附件的下载地址。
fn manifest_url_from_release(json: &str) -> Result<Url, String> {
    #[derive(Deserialize)]
    struct Release {
        assets: Vec<Asset>,
    }
    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }
    let release: Release = serde_json::from_str(json).map_err(|_| {
        crate::tr!(
            "Gitee 返回的不是发行版信息",
            "Gitee didn't return release information"
        )
        .to_string()
    })?;
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name == "latest.json")
        .ok_or(crate::tr!(
            "Gitee 上最新的发行版里没有 latest.json",
            "The latest release on Gitee has no latest.json"
        ))?;
    Url::parse(&asset.browser_download_url).map_err(|_| {
        crate::tr!(
            "Gitee 上 latest.json 的地址不对",
            "The latest.json address on Gitee is invalid"
        )
        .to_string()
    })
}

/// 清单里 GitHub 的下载地址换成 Gitee 上的同名文件；本来就不是 GitHub 的不动。
fn to_gitee(url: &Url) -> Url {
    url.as_str()
        .strip_prefix(GITHUB_DOWNLOADS)
        .and_then(|rest| Url::parse(&format!("{GITEE_DOWNLOADS}{rest}")).ok())
        .unwrap_or_else(|| url.clone())
}

/// 问 Gitee 接口最新的发行版，取它的 `latest.json` 地址。
async fn gitee_manifest(src: &Sources) -> Result<Url, String> {
    // 和更新插件一样：还没有全局的加密算法提供者就装 ring（依赖里只有它）
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let mut client = reqwest::Client::builder()
        .timeout(src.timeout)
        .user_agent("lolPing-updater");
    if src.test_target.is_some() {
        client = client.no_proxy();
    }
    let text = client
        .build()
        .map_err(|e| e.to_string())?
        .get(src.gitee_api.clone())
        .send()
        .await
        .map_err(|e| plain(&e.to_string()))?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    manifest_url_from_release(&text)
}

async fn check_gitee<R: Runtime>(
    app: &AppHandle<R>,
    src: &Sources,
) -> Result<Option<Update>, String> {
    let manifest = gitee_manifest(src).await?;
    Ok(check_manifest(app, src, manifest).await?.map(|mut u| {
        u.download_url = to_gitee(&u.download_url);
        u
    }))
}

/// 按 `mode` 去查（文件头第 2 条）。成功时给出结果和来源；失败时给一句给人看的原因。
async fn check_sources<R: Runtime>(
    app: &AppHandle<R>,
    src: &Sources,
    mode: Mode,
) -> Result<(Option<Update>, Source), String> {
    match mode {
        Mode::Github => check_manifest(app, src, src.github.clone())
            .await
            .map(|u| (u, Source::Github))
            .map_err(|e| {
                crate::tr!(
                    format!("连不上 GitHub（{e}）。国内网络可以点「国内检查更新」"),
                    format!("Can't reach GitHub ({e}). In mainland China, try “Check via Gitee”")
                )
            }),
        Mode::Gitee => check_gitee(app, src)
            .await
            .map(|u| (u, Source::Gitee))
            .map_err(|e| {
                crate::tr!(
                    format!("Gitee 查不到（{e}）"),
                    format!("Gitee check failed ({e})")
                )
            }),
        Mode::GithubThenGitee => match check_manifest(app, src, src.github.clone()).await {
            Ok(u) => Ok((u, Source::Github)),
            Err(github) => check_gitee(app, src)
                .await
                .map(|u| (u, Source::Gitee))
                .map_err(|gitee| {
                    crate::tr!(
                        format!("GitHub 和 Gitee 都查不到。GitHub：{github}；Gitee：{gitee}"),
                        format!("Neither GitHub nor Gitee could be checked. GitHub: {github}; Gitee: {gitee}")
                    )
                }),
        },
    }
}

/// 下载失败的两种：超时（文件头第 4 条），别的错误（包括签名不对）。
#[derive(Debug, PartialEq, Eq)]
enum DownloadError {
    TimedOut,
    Failed(String),
}

/// 下载并校验签名，最多等 `limit`；超时就停下（丢掉下载的任务，连接随之断开）。
async fn download_within(
    update: &Update,
    limit: Duration,
    on_chunk: impl FnMut(usize, Option<u64>),
) -> Result<Vec<u8>, DownloadError> {
    match tokio::time::timeout(limit, update.download(on_chunk, || {})).await {
        Err(_) => Err(DownloadError::TimedOut),
        Ok(Err(e)) => Err(DownloadError::Failed(e.to_string())),
        Ok(Ok(bytes)) => Ok(bytes),
    }
}

/// 查一次。`quiet`：自动检查，查到新版本才提示，出错不打扰（错误照样记下，「关于」页能看到）。
pub async fn check(app: &AppHandle, quiet: bool, mode: Mode) -> UpdateView {
    let busy = app.try_state::<AppState>().is_some_and(|s| {
        matches!(
            s.updates.inner.lock().expect("更新锁没有中毒").phase,
            Some(Phase::Checking | Phase::Downloading | Phase::Installing)
        )
    });
    if busy {
        return view(app);
    }
    if crate::dev_instance().is_some() && test_update().is_none() {
        set(app, |i| {
            i.phase = Some(Phase::Idle);
            i.error = Some(
                crate::tr!(
                    "测试实例不检查更新",
                    "Test instances don't check for updates"
                )
                .into(),
            );
        });
        return view(app);
    }
    set(app, |i| {
        i.phase = Some(Phase::Checking);
        i.error = None;
    });
    match check_sources(app, &Sources::shipped(), mode).await {
        Ok((Some(update), source)) => {
            let version = update.version.clone();
            let mut first_time = false;
            set(app, |i| {
                first_time = i.told.as_deref() != Some(version.as_str());
                i.told = Some(version.clone());
                i.found = Some(update);
                i.source = Some(source);
                i.phase = Some(Phase::Available);
            });
            crate::tray::set_update_label(app, Some(&version));
            if first_time && let Some(state) = app.try_state::<AppState>() {
                state.overlays.send(Msg::Toast {
                    title: crate::tr!(
                        format!("lolPing 有新版本 {version}"),
                        format!("lolPing {version} is available")
                    ),
                    body: crate::tr!(
                        "在控制面板的「关于」里更新",
                        "Update from About in the control panel"
                    )
                    .into(),
                });
            }
        }
        Ok((None, source)) => {
            set(app, |i| {
                i.found = None;
                i.source = Some(source);
                i.phase = Some(Phase::UpToDate);
            });
            crate::tray::set_update_label(app, None);
        }
        Err(reason) => {
            set(app, |i| {
                i.phase = Some(Phase::Failed);
                i.source = None;
                i.error = Some(crate::tr!(
                    format!("检查更新失败：{reason}"),
                    format!("Update check failed: {reason}")
                ));
            });
            if !quiet {
                tray_show_about(app);
            }
        }
    }
    view(app)
}

/// 启动后 30 秒查一次，之后每 6 小时查一次（设置关了就跳过这一次）。先 GitHub，查不到才 Gitee。
pub fn start_schedule(app: &AppHandle) {
    if crate::dev_instance().is_some() && test_update().is_none() {
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
                check(&app, true, Mode::GithubThenGitee).await;
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

/// 「关于」页的「检查更新」：只查 GitHub。
#[tauri::command]
pub async fn update_check(app: AppHandle) -> UpdateView {
    check(&app, false, Mode::Github).await
}

/// 「关于」页的「国内检查更新」：只查 Gitee。
#[tauri::command]
pub async fn update_check_cn(app: AppHandle) -> UpdateView {
    check(&app, false, Mode::Gitee).await
}

/// 下载、校验、安装，然后重启。出错时留在原版本，「关于」页显示原因。
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    let (update, source) = app
        .try_state::<AppState>()
        .and_then(|s| {
            let inner = s.updates.inner.lock().expect("更新锁没有中毒");
            inner.found.clone().map(|u| (u, inner.source))
        })
        .ok_or(crate::tr!("还没有查到新版本", "No new version found yet"))?;
    set(&app, |i| {
        i.phase = Some(Phase::Downloading);
        i.progress = Some(0.0);
        i.error = None;
    });
    let mut got: u64 = 0;
    let progress_app = app.clone();
    let downloaded = download_within(&update, DOWNLOAD_TIMEOUT, move |chunk, total| {
        got += chunk as u64;
        let p = total
            .filter(|t| *t > 0)
            .map(|t| (got as f64 / t as f64).min(1.0));
        set(&progress_app, |i| i.progress = p);
    })
    .await;
    let result = match downloaded {
        // 测试实例（LOLGANG_TEST_UPDATE=1）：下载完、签名对了就停，永远不装（文件头第 6 条）
        Ok(bytes) if crate::dev_instance().is_some() => {
            let (n, from) = (
                bytes.len(),
                if source == Some(Source::Gitee) { "Gitee" } else { "GitHub" },
            );
            Err(crate::tr!(
                format!("测试实例不安装：已下载 {n} 字节，签名校验通过（来自 {from}）"),
                format!("Test instance, not installing: downloaded {n} bytes, signature verified (from {from})")
            ))
        }
        Ok(bytes) => {
            set(&app, |i| i.phase = Some(Phase::Installing));
            update
                .install(bytes)
                .map_err(|e| crate::tr!(format!("更新没装上：{e}"), format!("The update wasn't installed: {e}")))
        }
        Err(DownloadError::TimedOut) => Err(match source {
            Some(Source::Gitee) => crate::tr!("下载太慢或卡住了（超过 10 分钟），已经停下，请稍后再试", "The download was too slow or stuck (over 10 minutes) and was stopped. Please try again later").to_string(),
            _ => crate::tr!("下载太慢或卡住了（超过 10 分钟），已经停下。国内网络可以点「国内检查更新」，从 Gitee 下载", "The download was too slow or stuck (over 10 minutes) and was stopped. In mainland China, try “Check via Gitee” to download from Gitee").to_string(),
        }),
        Err(DownloadError::Failed(e)) => Err(crate::tr!(format!("更新没装上：{e}"), format!("The update wasn't installed: {e}"))),
    };
    match result {
        Ok(()) => app.restart(),
        Err(message) => {
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

/// 3.3.1：两个更新源（文件头第 2、4 条）。用真的更新插件和一个本机 HTTP 服务，GitHub、Gitee 都换成本机地址、超时缩到几百毫秒；
/// 不碰网络、不碰本机安装。
#[cfg(test)]
mod source_tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use minisign::KeyPair;
    use serde_json::{Value, json};

    use super::*;

    const TARGET: &str = "test-target";
    const FAST: Duration = Duration::from_millis(400);

    /// 一个路径怎么回。
    #[derive(Clone)]
    enum Reply {
        /// 更新清单：这个版本，下载地址是本机的 /pkg。
        Manifest(&'static str),
        /// 更新清单：这个版本，下载地址是给定的（像 Gitee 上原样传的那份，指向 GitHub）。
        ManifestAt(&'static str, &'static str),
        /// 原样回一段 JSON（Gitee 接口）。
        Json(String),
        Status(u16),
        /// 收了请求一直不回。
        Hang,
        /// 回头和一半的包，然后不动了（下载卡住）。
        Partial,
    }

    struct Server {
        base: String,
        hits: Arc<Mutex<Vec<String>>>,
    }

    impl Server {
        fn hits(&self, path: &str) -> usize {
            self.hits
                .lock()
                .unwrap()
                .iter()
                .filter(|p| *p == path)
                .count()
        }
    }

    /// 本机 HTTP 服务：`routes` 按路径决定怎么回，没列的回 404。
    fn serve(routes: Vec<(&'static str, Reply)>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let (b, h) = (base.clone(), hits.clone());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let (b, h, routes) = (b.clone(), h.clone(), routes.clone());
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let line = String::from_utf8_lossy(&buf[..n]);
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    h.lock().unwrap().push(path.clone());
                    let reply = routes
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, r)| r.clone());
                    let ok = |body: Vec<u8>| {
                        let mut out = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        out.extend(body);
                        out
                    };
                    let out = match reply {
                        Some(Reply::Manifest(version)) => ok(json!({
                            "version": version,
                            "notes": "测试",
                            "pub_date": "2026-11-12T00:00:00Z",
                            "platforms": { TARGET: { "url": format!("{b}/pkg"), "signature": "c2ln" } }
                        })
                        .to_string()
                        .into_bytes()),
                        Some(Reply::ManifestAt(version, url)) => ok(json!({
                            "version": version,
                            "notes": "测试",
                            "pub_date": "2026-11-12T00:00:00Z",
                            "platforms": { TARGET: { "url": url, "signature": "c2ln" } }
                        })
                        .to_string()
                        .into_bytes()),
                        Some(Reply::Json(s)) => ok(s.into_bytes()),
                        Some(Reply::Status(code)) => {
                            format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                                .into_bytes()
                        }
                        Some(Reply::Hang) => {
                            std::thread::sleep(Duration::from_secs(5));
                            return;
                        }
                        Some(Reply::Partial) => {
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\nhalf",
                            );
                            std::thread::sleep(Duration::from_secs(5));
                            return;
                        }
                        None => b"HTTP/1.1 404 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    };
                    let _ = stream.write_all(&out);
                });
            }
        });
        Server { base, hits }
    }

    /// 一个连不上的地址（占一个端口再放掉）。
    fn refused() -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/latest.json", l.local_addr().unwrap())
    }

    /// Gitee 接口的回答：最新发行版的附件里有 latest.json。
    fn gitee_release(manifest: &str) -> Reply {
        Reply::Json(
            json!({ "tag_name": "v9.9.9", "assets": [
                { "name": "lolping_9.9.9_x64-setup.exe", "browser_download_url": "http://127.0.0.1:1/x" },
                { "name": "latest.json", "browser_download_url": manifest },
            ]})
            .to_string(),
        )
    }

    /// 装着更新插件、用发版设置的模拟程序（版本 0.1.0）。
    fn app() -> tauri::AppHandle<tauri::test::MockRuntime> {
        let shipped: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let mut updater = shipped["plugins"]["updater"].clone();
        let keys = KeyPair::generate_unencrypted_keypair().unwrap();
        use base64::Engine;
        updater["pubkey"] = json!(
            base64::engine::general_purpose::STANDARD
                .encode(keys.pk.to_box().unwrap().into_string())
        );
        updater["dangerousInsecureTransportProtocol"] = json!(true);
        let mut ctx = tauri::test::mock_context(tauri::test::noop_assets());
        ctx.config_mut().plugins.0.insert("updater".into(), updater);
        tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(ctx)
            .unwrap()
            .handle()
            .clone()
    }

    fn sources(github: String, gitee_api: String) -> Sources {
        Sources {
            github: github.parse().unwrap(),
            gitee_api: gitee_api.parse().unwrap(),
            timeout: FAST,
            test_target: Some(TARGET.into()),
        }
    }

    fn run(src: &Sources, mode: Mode) -> Result<(Option<String>, Source), String> {
        let app = app();
        tauri::async_runtime::block_on(check_sources(&app, src, mode))
            .map(|(u, s)| (u.map(|u| u.version), s))
    }

    #[test]
    fn github_answers_first_and_gitee_is_not_asked() {
        let s = serve(vec![
            ("/gh.json", Reply::Manifest("9.9.9")),
            ("/api", Reply::Status(500)),
        ]);
        let src = sources(format!("{}/gh.json", s.base), format!("{}/api", s.base));
        let got = run(&src, Mode::GithubThenGitee);
        assert_eq!(got, Ok((Some("9.9.9".into()), Source::Github)));
        assert_eq!(s.hits("/api"), 0);
    }

    #[test]
    fn github_saying_up_to_date_still_wins_over_gitee() {
        // Gitee 上有更新的版本也不问：GitHub 拿到了清单就以它为准
        let s = serve(vec![
            ("/gh.json", Reply::Manifest("0.0.1")),
            ("/api", gitee_release("http://127.0.0.1:1/never")),
        ]);
        let src = sources(format!("{}/gh.json", s.base), format!("{}/api", s.base));
        assert_eq!(run(&src, Mode::GithubThenGitee), Ok((None, Source::Github)));
        assert_eq!(s.hits("/api"), 0);
    }

    #[test]
    fn github_unreachable_erroring_or_silent_falls_back_to_gitee() {
        for github in ["refused", "500", "hang"] {
            let s = serve(vec![
                ("/gh500", Reply::Status(500)),
                ("/ghhang", Reply::Hang),
                ("/gitee.json", Reply::Manifest("9.9.9")),
            ]);
            let s2 = serve(vec![(
                "/api",
                gitee_release(&format!("{}/gitee.json", s.base)),
            )]);
            let gh = match github {
                "refused" => refused(),
                "500" => format!("{}/gh500", s.base),
                _ => format!("{}/ghhang", s.base),
            };
            let src = sources(gh, format!("{}/api", s2.base));
            let started = Instant::now();
            let got = run(&src, Mode::GithubThenGitee);
            assert_eq!(
                got,
                Ok((Some("9.9.9".into()), Source::Gitee)),
                "GitHub {github}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "GitHub {github} 等太久了"
            );
            assert_eq!(s.hits("/gitee.json"), 1);
        }
    }

    #[test]
    fn github_only_mode_does_not_fall_back_and_points_to_the_cn_button() {
        let s = serve(vec![("/api", gitee_release("http://127.0.0.1:1/never"))]);
        let src = sources(refused(), format!("{}/api", s.base));
        let err = run(&src, Mode::Github).unwrap_err();
        assert!(err.contains("国内检查更新"), "{err}");
        assert_eq!(s.hits("/api"), 0);
    }

    #[test]
    fn gitee_only_mode_skips_github() {
        let s = serve(vec![
            ("/gh.json", Reply::Manifest("9.9.9")),
            ("/gitee.json", Reply::Manifest("9.9.8")),
        ]);
        let s2 = serve(vec![(
            "/api",
            gitee_release(&format!("{}/gitee.json", s.base)),
        )]);
        let src = sources(format!("{}/gh.json", s.base), format!("{}/api", s2.base));
        assert_eq!(
            run(&src, Mode::Gitee),
            Ok((Some("9.9.8".into()), Source::Gitee))
        );
        assert_eq!(s.hits("/gh.json"), 0);
    }

    #[test]
    fn both_failing_reports_both_reasons() {
        let s = serve(vec![("/api", Reply::Status(503))]);
        let src = sources(refused(), format!("{}/api", s.base));
        let err = run(&src, Mode::GithubThenGitee).unwrap_err();
        assert!(err.contains("GitHub：") && err.contains("Gitee："), "{err}");
    }

    #[test]
    fn broken_gitee_answers_are_failures_not_panics() {
        for (reply, expect) in [
            (
                Reply::Json(r#"{"tag_name":"v1","assets":[]}"#.into()),
                "没有 latest.json",
            ),
            (Reply::Json("<html>维护中</html>".into()), "不是发行版信息"),
            (
                Reply::Json(
                    r#"{"assets":[{"name":"latest.json","browser_download_url":"不是地址"}]}"#
                        .into(),
                ),
                "地址不对",
            ),
            (Reply::Status(404), "404"),
        ] {
            let s = serve(vec![("/api", reply)]);
            let src = sources(refused(), format!("{}/api", s.base));
            let err = run(&src, Mode::Gitee).unwrap_err();
            assert!(err.contains(expect), "想要「{expect}」，实际：{err}");
        }
    }

    #[test]
    fn gitee_manifest_address_comes_from_the_latest_release() {
        let url = manifest_url_from_release(
            r#"{"tag_name":"v3.3.0","assets":[{"name":"lolping_3.3.0_x64-setup.exe","browser_download_url":"https://gitee.com/a/b/releases/download/v3.3.0/lolping_3.3.0_x64-setup.exe"},{"name":"latest.json","browser_download_url":"https://gitee.com/lijianqiao/lolPing-rust/releases/download/v3.3.0/latest.json"}]}"#,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://gitee.com/lijianqiao/lolPing-rust/releases/download/v3.3.0/latest.json"
        );
    }

    #[test]
    fn github_download_addresses_become_gitee_ones() {
        let gh = Url::parse(
            "https://github.com/lijianqiao/lolPing-rust/releases/download/v3.3.1/lolping_3.3.1_x64-setup.exe",
        )
        .unwrap();
        assert_eq!(
            to_gitee(&gh).as_str(),
            "https://gitee.com/lijianqiao/lolPing-rust/releases/download/v3.3.1/lolping_3.3.1_x64-setup.exe"
        );
        // 已经是 Gitee 的、别人仓库的、别的地址：都不动
        for other in [
            "https://gitee.com/lijianqiao/lolPing-rust/releases/download/v3.3.1/x.exe",
            "https://github.com/someone/else/releases/download/v1/x.exe",
            "http://127.0.0.1:8080/pkg",
        ] {
            let u = Url::parse(other).unwrap();
            assert_eq!(to_gitee(&u), u);
        }
    }

    /// 所有者原样传到 Gitee 的 latest.json 里，下载地址指向 GitHub：经 Gitee 查到的要换成 Gitee 的；经 GitHub 查到的不换。
    #[test]
    fn updates_found_on_gitee_download_from_gitee() {
        const GH_PKG: &str = "https://github.com/lijianqiao/lolPing-rust/releases/download/v9.9.9/lolping_9.9.9_x64-setup.exe";
        let s = serve(vec![("/m.json", Reply::ManifestAt("9.9.9", GH_PKG))]);
        let s2 = serve(vec![("/api", gitee_release(&format!("{}/m.json", s.base)))]);
        let src = sources(format!("{}/m.json", s.base), format!("{}/api", s2.base));
        let app = app();
        let url_via = |mode| {
            tauri::async_runtime::block_on(check_sources(&app, &src, mode))
                .unwrap()
                .0
                .unwrap()
                .download_url
                .to_string()
        };
        assert_eq!(
            url_via(Mode::Gitee),
            "https://gitee.com/lijianqiao/lolPing-rust/releases/download/v9.9.9/lolping_9.9.9_x64-setup.exe"
        );
        assert_eq!(url_via(Mode::Github), GH_PKG);
    }

    /// 插件和 reqwest 的英文报错说成人话（实测时「关于」页显示的是一长串英文和本机地址）。
    #[test]
    fn common_failures_read_as_plain_chinese() {
        assert_eq!(
            plain("error sending request for url (https://127.0.0.1:9/latest.json)"),
            "连接失败"
        );
        assert_eq!(
            plain("Could not fetch a valid release JSON from the remote"),
            "没有拿到可用的更新清单"
        );
        assert_eq!(plain("operation timed out"), "等了 15 秒没有回应");
        assert_eq!(plain("别的原因"), "别的原因");
    }

    #[test]
    fn test_update_switches_are_only_for_test_instances() {
        assert_eq!(
            test_update_from(None, Some("1"), Some("1")),
            None,
            "正式身份不认"
        );
        assert_eq!(test_update_from(Some(2), None, Some("1")), None);
        assert_eq!(test_update_from(Some(2), Some("0"), None), None);
        assert_eq!(
            test_update_from(Some(3), Some("1"), None),
            Some(TestUpdate { no_github: false })
        );
        assert_eq!(
            test_update_from(Some(2), Some("1"), Some("1")),
            Some(TestUpdate { no_github: true })
        );
        // 正式身份下 GitHub 用真地址
        assert_eq!(Sources::shipped().github.as_str(), GITHUB_MANIFEST);
    }

    #[test]
    fn github_address_matches_the_shipped_config() {
        let shipped: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            shipped["plugins"]["updater"]["endpoints"][0],
            json!(GITHUB_MANIFEST)
        );
        // 编译进程序的地址都合法
        let s = Sources::shipped();
        assert_eq!(s.timeout, Duration::from_secs(15));
        assert!(
            s.gitee_api
                .as_str()
                .starts_with("https://gitee.com/api/v5/repos/lijianqiao/lolPing-rust/")
        );
    }

    #[test]
    fn a_stalled_download_stops_at_the_time_limit() {
        let s = serve(vec![
            ("/gh.json", Reply::Manifest("9.9.9")),
            ("/pkg", Reply::Partial),
        ]);
        let src = sources(format!("{}/gh.json", s.base), refused());
        let app = app();
        let result = tauri::async_runtime::block_on(async {
            let (update, _) = check_sources(&app, &src, Mode::Github).await.unwrap();
            let update = update.expect("查到新版本");
            let started = Instant::now();
            let r = download_within(&update, FAST, |_, _| {}).await;
            (r, started.elapsed())
        });
        assert_eq!(result.0, Err(DownloadError::TimedOut));
        assert!(result.1 < Duration::from_secs(3), "{:?}", result.1);
    }
}
