//! Display helpers (sizes, modes, type labels, icon categories). Pure functions, so
//! the controller can format 100k rows on a worker thread.

use crate::model::{FileEntry, FileKind};

/// Icon category; `pane.slint` draws one glyph per value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Category {
    Folder = 0,
    File = 1,
    Image = 2,
    Audio = 3,
    Video = 4,
    Archive = 5,
    Code = 6,
    Document = 7,
    Executable = 8,
    Link = 9,
    Special = 10,
}

/// Lowercased extension of `name`, without the dot (`None` for dotfiles like `.bashrc`).
#[must_use]
pub fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

#[must_use]
pub fn category(e: &FileEntry) -> Category {
    match e.kind {
        FileKind::Directory => return Category::Folder,
        FileKind::Symlink if e.target_is_dir => return Category::Folder,
        FileKind::Symlink => return Category::Link,
        FileKind::Special => return Category::Special,
        FileKind::File => {}
    }
    let by_ext = extension(&e.name).map(|x| match x.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "svg" | "bmp" | "tif" | "tiff" | "ico" | "heic" | "jxl" => {
            Category::Image
        }
        "flac" | "wav" | "mp3" | "ogg" | "opus" | "m4a" | "aac" | "aiff" | "alac" | "mid" | "midi" => Category::Audio,
        "mkv" | "mp4" | "webm" | "mov" | "avi" | "m4v" | "flv" => Category::Video,
        "zip" | "tar" | "gz" | "xz" | "zst" | "bz2" | "7z" | "rar" | "tgz" | "lz4" | "iso" | "deb" | "rpm" => {
            Category::Archive
        }
        "rs" | "zig" | "c" | "h" | "cpp" | "hpp" | "py" | "ts" | "tsx" | "js" | "json" | "toml" | "yaml" | "yml"
        | "sh" | "slint" | "go" | "lua" | "md" | "txt" | "conf" | "ini" | "log" | "css" | "html" | "xml" | "csv"
        | "lock" => Category::Code,
        "pdf" | "odt" | "ods" | "odp" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "epub" | "rtf" => {
            Category::Document
        }
        "appimage" | "bin" | "run" | "exe" => Category::Executable,
        _ => Category::File,
    });
    match by_ext {
        Some(c) if c != Category::File => c,
        _ if e.mode & 0o111 != 0 => Category::Executable,
        _ => Category::File,
    }
}

/// Human type label ("Folder", "FLAC audio", "Rust source", ...).
#[must_use]
pub fn type_label(e: &FileEntry) -> String {
    match e.kind {
        FileKind::Directory => return "Folder".into(),
        FileKind::Symlink if e.target_is_dir => return "Link to folder".into(),
        FileKind::Symlink => return "Link".into(),
        FileKind::Special => return "Special file".into(),
        FileKind::File => {}
    }
    let Some(ext) = extension(&e.name) else {
        return if e.mode & 0o111 != 0 { "Program".into() } else { "File".into() };
    };
    let known = match ext.as_str() {
        "rs" => "Rust source",
        "zig" => "Zig source",
        "slint" => "Slint markup",
        "md" => "Markdown",
        "txt" | "log" => "Plain text",
        "toml" | "yaml" | "yml" | "ini" | "conf" | "json" => "Config",
        "sh" => "Shell script",
        "pdf" => "PDF document",
        "flac" => "FLAC audio",
        "wav" => "WAV audio",
        "mp3" => "MP3 audio",
        "opus" | "ogg" => "Ogg audio",
        "mkv" => "Matroska video",
        "mp4" | "m4v" => "MP4 video",
        "png" => "PNG image",
        "jpg" | "jpeg" => "JPEG image",
        "webp" => "WebP image",
        "svg" => "SVG image",
        "zip" => "Zip archive",
        "tar" | "gz" | "xz" | "zst" | "tgz" | "bz2" => "Tar archive",
        "iso" => "Disk image",
        _ => "",
    };
    if known.is_empty() { format!("{} file", ext.to_ascii_uppercase()) } else { known.into() }
}

/// Decimal units like Nemo: `912 bytes`, `4.2 kB`, `1.3 GB`.
#[must_use]
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["kB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1000 {
        return if bytes == 1 { "1 byte".into() } else { format!("{bytes} bytes") };
    }
    #[allow(clippy::cast_precision_loss)]
    let mut v = bytes as f64 / 1000.0;
    let mut unit = 0;
    while v >= 999.95 && unit + 1 < UNITS.len() {
        v /= 1000.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// `ls -l` style: `drwxr-xr-x`, with setuid/setgid/sticky.
#[must_use]
pub fn mode(kind: FileKind, mode: u32) -> String {
    let t = match kind {
        FileKind::Directory => 'd',
        FileKind::Symlink => 'l',
        FileKind::Special => 's',
        FileKind::File => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(t);
    for (shift, special, set_x, set_nox) in [(6, 0o4000, 's', 'S'), (3, 0o2000, 's', 'S'), (0, 0o1000, 't', 'T')] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(match (bits & 1 != 0, mode & special != 0) {
            (true, true) => set_x,
            (false, true) => set_nox,
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VfsMetadata, VfsUri};

    fn entry(name: &str, kind: FileKind, mode: u32) -> FileEntry {
        FileEntry {
            name: name.into(),
            uri: VfsUri::parse("/x").unwrap_or_else(|e| panic!("{e}")),
            kind,
            target_is_dir: false,
            size: 0,
            modified: None,
            mode,
            vfs: VfsMetadata::default(),
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(size(0), "0 bytes");
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(999), "999 bytes");
        assert_eq!(size(1000), "1.0 kB");
        assert_eq!(size(4_200_000), "4.2 MB");
        assert_eq!(size(999_999_999), "1.0 GB");
    }

    #[test]
    fn modes() {
        assert_eq!(mode(FileKind::Directory, 0o755), "drwxr-xr-x");
        assert_eq!(mode(FileKind::File, 0o4755), "-rwsr-xr-x");
        assert_eq!(mode(FileKind::Directory, 0o1777), "drwxrwxrwt");
        assert_eq!(mode(FileKind::File, 0o2640), "-rw-r-S---");
    }

    #[test]
    fn categories_and_labels() {
        assert_eq!(category(&entry("song.FLAC", FileKind::File, 0o644)), Category::Audio);
        assert_eq!(category(&entry("run", FileKind::File, 0o755)), Category::Executable);
        assert_eq!(category(&entry(".bashrc", FileKind::File, 0o644)), Category::File);
        assert_eq!(category(&entry("d", FileKind::Directory, 0o755)), Category::Folder);
        assert_eq!(type_label(&entry("main.rs", FileKind::File, 0o644)), "Rust source");
        assert_eq!(type_label(&entry("a.qux", FileKind::File, 0o644)), "QUX file");
        assert_eq!(extension(".bashrc"), None);
    }
}
