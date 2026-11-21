//! Turning `FileEntry`s into display rows, filtering and selection math. Pure and
//! `Send`: everything here runs on blocking worker threads, never the UI thread.

use std::collections::HashSet;
use std::path::PathBuf;

use core_vfs::{FileEntry, format};
use slint::SharedString;

use crate::FileRow;

/// Representation of a file row that is pure data and completely `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowItem {
    pub name: String,
    pub size_text: String,
    pub type_text: String,
    pub modified_text: String,
    pub perms_text: String,
    pub status_text: String,
    pub status: i32,
    pub icon: i32,
    pub is_dir: bool,
    pub hidden: bool,
    pub selected: bool,
    pub uri: String,
    pub mtime_secs: i64,
    pub thumbnail_path: Option<PathBuf>,
}

#[must_use]
pub fn row_item(e: &FileEntry, selected: bool) -> RowItem {
    let navigable = e.is_navigable();
    let mtime_secs = e
        .modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
        .unwrap_or(0);
    let uri_str = e.uri.to_uri_string();
    let cat = format::category(e);
    let ext = format::extension(&e.name);
    let thumbnail_path = if crate::thumbnails::is_previewable(cat, ext.as_deref()) {
        crate::thumbnails::lookup_cached(&uri_str, mtime_secs)
    } else {
        None
    };

    RowItem {
        name: e.name.clone(),
        size_text: if navigable { String::new() } else { format::size(e.size) },
        type_text: format::type_label(e),
        modified_text: backends::time::format_local(e.modified),
        perms_text: format::mode(e.kind, e.mode),
        status_text: e.vfs.status.label().to_string(),
        status: e.vfs.status.code(),
        icon: cat as i32,
        is_dir: navigable,
        hidden: e.is_hidden(),
        selected,
        uri: uri_str,
        mtime_secs,
        thumbnail_path,
    }
}

/// Converts a pure `RowItem` to Slint's `FileRow` (loading the `slint::Image` on the UI thread).
#[must_use]
pub fn to_file_row(r: &RowItem) -> FileRow {
    let (thumbnail, has_thumbnail) = if let Some(path) = &r.thumbnail_path {
        if let Ok(img) = slint::Image::load_from_path(path) { (img, true) } else { (slint::Image::default(), false) }
    } else {
        (slint::Image::default(), false)
    };

    FileRow {
        name: SharedString::from(r.name.as_str()),
        uri: SharedString::from(r.uri.as_str()),
        size_text: SharedString::from(r.size_text.as_str()),
        type_text: SharedString::from(r.type_text.as_str()),
        modified_text: SharedString::from(r.modified_text.as_str()),
        perms_text: SharedString::from(r.perms_text.as_str()),
        status_text: SharedString::from(r.status_text.as_str()),
        status: r.status,
        icon: r.icon,
        is_dir: r.is_dir,
        hidden: r.hidden,
        selected: r.selected,
        thumbnail,
        has_thumbnail,
    }
}

/// Indices of `entries` to show, following `order`: hidden files only with `show_hidden`, and
/// only names containing `filter` (case-insensitive).
#[must_use]
pub fn visible(entries: &[FileEntry], order: &[u32], show_hidden: bool, filter: &str) -> Vec<u32> {
    let needle = filter.trim().to_lowercase();
    order
        .iter()
        .copied()
        .filter(|&i| {
            entries.get(i as usize).is_some_and(|e| {
                (show_hidden || !e.is_hidden()) && (needle.is_empty() || e.name.to_lowercase().contains(&needle))
            })
        })
        .collect()
}

#[must_use]
pub fn rows(entries: &[FileEntry], visible: &[u32], selected: &HashSet<String>) -> Vec<RowItem> {
    visible.iter().filter_map(|&i| entries.get(i as usize)).map(|e| row_item(e, selected.contains(&e.name))).collect()
}

/// "1,284"
#[must_use]
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_vfs::{FileKind, VfsMetadata, VfsUri};

    fn e(name: &str) -> FileEntry {
        FileEntry {
            name: name.into(),
            uri: VfsUri::parse("/x").unwrap_or_else(|e| panic!("{e}")),
            kind: FileKind::File,
            target_is_dir: false,
            size: 1500,
            modified: None,
            mode: 0o644,
            vfs: VfsMetadata::default(),
        }
    }

    #[test]
    fn hidden_and_filter() {
        let v = vec![e("Alpha.flac"), e(".hidden"), e("beta.txt"), e("ALPHABET")];
        let o = [0, 1, 2, 3];
        assert_eq!(visible(&v, &o, false, ""), [0, 2, 3]);
        assert_eq!(visible(&v, &o, true, ""), [0, 1, 2, 3]);
        assert_eq!(visible(&v, &o, false, "alpha"), [0, 3]);
        assert_eq!(visible(&v, &[3, 1, 0], true, "HID"), [1]);
        assert_eq!(visible(&v, &[3, 2, 0], false, ""), [3, 2, 0], "order is kept");
    }

    #[test]
    fn rows_carry_selection_and_format() {
        let v = vec![e("a"), e("b")];
        let sel: HashSet<String> = ["b".to_owned()].into();
        let r = rows(&v, &[1, 0], &sel);
        assert_eq!(r[0].name, "b");
        assert!(r[0].selected && !r[1].selected);
        assert_eq!(r[0].size_text, "1.5 kB");
        assert_eq!(r[0].perms_text, "-rw-r--r--");
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
