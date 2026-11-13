//! Small shell icons, cached per file type (per file for .exe/.lnk/... which carry their own).

use std::collections::HashMap;
use std::sync::Arc;

use gpui::{Image, ImageFormat};

use crate::fsops::Entry;
use crate::platform;

#[derive(Default)]
pub struct IconCache {
    map: HashMap<String, Option<Arc<Image>>>,
}

impl IconCache {
    pub fn get(&mut self, e: &Entry) -> Option<Arc<Image>> {
        let ext = e
            .path
            .extension()
            .map(|x| format!(".{}", x.to_string_lossy().to_lowercase()))
            .unwrap_or_default();
        // Drive roots carry their own icon (and so do executables and shortcuts), so those are
        // asked for by path; everything else shares one icon per file type.
        let is_drive = e.is_drive;
        let per_file =
            is_drive || (!e.is_dir && matches!(ext.as_str(), ".exe" | ".lnk" | ".ico" | ".scr" | ".url" | ".cur" | ".msi"));
        let key = if e.is_dir && !is_drive {
            "<dir>".to_string()
        } else if per_file {
            e.path.to_string_lossy().to_lowercase()
        } else {
            ext
        };
        self.map
            .entry(key)
            .or_insert_with(|| {
                platform::file_icon_png(&e.path, e.is_dir && !per_file, per_file)
                    .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)))
            })
            .clone()
    }
}
