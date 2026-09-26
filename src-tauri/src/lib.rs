mod clipboard;
mod config;
mod crypto;
mod sync;

use std::sync::Arc;

use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, RunEvent, State, WindowEvent, Wry,
};
use tauri_plugin_opener::OpenerExt;

use config::Config;
use sync::{Engine, Snapshot};

const RELEASES_URL: &str = "https://github.com/re-ovo/rikka-paste/releases/";

struct TrayToggle(CheckMenuItem<Wry>);

#[tauri::command]
fn get_state(engine: State<'_, Arc<Engine>>) -> Snapshot {
    engine.snapshot()
}

#[tauri::command]
async fn update_config(
    app: AppHandle,
    engine: State<'_, Arc<Engine>>,
    config: Config,
) -> Result<Snapshot, String> {
    engine.update_config(config)?;
    sync_tray(&app);
    Ok(engine.snapshot())
}

#[tauri::command]
fn generate_key() -> String {
    config::generate_key()
}

/// 以"敏感内容"写入剪贴板：不会被同步，也不会进入 Maccy / Win+V 历史
#[tauri::command]
fn copy_secret(text: String) -> Result<(), String> {
    clipboard::write(
        &clipboard::Content::Text(text),
        clipboard::WriteMode::Concealed,
    )
}

/// 用系统浏览器打开新版本的 Release 页面；只接受本项目的链接
#[tauri::command]
fn open_release(app: AppHandle, url: String) -> Result<(), String> {
    if !url.starts_with(RELEASES_URL) {
        return Err("不支持打开该链接".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

fn sync_tray(app: &AppHandle) {
    if let Some(toggle) = app.try_state::<TrayToggle>() {
        let _ = toggle
            .0
            .set_checked(app.state::<Arc<Engine>>().config().enabled);
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn setup_tray(app: &tauri::App, enabled: bool) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "打开 Rikka Paste", true, None::<&str>)?;
    let toggle = CheckMenuItem::with_id(app, "toggle", "启用同步", true, enabled, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&show, &toggle, &PredefinedMenuItem::separator(app)?, &quit],
    )?;
    app.manage(TrayToggle(toggle));

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("Rikka Paste")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main_window(app),
            "toggle" => {
                let engine = app.state::<Arc<Engine>>();
                let mut config = engine.config();
                config.enabled = !config.enabled;
                if let Err(e) = engine.update_config(config) {
                    eprintln!("{e}");
                }
                sync_tray(app);
            }
            "quit" => app.exit(0),
            _ => {}
        });
    // macOS 用单色模板图标，随菜单栏明暗自动变色；Windows 托盘不支持模板图标，沿用彩色应用图标
    #[cfg(target_os = "macos")]
    {
        tray = tray
            .icon(tauri::include_image!("icons/tray.png"))
            .icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_state,
            update_config,
            generate_key,
            copy_secret,
            open_release
        ])
        .setup(|app| {
            // 常驻托盘，不占 Dock
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config_path = app.path().app_config_dir()?.join("config.json");
            let engine = Engine::start(app.handle().clone(), config_path)?;
            let enabled = engine.config().enabled;
            app.manage(engine);
            setup_tray(app, enabled)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // 关闭窗口只是隐藏，同步继续在后台运行
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let RunEvent::Exit = event {
                app.state::<Arc<Engine>>().shutdown();
            }
        });
}
