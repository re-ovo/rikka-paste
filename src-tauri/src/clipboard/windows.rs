//! 通过 Win32 剪贴板读写，遵循 Windows 剪贴板历史（Win+V）及第三方管理器的排除格式约定：
//! https://learn.microsoft.com/windows/win32/dataxchg/clipboard-formats#cloud-clipboard-and-clipboard-history-formats

use clipboard_win::{formats, raw, register_format, Clipboard};

use super::{Read, WriteMode};

const EXCLUDE_MONITOR: &str = "ExcludeClipboardContentFromMonitorProcessing";
const VIEWER_IGNORE: &str = "Clipboard Viewer Ignore";
const CAN_INCLUDE_IN_HISTORY: &str = "CanIncludeInClipboardHistory";
const CAN_UPLOAD_TO_CLOUD: &str = "CanUploadToCloudClipboard";

const OPEN_ATTEMPTS: usize = 10;

pub fn change_count() -> u64 {
    raw::seq_num().map_or(0, |n| n.get() as u64)
}

fn format_id(name: &str) -> Option<u32> {
    register_format(name).map(|f| f.get())
}

fn is_sensitive() -> bool {
    let has = |name| format_id(name).is_some_and(raw::is_format_avail);
    if has(EXCLUDE_MONITOR) || has(VIEWER_IGNORE) {
        return true;
    }
    // KeePass、1Password 等会写入值为 0 的 CanIncludeInClipboardHistory
    if let Some(fmt) = format_id(CAN_INCLUDE_IN_HISTORY).filter(|f| raw::is_format_avail(*f)) {
        let mut buf = [0u8; 4];
        if raw::get(fmt, &mut buf).is_ok() && u32::from_le_bytes(buf) == 0 {
            return true;
        }
    }
    false
}

pub fn read() -> Read {
    let Ok(_clip) = Clipboard::new_attempts(OPEN_ATTEMPTS) else {
        return Read::Busy;
    };
    if is_sensitive() {
        return Read::Sensitive;
    }
    if !raw::is_format_avail(formats::CF_UNICODETEXT) {
        return Read::Empty;
    }
    let mut out = Vec::new();
    match raw::get_string(&mut out) {
        Ok(_) => String::from_utf8(out).map_or(Read::Empty, Read::Text),
        Err(_) => Read::Busy,
    }
}

pub fn write_text(text: &str, mode: WriteMode) -> Result<(), String> {
    let _clip =
        Clipboard::new_attempts(OPEN_ATTEMPTS).map_err(|e| format!("打开剪贴板失败: {e}"))?;
    raw::set_string(text).map_err(|e| format!("写入剪贴板失败: {e}"))?;

    let set_dword = |name: &str, value: u32| {
        if let Some(fmt) = format_id(name) {
            let _ = raw::set_without_clear(fmt, &value.to_le_bytes());
        }
    };
    match mode {
        WriteMode::Normal => {}
        WriteMode::Transient => {
            set_dword(CAN_INCLUDE_IN_HISTORY, 0);
            set_dword(CAN_UPLOAD_TO_CLOUD, 0);
        }
        WriteMode::Concealed => {
            set_dword(EXCLUDE_MONITOR, 0);
            set_dword(CAN_INCLUDE_IN_HISTORY, 0);
            set_dword(CAN_UPLOAD_TO_CLOUD, 0);
        }
    }
    Ok(())
}
