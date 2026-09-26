//! 通过 Win32 剪贴板读写，遵循 Windows 剪贴板历史（Win+V）及第三方管理器的排除格式约定：
//! https://learn.microsoft.com/windows/win32/dataxchg/clipboard-formats#cloud-clipboard-and-clipboard-history-formats

use std::io::Cursor;

use clipboard_win::{formats, raw, register_format, Clipboard};
use image::{codecs::bmp::BmpDecoder, DynamicImage, ImageFormat};

use super::{Content, Read, WriteMode};

const EXCLUDE_MONITOR: &str = "ExcludeClipboardContentFromMonitorProcessing";
const VIEWER_IGNORE: &str = "Clipboard Viewer Ignore";
const CAN_INCLUDE_IN_HISTORY: &str = "CanIncludeInClipboardHistory";
const CAN_UPLOAD_TO_CLOUD: &str = "CanUploadToCloudClipboard";
/// Chrome、Office、截图工具等使用的注册格式，比 CF_DIB 多了透明度
const PNG: &str = "PNG";
const PNG_END: &[u8] = b"IEND\xAE\x42\x60\x82";

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
    let dib = {
        let Ok(_clip) = Clipboard::new_attempts(OPEN_ATTEMPTS) else {
            return Read::Busy;
        };
        if is_sensitive() {
            return Read::Sensitive;
        }
        if raw::is_format_avail(formats::CF_UNICODETEXT) {
            let mut out = Vec::new();
            return match raw::get_string(&mut out) {
                Ok(_) => {
                    String::from_utf8(out).map_or(Read::Empty, |s| Read::Content(Content::Text(s)))
                }
                Err(_) => Read::Busy,
            };
        }
        if let Some(fmt) = format_id(PNG).filter(|f| raw::is_format_avail(*f)) {
            let mut out = Vec::new();
            return match raw::get_vec(fmt, &mut out) {
                Ok(_) => Read::Content(Content::Png(trim_png(out))),
                Err(_) => Read::Busy,
            };
        }
        if !raw::is_format_avail(formats::CF_DIB) {
            return Read::Empty;
        }
        let mut dib = Vec::new();
        if raw::get_vec(formats::CF_DIB, &mut dib).is_err() {
            return Read::Busy;
        }
        dib
    };
    // 转码较慢，放到关闭剪贴板之后
    dib_to_png(&dib).map_or(Read::Empty, |png| Read::Content(Content::Png(png)))
}

/// GlobalSize 可能比实际写入的长度大，去掉 IEND 之后的填充，保证同一张图读回来字节一致（防回环依赖 hash）
fn trim_png(mut png: Vec<u8>) -> Vec<u8> {
    if let Some(pos) = png.windows(PNG_END.len()).rposition(|w| w == PNG_END) {
        png.truncate(pos + PNG_END.len());
    }
    png
}

fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let decoder = BmpDecoder::new_without_file_header(Cursor::new(dib)).ok()?;
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    // 有些程序写入的 32 位 DIB 带 alpha 掩码但 alpha 全为 0，按不透明处理
    if let DynamicImage::ImageRgba8(buf) = &mut img {
        if buf.pixels().all(|p| p[3] == 0) {
            buf.pixels_mut().for_each(|p| p[3] = 255);
        }
    }
    let mut png = Vec::new();
    img.write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// 32 位 BI_RGB、自下而上的 CF_DIB。多数程序会忽略这里的 alpha，所以透明区域先与白色混合
fn png_to_dib(png: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(png, ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (width, height) = img.dimensions();
    let mut dib = Vec::with_capacity(40 + img.len());
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&(width as i32).to_le_bytes());
    dib.extend_from_slice(&(height as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&(img.len() as u32).to_le_bytes()); // biSizeImage
    dib.extend_from_slice(&[0; 16]); // 分辨率、调色板
    for row in img.rows().rev() {
        for p in row {
            let [r, g, b, a] = p.0.map(u32::from);
            let blend = |c: u32| ((c * a + 255 * (255 - a)) / 255) as u8;
            dib.extend_from_slice(&[blend(b), blend(g), blend(r), 255]);
        }
    }
    Some(dib)
}

pub fn write(content: &Content, mode: WriteMode) -> Result<(), String> {
    // 解码 PNG 较慢，在打开剪贴板之前完成
    let dib = match content {
        Content::Text(_) => Vec::new(),
        Content::Png(png) => png_to_dib(png).ok_or("图片解码失败")?,
    };
    let _clip =
        Clipboard::new_attempts(OPEN_ATTEMPTS).map_err(|e| format!("打开剪贴板失败: {e}"))?;
    match content {
        Content::Text(text) => raw::set_string(text).map_err(|e| format!("写入剪贴板失败: {e}"))?,
        Content::Png(png) => {
            raw::empty().map_err(|e| format!("清空剪贴板失败: {e}"))?;
            let fmt = format_id(PNG).ok_or("注册 PNG 格式失败")?;
            raw::set_without_clear(fmt, png).map_err(|e| format!("写入剪贴板失败: {e}"))?;
            // 画图等很多程序只认 CF_DIB
            raw::set_without_clear(formats::CF_DIB, &dib)
                .map_err(|e| format!("写入剪贴板失败: {e}"))?;
        }
    }

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
