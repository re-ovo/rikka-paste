//! Android：通过 `gen/android` 里的 Kotlin 插件 `ClipboardPlugin` 读写系统剪贴板。
//!
//! 插件桥只能传 JSON，图片经由应用缓存目录里的文件交换；写入的图片由 FileProvider 以 content:// URI 暴露给其他应用。
//! Android 10 起只有前台且有焦点的应用才能读剪贴板，所以不轮询，由用户在界面上手动发送。
//! 插件调用会等待主线程上的 Kotlin 代码返回，不能在主线程上调用这里的函数。

use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use serde::{de::IgnoredAny, Deserialize, Serialize};
use tauri::{
    plugin::{Builder, PluginHandle, TauriPlugin},
    Manager, Wry,
};

use super::{Content, Read, WriteMode};

struct Bridge {
    handle: PluginHandle<Wry>,
    cache_dir: PathBuf,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

pub fn plugin() -> TauriPlugin<Wry> {
    Builder::new("rikka-clipboard")
        .setup(|app, api| {
            let handle = api.register_android_plugin("me.rerere.rikka_paste", "ClipboardPlugin")?;
            let cache_dir = app.path().app_cache_dir()?;
            let _ = BRIDGE.set(Bridge { handle, cache_dir });
            Ok(())
        })
        .build()
}

fn bridge() -> Result<&'static Bridge, String> {
    BRIDGE.get().ok_or_else(|| "剪贴板插件未初始化".into())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReadArgs {
    /// 剪贴板里是图片时，插件把它转成 PNG 写到这里
    image_path: PathBuf,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum ReadResponse {
    Text { text: String },
    Image,
    Sensitive,
    Empty,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WriteArgs<'a> {
    text: Option<&'a str>,
    /// 位于缓存目录 `clipboard/` 下的 PNG，与 `file_paths.xml` 对应
    image_path: Option<PathBuf>,
    sensitive: bool,
}

pub fn read() -> Read {
    try_read().unwrap_or_else(|e| {
        eprintln!("读取剪贴板失败: {e}");
        Read::Empty
    })
}

fn try_read() -> Result<Read, String> {
    let bridge = bridge()?;
    let image_path = bridge.cache_dir.join("clipboard-read.png");
    let response = bridge
        .handle
        .run_mobile_plugin("read", ReadArgs { image_path: image_path.clone() })
        .map_err(|e| e.to_string())?;
    Ok(match response {
        ReadResponse::Text { text } => Read::Content(Content::Text(text)),
        ReadResponse::Image => {
            let png = fs::read(&image_path).map_err(|e| e.to_string())?;
            let _ = fs::remove_file(&image_path);
            Read::Content(Content::Png(png))
        }
        ReadResponse::Sensitive => Read::Sensitive,
        ReadResponse::Empty => Read::Empty,
    })
}

/// Android 没有"只是不进历史"的标记，Transient 同样标为敏感，输入法的剪贴板历史不会记录
pub fn write(content: &Content, mode: WriteMode) -> Result<(), String> {
    let bridge = bridge()?;
    let sensitive = !matches!(mode, WriteMode::Normal);
    let args = match content {
        Content::Text(text) => WriteArgs {
            text: Some(text),
            image_path: None,
            sensitive,
        },
        Content::Png(png) => {
            let dir = bridge.cache_dir.join("clipboard");
            fs::create_dir_all(&dir).map_err(|e| format!("写入图片失败: {e}"))?;
            // 每次用新文件名，避免其他应用按 URI 缓存到旧图片
            let path = dir.join(format!("{:016x}.png", rand::random::<u64>()));
            fs::write(&path, png).map_err(|e| format!("写入图片失败: {e}"))?;
            WriteArgs {
                text: None,
                image_path: Some(path),
                sensitive,
            }
        }
    };
    bridge
        .handle
        .run_mobile_plugin::<IgnoredAny>("write", &args)
        .map_err(|e| format!("写入剪贴板失败: {e}"))?;
    remove_stale_images(&bridge.cache_dir.join("clipboard"), args.image_path.as_deref());
    Ok(())
}

/// 旧图片在被新内容替换之前可能还会被粘贴，所以等写入成功后再删
fn remove_stale_images(dir: &Path, keep: Option<&Path>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if Some(path.as_path()) != keep {
            let _ = fs::remove_file(path);
        }
    }
}
