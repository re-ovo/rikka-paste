//! 通过 NSPasteboard 读写，遵循 http://nspasteboard.org 的标记约定（Maccy 等工具都认这套）。

use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

use super::{Read, WriteMode};

const CONCEALED: &str = "org.nspasteboard.ConcealedType";
const TRANSIENT: &str = "org.nspasteboard.TransientType";
const SOURCE: &str = "org.nspasteboard.source";
const BUNDLE_ID: &str = "me.rerere.rikka-paste";

pub fn change_count() -> u64 {
    autoreleasepool(|_| NSPasteboard::generalPasteboard().changeCount() as u64)
}

pub fn read() -> Read {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        if let Some(types) = pb.types() {
            let sensitive = types.iter().any(|t| {
                let t = t.to_string();
                t == CONCEALED || t == TRANSIENT
            });
            if sensitive {
                return Read::Sensitive;
            }
        }
        match pb.stringForType(unsafe { NSPasteboardTypeString }) {
            Some(s) => Read::Text(s.to_string()),
            None => Read::Empty,
        }
    })
}

pub fn write_text(text: &str, mode: WriteMode) -> Result<(), String> {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        if !pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString }) {
            return Err("写入 NSPasteboard 失败".into());
        }
        let set_marker = |ty: &str, value: &str| {
            pb.setString_forType(&NSString::from_str(value), &NSString::from_str(ty));
        };
        set_marker(SOURCE, BUNDLE_ID);
        match mode {
            WriteMode::Normal => {}
            WriteMode::Transient => set_marker(TRANSIENT, ""),
            WriteMode::Concealed => set_marker(CONCEALED, ""),
        }
        Ok(())
    })
}
