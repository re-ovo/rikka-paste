//! 系统剪贴板的最小抽象：只关心纯文本和图片（统一为 PNG），以及剪贴板管理器通用的"敏感/临时"标记。
//!
//! 剪贴板里同时有文本和图片时只取文本：Office 等应用复制文字时也会附带一张渲染图。

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
use android as imp;
#[cfg(target_os = "android")]
pub use android::plugin;

pub enum Content {
    Text(String),
    /// PNG 编码的图片
    Png(Vec<u8>),
}

impl Content {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Content::Text(text) => text.as_bytes(),
            Content::Png(png) => png,
        }
    }
}

pub enum Read {
    Content(Content),
    /// 带有敏感/临时标记（密码管理器等），不应同步
    Sensitive,
    /// 没有文本或图片
    Empty,
    /// 剪贴板暂时被其他程序占用，稍后重试（仅 Windows）
    #[cfg_attr(not(windows), allow(dead_code))]
    Busy,
}

#[derive(Clone, Copy)]
pub enum WriteMode {
    /// 普通写入，剪贴板管理器会记录
    Normal,
    /// 标记为临时内容，剪贴板管理器不记录
    Transient,
    /// 标记为敏感内容，剪贴板管理器不记录
    Concealed,
}

/// 每次剪贴板内容变化都会递增的计数。Android 不能在后台读剪贴板，没有这个概念
#[cfg(desktop)]
pub fn change_count() -> u64 {
    imp::change_count()
}

pub fn read() -> Read {
    imp::read()
}

pub fn write(content: &Content, mode: WriteMode) -> Result<(), String> {
    imp::write(content, mode)
}
