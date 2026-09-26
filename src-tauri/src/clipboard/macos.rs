//! 通过 NSPasteboard 读写，遵循 http://nspasteboard.org 的标记约定（Maccy 等工具都认这套）。

use objc2::rc::autoreleasepool;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSDictionary, NSString};

use super::{Content, Read, WriteMode};

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
        if let Some(s) = pb.stringForType(unsafe { NSPasteboardTypeString }) {
            return Read::Content(Content::Text(s.to_string()));
        }
        if let Some(png) = pb.dataForType(unsafe { NSPasteboardTypePNG }) {
            return Read::Content(Content::Png(png.to_vec()));
        }
        // 预览、Safari 等只提供 TIFF，转成 PNG 再传
        match pb
            .dataForType(unsafe { NSPasteboardTypeTIFF })
            .and_then(|tiff| tiff_to_png(&tiff))
        {
            Some(png) => Read::Content(Content::Png(png)),
            None => Read::Empty,
        }
    })
}

fn tiff_to_png(tiff: &NSData) -> Option<Vec<u8>> {
    let rep = NSBitmapImageRep::imageRepWithData(tiff)?;
    let png = unsafe {
        rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }?;
    Some(png.to_vec())
}

pub fn write(content: &Content, mode: WriteMode) -> Result<(), String> {
    autoreleasepool(|_| {
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let ok = match content {
            Content::Text(text) => {
                pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString })
            }
            Content::Png(png) => pb.setData_forType(Some(&NSData::with_bytes(png)), unsafe {
                NSPasteboardTypePNG
            }),
        };
        if !ok {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiff_converts_to_png() {
        // 1×1 的 PNG，先经 AppKit 转成 TIFF，再走 tiff_to_png
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\x9cc\xf8\xcf\xc0\xf0\x1f\0\x05\0\x01\xff\x89\x99=\x1d\0\0\0\0IEND\xaeB`\x82";
        let rep = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(PNG)).unwrap();
        let tiff = rep.TIFFRepresentation().unwrap();
        let png = tiff_to_png(&tiff).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }
}
