//! Turning `FileEntry`s into display rows, filtering and selection math. Pure and
//! `Send`: everything here runs on blocking worker threads, never the UI thread.

use std::collections::HashSet;

use core_vfs::{FileEntry, format};
use slint::SharedString;

use crate::FileRow;

#[must_use]
pub fn file_row(e: &FileEntry, selected: bool) -> FileRow {
    let navigable = e.is_navigable();
    FileRow {
        name: SharedString::from(e.name.as_str()),
        size_text: if navigable { SharedString::new() } else { format::size(e.size).into() },
        type_text: format::type_label(e).into(),
        modified_text: backends::time::format_local(e.modified).into(),
        perms_text: format::mode(e.kind, e.mode).into(),
        status_text: e.vfs.status.label().into(),
        status: e.vfs.status.code(),
        icon: format::category(e) as i32,
        is_dir: navigable,
        hidden: e.is_hidden(),
        selected,
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
pub fn rows(entries: &[FileEntry], visible: &[u32], selected: &HashSet<String>) -> Vec<FileRow> {
    visible.iter().filter_map(|&i| entries.get(i as usize)).map(|e| file_row(e, selected.contains(&e.name))).collect()
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
