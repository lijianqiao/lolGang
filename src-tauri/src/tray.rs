//! 托盘（需求 4.8）：单击图标打开控制面板；菜单是「信号开启」（勾选）、「打开控制面板」、
//! 「检查更新」（查到新版本时变成「更新到 vX」）、「退出」。
//! 只在 Rust 侧，不给前端开权限。

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, Window, Wry,
};

use crate::api;
use crate::settings2::PanelRect;

const TRAY_ID: &str = "main";
const TOOLTIP: &str = "lolPing-rust";

/// 「信号开启」这一项，开关变了要跟着勾上或取消。
struct PingsItem(CheckMenuItem<Wry>);
/// 「检查更新」这一项。
struct UpdateItem(MenuItem<Wry>);

/// 把控制面板调到前面（托盘、再次打开程序都用它）。
pub fn show_main(app: &AppHandle) {
    show_page(app, None);
}

/// 把控制面板调到前面，`page` 是要打开的那一页。关掉过的控制面板网页已经销毁（3.0-M6），按配置重新建一个。
pub fn show_page(app: &AppHandle, page: Option<&'static str>) {
    // macOS：控制面板开着时程序坞里才有图标（关掉时藏起来，见 lib.rs）
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(true);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        if let Some(p) = page {
            let _ = app.emit_to("main", "nav://page", p);
        }
        return;
    }
    let app = app.clone();
    // Windows 上在事件回调里同步建窗口可能卡住（Tauri 的已知问题），放到别的线程去建。
    std::thread::spawn(move || {
        let Some(mut cfg) = app.config().app.windows.first().cloned() else {
            return;
        };
        if let Some(p) = page {
            cfg.url = WebviewUrl::App(format!("index.html#{p}").into());
        }
        match WebviewWindowBuilder::from_config(&app, &cfg).and_then(|b| b.build()) {
            Ok(w) => {
                let rect = app
                    .try_state::<api::AppState>()
                    .and_then(|s| s.settings.lock().ok().and_then(|s| s.panel_rect));
                reveal_main(&app, &w, rect);
                if let Some(state) = app.try_state::<api::AppState>() {
                    let theme = state
                        .settings
                        .lock()
                        .map(|s| s.theme.clone())
                        .unwrap_or_default();
                    api::apply_window_theme(&app, &theme);
                }
            }
            Err(e) => eprintln!("控制面板建不起来：{e}"),
        }
    });
}

/// 控制面板建好时是藏着的（tauri.conf.json 里 visible 是 false）：上次关掉时的位置还在某块显示器上，
/// 就挪回那儿、还原大小，再显示出来（3.1.1；3.0-M6 起每次打开都是新建的，以前总回到屏幕中间）。
/// 先挪位置再定大小：挪到缩放不同的显示器上时系统会按比例改大小，最后再按记下的定一次。
pub fn reveal_main(app: &AppHandle, w: &WebviewWindow, rect: Option<PanelRect>) {
    if let Some(r) = rect.filter(|r| on_screen(app, r)) {
        let _ = w.set_position(PhysicalPosition::new(r.x, r.y));
        let _ = w.set_size(PhysicalSize::new(r.width, r.height));
    }
    let _ = w.show();
    let _ = w.set_focus();
}

/// 标题栏的中间（窗口上沿往下 16 像素）还在某块显示器上：显示器拔了、换了排列，就不还原，按默认的居中。
fn on_screen(app: &AppHandle, r: &PanelRect) -> bool {
    let (cx, cy) = (
        r.x.saturating_add((r.width / 2) as i32),
        r.y.saturating_add(16),
    );
    app.available_monitors().is_ok_and(|list| {
        list.iter().any(|m| {
            let (p, s) = (m.position(), m.size());
            (p.x..p.x + s.width as i32).contains(&cx) && (p.y..p.y + s.height as i32).contains(&cy)
        })
    })
}

/// 关控制面板时记下它的位置和大小（存进设置）。最小化、最大化时不记，留着上一次正常时的。
pub fn remember_main(window: &Window) {
    if window.is_minimized().unwrap_or(true) || window.is_maximized().unwrap_or(true) {
        return;
    }
    let (Ok(p), Ok(s)) = (window.outer_position(), window.inner_size()) else {
        return;
    };
    if let Some(state) = window.app_handle().try_state::<api::AppState>() {
        api::save_panel_rect(
            &state,
            PanelRect {
                x: p.x,
                y: p.y,
                width: s.width,
                height: s.height,
            },
        );
    }
}

pub fn setup(app: &tauri::App, enabled: bool) -> tauri::Result<()> {
    let pings = CheckMenuItem::with_id(app, "pings", "信号开启", true, enabled, None::<&str>)?;
    let open = MenuItem::with_id(app, "open", "打开控制面板", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", "检查更新", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &pings,
            &open,
            &update,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    app.manage(PingsItem(pings));
    app.manage(UpdateItem(update));
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip(TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| match e.id.as_ref() {
            "pings" => {
                let on = app
                    .try_state::<api::AppState>()
                    .is_some_and(|s| !s.enabled.load(std::sync::atomic::Ordering::Relaxed));
                api::set_enabled(app, on);
            }
            "open" => show_main(app),
            // 打开「关于」页；还没查到新版本就顺便查一次
            "update" => {
                crate::update::tray_show_about(app);
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    crate::update::check(&app, false).await;
                });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = e
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

pub fn set_checked(app: &AppHandle, on: bool) {
    if let Some(item) = app.try_state::<PingsItem>() {
        let _ = item.0.set_checked(on);
    }
}

/// 查到新版本时「检查更新」变成「更新到 vX」，没有时变回来。
pub fn set_update_label(app: &AppHandle, version: Option<&str>) {
    if let Some(item) = app.try_state::<UpdateItem>() {
        let text = version.map_or_else(|| "检查更新".to_string(), |v| format!("更新到 v{v}"));
        let _ = item.0.set_text(text);
    }
}
