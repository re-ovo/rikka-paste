mod clipboard;
mod config;
mod crypto;
mod sync;

use std::sync::Arc;

use tauri::{AppHandle, Manager, RunEvent, State, WindowEvent};
#[cfg(desktop)]
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    Wry,
};
#[cfg(desktop)]
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_opener::OpenerExt;

use config::Config;
use sync::{Engine, Snapshot};

const RELEASES_URL: &str = "https://github.com/re-ovo/rikka-paste/releases/";
/// 开机自启时附带的参数，带上它启动时只驻留托盘、不弹出窗口
#[cfg(desktop)]
const AUTOSTART_ARG: &str = "--autostart";

#[cfg(desktop)]
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

/// 剪贴板读写放到阻塞线程：Android 上要等主线程里的 Kotlin 插件返回，在主线程上同步调用会死锁
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| e.to_string())?
}

/// 以"敏感内容"写入剪贴板：不会被同步，也不会进入 Maccy / Win+V 历史
#[tauri::command]
async fn copy_secret(text: String) -> Result<(), String> {
    blocking(move || {
        clipboard::write(
            &clipboard::Content::Text(text),
            clipboard::WriteMode::Concealed,
        )
    })
    .await
}

/// 手动发送本机剪贴板（Android 没法在后台监听剪贴板）
#[tauri::command]
async fn send_clipboard(engine: State<'_, Arc<Engine>>) -> Result<(), String> {
    let engine = engine.inner().clone();
    blocking(move || engine.send_clipboard()).await
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

/// 以系统登录项为准，不存进 config.json，避免用户在系统设置里删掉后两边不一致
#[cfg(desktop)]
#[tauri::command]
fn get_autostart(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[cfg(desktop)]
#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    let autolaunch = app.autolaunch();
    let result = if enabled {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    result.map_err(|e| format!("设置开机自启失败: {e}"))?;
    autolaunch.is_enabled().map_err(|e| e.to_string())
}

/// 移动端没有开机自启，界面上也不显示该选项
#[cfg(mobile)]
#[tauri::command]
fn get_autostart() -> bool {
    false
}

#[cfg(mobile)]
#[tauri::command]
fn set_autostart() -> Result<bool, String> {
    Err("当前平台不支持开机自启".into())
}

#[cfg(mobile)]
fn sync_tray(_app: &AppHandle) {}

#[cfg(desktop)]
fn sync_tray(app: &AppHandle) {
    if let Some(toggle) = app.try_state::<TrayToggle>() {
        let _ = toggle
            .0
            .set_checked(app.state::<Arc<Engine>>().config().enabled);
    }
}

#[cfg(desktop)]
fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg(desktop)]
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
    let builder = tauri::Builder::default().plugin(tauri_plugin_opener::init());
    #[cfg(desktop)]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        MacosLauncher::LaunchAgent,
        Some(vec![AUTOSTART_ARG]),
    ));
    #[cfg(target_os = "android")]
    let builder = builder.plugin(clipboard::plugin());
    builder
        .invoke_handler(tauri::generate_handler![
            get_state,
            update_config,
            generate_key,
            copy_secret,
            send_clipboard,
            open_release,
            get_autostart,
            set_autostart
        ])
        .setup(|app| {
            // 常驻托盘，不占 Dock
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let config_path = app.path().app_config_dir()?.join("config.json");
            let engine = Engine::start(app.handle().clone(), config_path)?;
            app.manage(engine);

            // 窗口等 Engine 就绪后再建：Android 上 IPC 不经过主线程，先建窗口的话前端可能在 manage 之前就调用 get_state
            if let Some(window) = app.config().app.windows.first() {
                tauri::WebviewWindowBuilder::from_config(app.handle(), window)?.build()?;
            }

            #[cfg(desktop)]
            {
                setup_tray(app, app.state::<Arc<Engine>>().config().enabled)?;

                // 窗口在配置里默认隐藏，手动打开时才显示，避免开机自启时闪一下
                if !std::env::args().any(|arg| arg == AUTOSTART_ARG) {
                    show_main_window(app.handle());
                }
            }
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
