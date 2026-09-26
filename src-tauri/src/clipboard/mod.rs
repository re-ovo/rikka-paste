//! 系统剪贴板的最小抽象：只关心纯文本，以及剪贴板管理器通用的"敏感/临时"标记。

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

pub enum Read {
    Text(String),
    /// 带有敏感/临时标记（密码管理器等），不应同步
    Sensitive,
    /// 没有文本内容
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

/// 每次剪贴板内容变化都会递增的计数
pub fn change_count() -> u64 {
    imp::change_count()
}

pub fn read() -> Read {
    imp::read()
}

pub fn write_text(text: &str, mode: WriteMode) -> Result<(), String> {
    imp::write_text(text, mode)
}
