//! Ordering of directory listings: folders first, then the chosen column, with a
//! natural ("file2" < "file10"), case-insensitive name order as the tie-breaker.

use std::cmp::Ordering;

use crate::format;
use crate::model::FileEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortColumn {
    #[default]
    Name,
    Size,
    Type,
    Modified,
    Permissions,
    VfsStatus,
}

impl SortColumn {
    /// Column index used by `pane.slint` headers (0 = Name ... 5 = VFS Status).
    #[must_use]
    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Self::Size,
            2 => Self::Type,
            3 => Self::Modified,
            4 => Self::Permissions,
            5 => Self::VfsStatus,
            _ => Self::Name,
        }
    }

    #[must_use]
    pub fn index(self) -> i32 {
        self as i32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SortKey {
    pub column: SortColumn,
    pub ascending: bool,
}

impl Default for SortKey {
    fn default() -> Self {
        Self { column: SortColumn::Name, ascending: true }
    }
}

/// Natural comparison of two already-lowercased names.
#[must_use]
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let da = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let db = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let (na, nb) = (trim_zeros(&a[..da]), trim_zeros(&b[..db]));
                let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb)).then_with(|| da.cmp(&db));
                if ord != Ordering::Equal {
                    return ord;
                }
                a = &a[da..];
                b = &b[db..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|&&c| c == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
}

/// The display order of `entries` as indices (entries themselves are not moved), with
/// the same rules as [`sort_entries`]. Each name is lowercased once.
#[must_use]
pub fn sort_order(entries: &[FileEntry], key: SortKey) -> Vec<u32> {
    let lower: Vec<String> = entries.iter().map(|e| e.name.to_lowercase()).collect();
    let mut order: Vec<u32> = (0..u32::try_from(entries.len()).unwrap_or(u32::MAX)).collect();
    order.sort_by(|&x, &y| {
        let (a, b) = (&entries[x as usize], &entries[y as usize]);
        let by_col = match key.column {
            SortColumn::Name => Ordering::Equal,
            SortColumn::Size => a.size.cmp(&b.size),
            SortColumn::Type => format::extension(&a.name).cmp(&format::extension(&b.name)),
            SortColumn::Modified => a.modified.cmp(&b.modified),
            SortColumn::Permissions => a.mode.cmp(&b.mode),
            SortColumn::VfsStatus => a.vfs.status.cmp(&b.vfs.status),
        };
        let ord =
            by_col.then_with(|| natural_cmp(&lower[x as usize], &lower[y as usize])).then_with(|| a.name.cmp(&b.name));
        (!a.is_navigable()).cmp(&!b.is_navigable()).then(if key.ascending { ord } else { ord.reverse() })
    });
    order
}

/// Sort `entries` in place. Folders always come first (Nemo's default), whatever the
/// direction. Runs in O(n log n) with each name lowercased once.
pub fn sort_entries(entries: &mut Vec<FileEntry>, key: SortKey) {
    let mut keyed: Vec<(bool, String, FileEntry)> =
        entries.drain(..).map(|e| (!e.is_navigable(), e.name.to_lowercase(), e)).collect();
    keyed.sort_by(|(da, na, a), (db, nb, b)| {
        let by_col = match key.column {
            SortColumn::Name => Ordering::Equal,
            SortColumn::Size => a.size.cmp(&b.size),
            SortColumn::Type => format::extension(&a.name).cmp(&format::extension(&b.name)),
            SortColumn::Modified => a.modified.cmp(&b.modified),
            SortColumn::Permissions => a.mode.cmp(&b.mode),
            SortColumn::VfsStatus => a.vfs.status.cmp(&b.vfs.status),
        };
        let ord = by_col.then_with(|| natural_cmp(na, nb)).then_with(|| a.name.cmp(&b.name));
        da.cmp(db).then(if key.ascending { ord } else { ord.reverse() })
    });
    entries.extend(keyed.into_iter().map(|(_, _, e)| e));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileKind, VfsMetadata, VfsUri};

    fn e(name: &str, kind: FileKind, size: u64) -> FileEntry {
        FileEntry {
            name: name.into(),
            uri: VfsUri::parse("/").unwrap_or_else(|e| panic!("{e}")),
            kind,
            target_is_dir: false,
            size,
            modified: None,
            mode: 0o644,
            vfs: VfsMetadata::default(),
        }
    }

    fn names(v: &[FileEntry]) -> Vec<&str> {
        v.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("a", "b"), Ordering::Less);
        assert_eq!(natural_cmp("x01", "x1"), Ordering::Greater);
        assert_eq!(natural_cmp("x0", "x00"), Ordering::Less);
        assert_eq!(natural_cmp("abc", "ab"), Ordering::Greater);
    }

    #[test]
    fn folders_first_then_column() {
        let mut v = vec![
            e("b.txt", FileKind::File, 30),
            e("Zeta", FileKind::Directory, 0),
            e("a10.txt", FileKind::File, 10),
            e("a2.txt", FileKind::File, 20),
            e("alpha", FileKind::Directory, 0),
        ];
        sort_entries(&mut v, SortKey::default());
        assert_eq!(names(&v), ["alpha", "Zeta", "a2.txt", "a10.txt", "b.txt"]);
        sort_entries(&mut v, SortKey { column: SortColumn::Size, ascending: false });
        assert_eq!(names(&v), ["Zeta", "alpha", "b.txt", "a2.txt", "a10.txt"]);
    }

    #[test]
    fn order_matches_in_place_sort() {
        let v = vec![
            e("b10", FileKind::File, 1),
            e("dir", FileKind::Directory, 0),
            e("b9", FileKind::File, 5),
            e("A", FileKind::File, 3),
        ];
        for key in [SortKey::default(), SortKey { column: SortColumn::Size, ascending: false }] {
            let order = sort_order(&v, key);
            let mut sorted = v.clone();
            sort_entries(&mut sorted, key);
            let by_order: Vec<&str> = order.iter().map(|&i| v[i as usize].name.as_str()).collect();
            assert_eq!(by_order, names(&sorted));
        }
    }

    #[test]
    fn column_index_round_trip() {
        for i in 0..6 {
            assert_eq!(SortColumn::from_index(i).index(), i);
        }
    }
}
