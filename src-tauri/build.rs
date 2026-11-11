/// 所有 Rust 命令（和 lib.rs 的 `generate_handler!` 一一对应）。列在这里，每个命令就有自己的权限 `allow-命令名`
/// （下划线换成连字符），哪个窗口能调哪个命令由 capabilities/ 里的文件给；没给的窗口调了会被拒（3.2.0）。
const COMMANDS: &[&str] = &[
    "settings_read",
    "settings_write",
    "status_get",
    "pings_set_enabled",
    "input_suspend",
    "ping_preview",
    "overlay_hello",
    "overlay_idle",
    "emote_import",
    "emote_remove",
    "emote_preview",
    "open_accessibility_settings",
    "app_relaunch",
    "voice_view",
    "voice_test",
    "history_list",
    "history_play",
    "history_stop",
    "text_preview",
    "vote_view",
    "vote_start",
    "vote_answer",
    "text_send",
    "text_cancel",
    "update_view",
    "update_check",
    "update_check_cn",
    "update_install",
    "team_view",
    "team_create",
    "team_join",
    "team_cancel",
    "team_leave",
    "team_dissolve",
    "team_rename",
    "profile_set",
];

fn main() {
    let attributes = tauri_build::Attributes::new()
        .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS));
    // Windows：程序要带 Common Controls v6 的清单（windows/app-manifest.xml，抄自 tauri-build 的默认清单）。
    // tauri-build 默认只把它放进程序本身，测试程序没有；用 tauri::test 的单元测试没有它就启动不了
    // （STATUS_ENTRYPOINT_NOT_FOUND）。所以不让 tauri-build 放，改由 MSVC 链接器给所有目标放同一份。
    let msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if !msvc {
        tauri_build::try_build(attributes).expect("tauri-build 失败");
        return;
    }
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo 给了 CARGO_MANIFEST_DIR");
    let manifest = std::path::Path::new(&dir)
        .join("windows")
        .join("app-manifest.xml");
    println!("cargo:rerun-if-changed=windows/app-manifest.xml");
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
    let attributes =
        attributes.windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
    tauri_build::try_build(attributes).expect("tauri-build 失败");
}
