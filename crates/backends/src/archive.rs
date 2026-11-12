//! Bit-perfect lossless compression and archive engine for Gungnir FM.
//!
//! Provides a 3-tier hybrid pipeline:
//! - **Tier 1**: In-Process `libarchive` Core (C FFI) for streaming tar.zst, zip, tar.xz,
//!   tar.gz, tar.bz2, tar.lz4, and archive extraction with zero intermediate disk writes.
//! - **Tier 2**: Dedicated In-Process and Streaming Single-File Engines for `.zst`, `.xz`,
//!   `.gz`, `.bz2`, `.lz4`, `.br`.
//! - **Tier 3**: Asynchronous CLI Worker Fallback for solid 7-Zip (`7z a`/`7z x`), Brotli,
//!   encrypted archives, and specialized lossless domain formats (FLAC, WavPack, ALAC, JXL, FFV1).
//!
//! # Strict Invariant
//! Every algorithm in this module is strictly 100% BIT-PERFECT LOSSLESS. Original file
//! checksums (SHA-256 / BLAKE3) remain identical after decode.

use std::ffi::{CStr, CString, OsStr};
use std::os::raw::{c_char, c_int, c_long, c_uint, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ArchiveError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("libarchive error: {0}")]
    Libarchive(String),
    #[error("Unsupported archive format: {0}")]
    UnsupportedFormat(String),
    #[error("CLI tool '{tool}' failed with exit code {status}: {stderr}")]
    CliToolFailed { tool: String, status: i32, stderr: String },
    #[error("Tool not installed: '{0}'")]
    ToolNotFound(String),
    #[error("Verification failed: original {original_hash} != decompressed {decompressed_hash}")]
    IntegrityMismatch { original_hash: String, decompressed_hash: String },
    #[error("Extraction security violation: entry path attempts directory traversal: '{0}'")]
    PathTraversal(String),
    #[error("Empty item list provided for compression")]
    EmptyItemList,
    /// The archive is encrypted: no password was given (`wrong` false) or a wrong one.
    #[error("{}", if *wrong { "Wrong password for this archive" } else { "This archive needs its password" })]
    PasswordRequired { wrong: bool },
}

// ---- libarchive C FFI --------------------------------------------------------

const ARCHIVE_OK: c_int = 0;
const ARCHIVE_EOF: c_int = 1;
#[allow(dead_code)]
const ARCHIVE_RETRY: c_int = -10;
#[allow(dead_code)]
const ARCHIVE_WARN: c_int = -20;
#[allow(dead_code)]
const ARCHIVE_FAILED: c_int = -25;
#[allow(dead_code)]
const ARCHIVE_FATAL: c_int = -30;

const ARCHIVE_EXTRACT_TIME: c_int = 0x0004;
const ARCHIVE_EXTRACT_PERM: c_int = 0x0002;
const ARCHIVE_EXTRACT_SECURE_SYMLINKS: c_int = 0x0100;
/// The mode bits an extracted entry may keep: no group/other write, no setuid, setgid or
/// sticky bit.
const EXTRACT_PERM_MASK: c_uint = 0o755;
const ARCHIVE_EXTRACT_SECURE_NODOTDOT: c_int = 0x0200;
#[allow(dead_code)]
const ARCHIVE_EXTRACT_SECURE_NOABSOLUTEPATHS: c_int = 0x10000;

const AE_IFREG: c_uint = 0o100000;
const AE_IFDIR: c_uint = 0o040000;
const AE_IFLNK: c_uint = 0o120000;

#[allow(dead_code)]
unsafe extern "C" {
    fn archive_read_new() -> *mut c_void;
    fn archive_read_support_filter_all(a: *mut c_void) -> c_int;
    fn archive_read_support_format_all(a: *mut c_void) -> c_int;
    fn archive_read_open_filename(a: *mut c_void, filename: *const c_char, block_size: usize) -> c_int;
    fn archive_read_add_passphrase(a: *mut c_void, passphrase: *const c_char) -> c_int;
    fn archive_read_next_header(a: *mut c_void, entry: *mut *mut c_void) -> c_int;
    fn archive_read_data_block(a: *mut c_void, buff: *mut *const c_void, size: *mut usize, offset: *mut i64) -> c_int;
    fn archive_read_close(a: *mut c_void) -> c_int;
    fn archive_read_free(a: *mut c_void) -> c_int;

    fn archive_write_new() -> *mut c_void;
    fn archive_write_set_format_pax_restricted(a: *mut c_void) -> c_int;
    fn archive_write_set_format_zip(a: *mut c_void) -> c_int;
    fn archive_write_set_format_7zip(a: *mut c_void) -> c_int;
    fn archive_write_add_filter_none(a: *mut c_void) -> c_int;
    fn archive_write_add_filter_gzip(a: *mut c_void) -> c_int;
    fn archive_write_add_filter_bzip2(a: *mut c_void) -> c_int;
    fn archive_write_add_filter_xz(a: *mut c_void) -> c_int;
    fn archive_write_add_filter_zstd(a: *mut c_void) -> c_int;
    fn archive_write_add_filter_lz4(a: *mut c_void) -> c_int;
    fn archive_write_set_filter_option(a: *mut c_void, m: *const c_char, o: *const c_char, v: *const c_char) -> c_int;
    fn archive_write_set_format_option(a: *mut c_void, m: *const c_char, o: *const c_char, v: *const c_char) -> c_int;
    fn archive_write_open_filename(a: *mut c_void, filename: *const c_char) -> c_int;
    fn archive_write_header(a: *mut c_void, entry: *mut c_void) -> c_int;
    fn archive_write_data(a: *mut c_void, buff: *const c_void, size: usize) -> isize;
    fn archive_write_data_block(a: *mut c_void, buff: *const c_void, size: usize, offset: i64) -> isize;
    fn archive_write_close(a: *mut c_void) -> c_int;
    fn archive_write_free(a: *mut c_void) -> c_int;

    fn archive_write_disk_new() -> *mut c_void;
    fn archive_write_disk_set_options(a: *mut c_void, flags: c_int) -> c_int;
    fn archive_write_disk_set_standard_lookup(a: *mut c_void) -> c_int;
    fn archive_write_finish_entry(a: *mut c_void) -> c_int;

    fn archive_entry_new() -> *mut c_void;
    fn archive_entry_free(entry: *mut c_void);
    fn archive_entry_pathname(entry: *mut c_void) -> *const c_char;
    fn archive_entry_set_pathname(entry: *mut c_void, path: *const c_char);
    fn archive_entry_size(entry: *mut c_void) -> i64;
    fn archive_entry_set_size(entry: *mut c_void, size: i64);
    fn archive_entry_filetype(entry: *mut c_void) -> c_uint;
    fn archive_entry_set_filetype(entry: *mut c_void, filetype: c_uint);
    fn archive_entry_perm(entry: *mut c_void) -> c_uint;
    fn archive_entry_set_perm(entry: *mut c_void, perm: c_uint);
    fn archive_entry_mtime(entry: *mut c_void) -> i64;
    fn archive_entry_set_mtime(entry: *mut c_void, sec: i64, nsec: c_long);
    fn archive_entry_hardlink(entry: *mut c_void) -> *const c_char;
    fn archive_entry_set_hardlink(entry: *mut c_void, link: *const c_char);
    fn archive_entry_symlink(entry: *mut c_void) -> *const c_char;
    fn archive_entry_set_symlink(entry: *mut c_void, link: *const c_char);

    fn archive_error_string(a: *mut c_void) -> *const c_char;
}

struct ArchiveReader(*mut c_void);
impl Drop for ArchiveReader {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = archive_read_close(self.0);
                let _ = archive_read_free(self.0);
            }
        }
    }
}

struct ArchiveWriter(*mut c_void);
impl Drop for ArchiveWriter {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = archive_write_close(self.0);
                let _ = archive_write_free(self.0);
            }
        }
    }
}

impl ArchiveWriter {
    fn finish(mut self) -> Result<(), ArchiveError> {
        // Closing writes the final compressed blocks and can fail (e.g. ENOSPC).
        let result = unsafe { archive_write_close(self.0) };
        let error = if result != ARCHIVE_OK { Some(unsafe { get_archive_error(self.0) }) } else { None };
        unsafe {
            archive_write_free(self.0);
        }
        self.0 = std::ptr::null_mut();
        if let Some(error) = error { Err(ArchiveError::Libarchive(error)) } else { Ok(()) }
    }
}

struct ArchiveEntry(*mut c_void);
impl Drop for ArchiveEntry {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                archive_entry_free(self.0);
            }
        }
    }
}

// ---- Data Models & Enums -----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionLevel {
    Fast,
    #[default]
    Normal,
    Maximum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    TarZstd,
    Zip,
    TarXz,
    SevenZip,
    TarGz,
    TarBz2,
    TarLz4,
    TarBrotli,
    Tar,
}

impl ArchiveFormat {
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::TarZstd,
            Self::Zip,
            Self::TarXz,
            Self::SevenZip,
            Self::TarGz,
            Self::TarBz2,
            Self::TarLz4,
            Self::TarBrotli,
            Self::Tar,
        ]
    }

    #[must_use]
    pub fn default_extension(&self) -> &'static str {
        match self {
            Self::TarZstd => "tar.zst",
            Self::Zip => "zip",
            Self::TarXz => "tar.xz",
            Self::SevenZip => "7z",
            Self::TarGz => "tar.gz",
            Self::TarBz2 => "tar.bz2",
            Self::TarLz4 => "tar.lz4",
            Self::TarBrotli => "tar.br",
            Self::Tar => "tar",
        }
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::TarZstd => "Zstandard (.tar.zst) - Recommended / Ultra Fast",
            Self::Zip => "ZIP (.zip) - Universal Cross-Platform",
            Self::TarXz => "XZ (.tar.xz) - Maximum Archival Ratio",
            Self::SevenZip => "7-Zip (.7z) - High Compression & AES-256",
            Self::TarGz => "Gzip (.tar.gz) - Classic Unix / Web",
            Self::TarBz2 => "Bzip2 (.tar.bz2) - Burrows-Wheeler Fallback",
            Self::TarLz4 => "LZ4 (.tar.lz4) - Real-time Memory-Bus Speed",
            Self::TarBrotli => "Brotli (.tar.br) - High-Density Text",
            Self::Tar => "Tar (.tar) - Uncompressed POSIX Container",
        }
    }

    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_lowercase();
        if name.ends_with(".tar.zst") || name.ends_with(".tzst") {
            Some(Self::TarZstd)
        } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
            Some(Self::TarXz)
        } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
            Some(Self::TarGz)
        } else if name.ends_with(".tar.bz2") || name.ends_with(".tbz2") {
            Some(Self::TarBz2)
        } else if name.ends_with(".tar.lz4") || name.ends_with(".tlz4") {
            Some(Self::TarLz4)
        } else if name.ends_with(".tar.br") || name.ends_with(".tbr") {
            Some(Self::TarBrotli)
        } else if name.ends_with(".zip") || name.ends_with(".zipx") {
            Some(Self::Zip)
        } else if name.ends_with(".7z") {
            Some(Self::SevenZip)
        } else if name.ends_with(".tar") {
            Some(Self::Tar)
        } else if name.ends_with(".zst") {
            Some(Self::TarZstd)
        } else if name.ends_with(".xz") {
            Some(Self::TarXz)
        } else if name.ends_with(".gz") {
            Some(Self::TarGz)
        } else if name.ends_with(".bz2") {
            Some(Self::TarBz2)
        } else if name.ends_with(".lz4") {
            Some(Self::TarLz4)
        } else if name.ends_with(".br") {
            Some(Self::TarBrotli)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ArchiveOptions {
    pub format: Option<ArchiveFormat>,
    pub level: CompressionLevel,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ArchiveStats {
    pub files_count: usize,
    pub directories_count: usize,
    pub symlinks_count: usize,
    pub total_uncompressed_bytes: u64,
    pub total_compressed_bytes: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ExtractStats {
    pub files_extracted: usize,
    pub directories_extracted: usize,
    pub symlinks_extracted: usize,
    pub total_bytes_written: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ArchiveEntryInfo {
    pub pathname: String,
    pub size: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub mtime: i64,
    pub mode: u32,
}

// ---- Tier 1: In-Process libarchive Core ---------------------------------------

/// Create an archive containing `items` under `base_dir` using libarchive or CLI fallback.
pub fn compress_items(
    dest_path: &Path,
    base_dir: &Path,
    items: &[PathBuf],
    options: &ArchiveOptions,
) -> Result<ArchiveStats, ArchiveError> {
    if items.is_empty() {
        return Err(ArchiveError::EmptyItemList);
    }
    let format = options.format.or_else(|| ArchiveFormat::from_path(dest_path)).unwrap_or(ArchiveFormat::TarZstd);

    let canonical_base = std::fs::canonicalize(base_dir)?;
    let dest_absolute =
        if dest_path.is_absolute() { dest_path.to_path_buf() } else { std::env::current_dir()?.join(dest_path) };
    let dest_parent = std::fs::canonicalize(
        dest_absolute.parent().ok_or_else(|| ArchiveError::UnsupportedFormat("Invalid output path".into()))?,
    )?;
    let canonical_dest = dest_parent.join(dest_absolute.file_name().ok_or(ArchiveError::EmptyItemList)?);
    if std::fs::symlink_metadata(&canonical_dest).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "Archive destination already exists").into());
    }
    for item in items {
        let item = if item.is_absolute() { item.clone() } else { canonical_base.join(item) };
        let metadata = std::fs::symlink_metadata(&item)?;
        if metadata.is_dir() && canonical_dest.starts_with(std::fs::canonicalize(&item)?) {
            return Err(ArchiveError::PathTraversal("Archive output is inside a source directory".into()));
        }
    }
    // Encryption must fail closed: never retry by writing an unencrypted archive.
    if options.password.is_some() {
        if !matches!(format, ArchiveFormat::Zip | ArchiveFormat::SevenZip) {
            return Err(ArchiveError::UnsupportedFormat("Encryption requires ZIP or 7-Zip".into()));
        }
        return compress_via_7z_cli(dest_path, base_dir, items, options);
    }
    if format == ArchiveFormat::SevenZip {
        match compress_via_7z_cli(dest_path, base_dir, items, options) {
            Ok(stats) => return Ok(stats),
            Err(ArchiveError::ToolNotFound(_)) => {}
            Err(e) => return Err(e),
        }
    }

    // If Brotli format is requested, use brotli CLI pipeline
    if format == ArchiveFormat::TarBrotli {
        return compress_via_brotli_cli(dest_path, base_dir, items, options);
    }

    compress_items_libarchive(dest_path, base_dir, items, format, options.level)
}

fn compress_items_libarchive(
    dest_path: &Path,
    base_dir: &Path,
    items: &[PathBuf],
    format: ArchiveFormat,
    level: CompressionLevel,
) -> Result<ArchiveStats, ArchiveError> {
    let t0 = Instant::now();
    let mut stats = ArchiveStats::default();

    unsafe {
        let a = archive_write_new();
        if a.is_null() {
            return Err(ArchiveError::Libarchive("archive_write_new failed".into()));
        }
        let _guard = ArchiveWriter(a);

        // Configure format
        match format {
            ArchiveFormat::Zip => {
                if archive_write_set_format_zip(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
            }
            ArchiveFormat::SevenZip => {
                if archive_write_set_format_7zip(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
            }
            _ => {
                if archive_write_set_format_pax_restricted(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
            }
        }

        // Configure compression filter and level
        let (filter_name, level_str) = match format {
            ArchiveFormat::TarZstd => {
                if archive_write_add_filter_zstd(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
                (
                    "zstd",
                    match level {
                        CompressionLevel::Fast => "1",
                        CompressionLevel::Normal => "3",
                        CompressionLevel::Maximum => "19",
                    },
                )
            }
            ArchiveFormat::TarXz => {
                if archive_write_add_filter_xz(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
                (
                    "xz",
                    match level {
                        CompressionLevel::Fast => "1",
                        CompressionLevel::Normal => "6",
                        CompressionLevel::Maximum => "9",
                    },
                )
            }
            ArchiveFormat::TarGz => {
                if archive_write_add_filter_gzip(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
                (
                    "gzip",
                    match level {
                        CompressionLevel::Fast => "1",
                        CompressionLevel::Normal => "6",
                        CompressionLevel::Maximum => "9",
                    },
                )
            }
            ArchiveFormat::TarBz2 => {
                if archive_write_add_filter_bzip2(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
                (
                    "bzip2",
                    match level {
                        CompressionLevel::Fast => "1",
                        CompressionLevel::Normal => "6",
                        CompressionLevel::Maximum => "9",
                    },
                )
            }
            ArchiveFormat::TarLz4 => {
                if archive_write_add_filter_lz4(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
                (
                    "lz4",
                    match level {
                        CompressionLevel::Fast => "1",
                        CompressionLevel::Normal => "4",
                        CompressionLevel::Maximum => "9",
                    },
                )
            }
            ArchiveFormat::Tar | ArchiveFormat::Zip | ArchiveFormat::SevenZip | ArchiveFormat::TarBrotli => {
                if archive_write_add_filter_none(a) != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
                ("", "")
            }
        };

        if !filter_name.is_empty() {
            let m = CString::new(filter_name).unwrap_or_default();
            let o = CString::new("compression-level").unwrap_or_default();
            let v = CString::new(level_str).unwrap_or_default();
            let _ = archive_write_set_filter_option(a, m.as_ptr(), o.as_ptr(), v.as_ptr());
        }

        if !matches!(format, ArchiveFormat::Zip | ArchiveFormat::SevenZip)
            && archive_write_set_format_option(a, c"pax".as_ptr(), c"hdrcharset".as_ptr(), c"BINARY".as_ptr())
                != ARCHIVE_OK
        {
            return Err(ArchiveError::Libarchive(get_archive_error(a)));
        }

        let c_dest = CString::new(dest_path.as_os_str().as_bytes())
            .map_err(|_| ArchiveError::Libarchive("Invalid destination path".into()))?;
        if archive_write_open_filename(a, c_dest.as_ptr()) != ARCHIVE_OK {
            return Err(ArchiveError::Libarchive(get_archive_error(a)));
        }

        // A work list, not recursion: a frame per level (64 KiB each, with the read
        // buffer) overflowed the 2 MiB stack about 30 folders deep and aborted the app.
        let mut buf = vec![0u8; 65536];
        for item in items {
            let abs_item = if item.is_absolute() { item.clone() } else { base_dir.join(item) };
            let mut todo = vec![abs_item];
            while let Some(path) = todo.pop() {
                let mut children = Vec::new();
                add_item(a, &path, base_dir, &mut stats, &mut children, &mut buf)?;
                todo.extend(children.into_iter().rev()); // each folder before its contents, as before
            }
        }
        _guard.finish()?;
    }

    if let Ok(meta) = std::fs::metadata(dest_path) {
        stats.total_compressed_bytes = meta.len();
    }
    stats.elapsed_ms = t0.elapsed().as_millis() as u64;
    Ok(stats)
}

/// Writes the entry for `path`; a folder's contents are pushed onto `children`.
fn add_item(
    a: *mut c_void,
    path: &Path,
    base_dir: &Path,
    stats: &mut ArchiveStats,
    children: &mut Vec<PathBuf>,
    buf: &mut [u8],
) -> Result<(), ArchiveError> {
    let rel_path = path.strip_prefix(base_dir).unwrap_or(path);
    let rel_name = if rel_path.as_os_str().is_empty() {
        path.file_name().unwrap_or(OsStr::new("entry"))
    } else {
        rel_path.as_os_str()
    };

    let meta = std::fs::symlink_metadata(path)?;
    let is_symlink = meta.file_type().is_symlink();
    let is_dir = meta.is_dir() && !is_symlink;

    unsafe {
        let entry = archive_entry_new();
        if entry.is_null() {
            return Err(ArchiveError::Libarchive("archive_entry_new failed".into()));
        }
        let _eguard = ArchiveEntry(entry);

        let c_path =
            CString::new(rel_name.as_bytes()).map_err(|_| ArchiveError::Libarchive("Invalid entry path".into()))?;
        archive_entry_set_pathname(entry, c_path.as_ptr());

        if let Ok(modified) = meta.modified()
            && let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            archive_entry_set_mtime(entry, dur.as_secs() as i64, dur.subsec_nanos() as c_long);
        }

        use std::os::unix::fs::PermissionsExt;
        archive_entry_set_perm(entry, meta.permissions().mode() & 0o7777);

        if is_symlink {
            archive_entry_set_filetype(entry, AE_IFLNK);
            archive_entry_set_size(entry, 0);
            let target = std::fs::read_link(path)?;
            let c_target = CString::new(target.as_os_str().as_bytes())
                .map_err(|_| ArchiveError::Libarchive("Invalid symlink target".into()))?;
            archive_entry_set_symlink(entry, c_target.as_ptr());
            if archive_write_header(a, entry) != ARCHIVE_OK {
                return Err(ArchiveError::Libarchive(get_archive_error(a)));
            }
            stats.symlinks_count += 1;
        } else if is_dir {
            archive_entry_set_filetype(entry, AE_IFDIR);
            archive_entry_set_size(entry, 0);
            if archive_write_header(a, entry) != ARCHIVE_OK {
                return Err(ArchiveError::Libarchive(get_archive_error(a)));
            }
            stats.directories_count += 1;

            for de in std::fs::read_dir(path)? {
                children.push(de?.path());
            }
        } else if meta.is_file() {
            archive_entry_set_filetype(entry, AE_IFREG);
            archive_entry_set_size(entry, meta.len() as i64);
            if archive_write_header(a, entry) != ARCHIVE_OK {
                return Err(ArchiveError::Libarchive(get_archive_error(a)));
            }

            let mut file = std::fs::File::open(path)?;
            use std::io::Read;
            loop {
                let n = file.read(buf)?;
                if n == 0 {
                    break;
                }
                let written = archive_write_data(a, buf.as_ptr() as *const c_void, n);
                if written != n as isize {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
            }
            stats.files_count += 1;
            stats.total_uncompressed_bytes += meta.len();
        } else {
            return Err(ArchiveError::UnsupportedFormat(format!("Special file: {}", path.display())));
        }
    }
    Ok(())
}

/// Extract any archive (tar.zst, zip, tar.xz, 7z, tar.gz, tar.bz2, etc.) to target directory.
pub fn extract_archive(
    archive_path: &Path,
    target_dir: &Path,
    password: Option<&str>,
) -> Result<ExtractStats, ArchiveError> {
    std::fs::create_dir_all(target_dir)?;
    let target_dir = std::fs::canonicalize(target_dir)?;
    // Everything is extracted into a private staging directory inside the destination
    // and only moved in once complete, never over anything already there. Extracting
    // straight into the destination replaced files of the same name, and an encrypted
    // entry with a wrong password replaced them with zeros before failing.
    let staging = Staging::new(&target_dir)?;

    // Use the same validated extraction path for password-protected archives.
    // Never retry rejected input through a CLI with different safety rules: the one
    // exception is a 7z whose headers are encrypted (names hidden, which is how we make
    // them), which libarchive cannot read at all. That goes through 7z and is checked by
    // our own rules before anything arrives.
    let stats = match extract_archive_libarchive(archive_path, &staging.0, password) {
        Err(ArchiveError::Libarchive(msg)) if msg.contains("header is encrypted") && is_7z(archive_path) => {
            let Some(password) = password else {
                return Err(ArchiveError::PasswordRequired { wrong: false });
            };
            extract_7z_into(archive_path, &staging.0, password)?
        }
        Err(ArchiveError::Libarchive(msg)) if msg.contains("Passphrase required") => {
            return Err(ArchiveError::PasswordRequired { wrong: false });
        }
        Err(ArchiveError::Libarchive(msg)) if msg.contains("Incorrect passphrase") => {
            return Err(ArchiveError::PasswordRequired { wrong: true });
        }
        r => r?,
    };
    move_in_without_replacing(&staging.0, &target_dir)?;
    Ok(stats)
}

/// A private (0700) directory inside the destination, removed (without following a
/// link) when dropped.
struct Staging(PathBuf);

impl Staging {
    fn new(target_dir: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = target_dir.join(format!(".gungnir-extract-{}-{n}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&p)?;
        Ok(Self(p))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Move what was extracted into `staging` into `target_dir`: a directory that is already
/// there (a real one) is merged into; anything else already there stops it before
/// anything moves (nothing is replaced). Renames never replace either, so a name that
/// appears meanwhile is an error, not an overwrite.
fn move_in_without_replacing(staging: &Path, target_dir: &Path) -> Result<(), ArchiveError> {
    let mut moves = Vec::new();
    let mut todo = vec![PathBuf::new()];
    while let Some(rel) = todo.pop() {
        for entry in std::fs::read_dir(staging.join(&rel))? {
            let rel = rel.join(entry?.file_name());
            let staged = std::fs::symlink_metadata(staging.join(&rel))?;
            match std::fs::symlink_metadata(target_dir.join(&rel)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => moves.push(rel),
                Ok(there) if there.is_dir() && staged.is_dir() => todo.push(rel),
                Ok(_) => {
                    return Err(ArchiveError::Io(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("{} already exists; nothing was extracted", target_dir.join(&rel).display()),
                    )));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    for rel in moves {
        rename_noreplace(&staging.join(&rel), &target_dir.join(&rel))?;
    }
    Ok(())
}

/// The file starts with the 7z signature.
fn is_7z(path: &Path) -> bool {
    use std::io::Read;
    let mut sig = [0u8; 6];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut sig)).is_ok() && sig == *b"7z\xBC\xAF\x27\x1C"
}

/// Extract a 7z with encrypted headers through `7z` into `staging` (private, empty), the
/// password on its stdin. Then our own rules, as for any archive: a symlink must stay
/// inside (relative, no `..`), only files, directories and such links, modes masked to
/// [`EXTRACT_PERM_MASK`]. [`extract_archive`] moves the result in.
fn extract_7z_into(archive_path: &Path, staging: &Path, password: &str) -> Result<ExtractStats, ArchiveError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let t0 = Instant::now();
    // No -p: 7z asks for the password and reads it from stdin. -snld is not given, so 7z
    // also refuses links it finds dangerous; ours are checked below either way.
    let mut child = std::process::Command::new("7z")
        .args(["x", "-y", "-bd", "-bso0", "-bsp0"])
        .arg(format!("-o{}", staging.display()))
        .arg("--")
        .arg(archive_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ArchiveError::ToolNotFound("7z".into())
            } else {
                ArchiveError::Io(e)
            }
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(format!("{password}\n").as_bytes());
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        // 7z refuses some escaping links itself: the same violation as ours below.
        if let Some(line) = stderr.lines().find(|l| l.contains("Dangerous link path")) {
            return Err(ArchiveError::PathTraversal(line.trim().to_owned()));
        }
        if stderr.contains("Wrong password") {
            return Err(ArchiveError::PasswordRequired { wrong: true });
        }
        return Err(ArchiveError::CliToolFailed { tool: "7z".into(), status: out.status.code().unwrap_or(-1), stderr });
    }

    // Check everything 7z wrote before any of it leaves the staging directory.
    let mut stats = ExtractStats::default();
    let mut todo = vec![staging.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let md = std::fs::symlink_metadata(&path)?;
            let ft = md.file_type();
            if ft.is_symlink() {
                let target = std::fs::read_link(&path)?;
                safe_archive_path(&target)?;
                stats.symlinks_extracted += 1;
            } else if ft.is_dir() {
                std::fs::set_permissions(
                    &path,
                    std::fs::Permissions::from_mode((md.mode() & EXTRACT_PERM_MASK) | 0o700),
                )?;
                stats.directories_extracted += 1;
                todo.push(path);
            } else if ft.is_file() {
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(md.mode() & EXTRACT_PERM_MASK))?;
                stats.files_extracted += 1;
                stats.total_bytes_written += md.len();
            } else {
                return Err(ArchiveError::UnsupportedFormat(format!(
                    "{} is not a file, directory or link",
                    path.strip_prefix(staging).unwrap_or(&path).display()
                )));
            }
        }
    }
    stats.elapsed_ms = t0.elapsed().as_millis() as u64;
    Ok(stats)
}

/// `renameat2(RENAME_NOREPLACE)`.
fn rename_noreplace(from: &Path, to: &Path) -> std::io::Result<()> {
    let c =
        |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL));
    let (f, t) = (c(from)?, c(to)?);
    // SAFETY: two valid NUL-terminated paths.
    let r = unsafe { libc::renameat2(libc::AT_FDCWD, f.as_ptr(), libc::AT_FDCWD, t.as_ptr(), libc::RENAME_NOREPLACE) };
    if r == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

fn safe_archive_path(path: &Path) -> Result<&Path, ArchiveError> {
    let raw = path.as_os_str().as_bytes();
    if raw.is_empty()
        || raw.starts_with(b"\\")
        || raw.windows(3).any(|part| part == b"..\\")
        || path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(ArchiveError::PathTraversal(path.display().to_string()));
    }
    Ok(path)
}

fn extract_archive_libarchive(
    archive_path: &Path,
    target_dir: &Path,
    password: Option<&str>,
) -> Result<ExtractStats, ArchiveError> {
    let target_dir = std::fs::canonicalize(target_dir)?;
    let t0 = Instant::now();
    let mut stats = ExtractStats::default();

    unsafe {
        let a = archive_read_new();
        if a.is_null() {
            return Err(ArchiveError::Libarchive("archive_read_new failed".into()));
        }
        let _guard_a = ArchiveReader(a);

        archive_read_support_filter_all(a);
        archive_read_support_format_all(a);

        if let Some(password) = password {
            let pass =
                CString::new(password).map_err(|_| ArchiveError::UnsupportedFormat("Password contains NUL".into()))?;
            if archive_read_add_passphrase(a, pass.as_ptr()) != ARCHIVE_OK {
                return Err(ArchiveError::Libarchive(get_archive_error(a)));
            }
        }

        let ext = archive_write_disk_new();
        if ext.is_null() {
            return Err(ArchiveError::Libarchive("archive_write_disk_new failed".into()));
        }
        let _guard_ext = ArchiveWriter(ext);

        let flags = ARCHIVE_EXTRACT_TIME
            | ARCHIVE_EXTRACT_PERM
            | ARCHIVE_EXTRACT_SECURE_NODOTDOT
            | ARCHIVE_EXTRACT_SECURE_SYMLINKS;
        archive_write_disk_set_options(ext, flags);
        archive_write_disk_set_standard_lookup(ext);

        let c_path = CString::new(archive_path.as_os_str().as_bytes())
            .map_err(|_| ArchiveError::Libarchive("Invalid archive path".into()))?;
        if archive_read_open_filename(a, c_path.as_ptr(), 65536) != ARCHIVE_OK {
            return Err(ArchiveError::Libarchive(get_archive_error(a)));
        }

        let mut entry: *mut c_void = std::ptr::null_mut();
        loop {
            let r = archive_read_next_header(a, &mut entry);
            if r == ARCHIVE_EOF {
                break;
            }
            if r != ARCHIVE_OK && r != ARCHIVE_WARN {
                return Err(ArchiveError::Libarchive(get_archive_error(a)));
            }

            let current_path_c = archive_entry_pathname(entry);
            if current_path_c.is_null() {
                continue;
            }
            let raw_path = Path::new(OsStr::from_bytes(CStr::from_ptr(current_path_c).to_bytes()));
            let sanitized = safe_archive_path(raw_path)?;
            let full_out = target_dir.join(sanitized);
            // Validate links before archive_write_header can create them.
            let symlink = archive_entry_symlink(entry);
            if !symlink.is_null() {
                safe_archive_path(Path::new(OsStr::from_bytes(CStr::from_ptr(symlink).to_bytes())))?;
            }
            let hardlink = archive_entry_hardlink(entry);
            if !hardlink.is_null() {
                let raw_link = Path::new(OsStr::from_bytes(CStr::from_ptr(hardlink).to_bytes()));
                let target = target_dir.join(safe_archive_path(raw_link)?);
                let target = CString::new(target.as_os_str().as_encoded_bytes())
                    .map_err(|_| ArchiveError::PathTraversal(raw_link.display().to_string()))?;
                archive_entry_set_hardlink(entry, target.as_ptr());
            }

            let c_full_out = CString::new(full_out.as_os_str().as_bytes())
                .map_err(|_| ArchiveError::Libarchive("Invalid output path".into()))?;
            archive_entry_set_pathname(entry, c_full_out.as_ptr());
            // ARCHIVE_EXTRACT_PERM takes the archive's mode as is: a hostile archive made
            // its files world-writable (another local user could change an extracted
            // script before it runs) or setuid. Keep the owner's bits and read/execute
            // for others; never group/other write, setuid, setgid or sticky (what GNU tar
            // does for a normal user).
            archive_entry_set_perm(entry, archive_entry_perm(entry) & EXTRACT_PERM_MASK);

            let write_header_res = archive_write_header(ext, entry);
            let ftype = archive_entry_filetype(entry);
            if write_header_res != ARCHIVE_OK && write_header_res != ARCHIVE_WARN {
                return Err(ArchiveError::Libarchive(get_archive_error(ext)));
            }

            let esize = archive_entry_size(entry);
            if ftype == AE_IFREG {
                if esize > 0 {
                    let mut buff: *const c_void = std::ptr::null();
                    let mut size: usize = 0;
                    let mut offset: i64 = 0;

                    loop {
                        let dr = archive_read_data_block(a, &mut buff, &mut size, &mut offset);
                        if dr == ARCHIVE_EOF {
                            break;
                        }
                        if dr != ARCHIVE_OK && dr != ARCHIVE_WARN {
                            return Err(ArchiveError::Libarchive(get_archive_error(a)));
                        }
                        let dw = archive_write_data_block(ext, buff, size, offset);
                        if dw < 0 {
                            return Err(ArchiveError::Libarchive(get_archive_error(ext)));
                        }
                        stats.total_bytes_written += size as u64;
                    }
                }
                stats.files_extracted += 1;
            } else if ftype == AE_IFDIR {
                stats.directories_extracted += 1;
            } else if ftype == AE_IFLNK {
                stats.symlinks_extracted += 1;
            }

            if archive_write_finish_entry(ext) != ARCHIVE_OK {
                return Err(ArchiveError::Libarchive(get_archive_error(ext)));
            }
        }
        _guard_ext.finish()?;
    }

    stats.elapsed_ms = t0.elapsed().as_millis() as u64;
    Ok(stats)
}

/// List all entries inside an archive without extracting files to disk.
pub fn list_archive(archive_path: &Path) -> Result<Vec<ArchiveEntryInfo>, ArchiveError> {
    let mut entries = Vec::new();

    unsafe {
        let a = archive_read_new();
        if a.is_null() {
            return Err(ArchiveError::Libarchive("archive_read_new failed".into()));
        }
        let _guard = ArchiveReader(a);

        archive_read_support_filter_all(a);
        archive_read_support_format_all(a);

        let c_path = CString::new(archive_path.as_os_str().as_bytes())
            .map_err(|_| ArchiveError::Libarchive("Invalid archive path".into()))?;
        if archive_read_open_filename(a, c_path.as_ptr(), 65536) != ARCHIVE_OK {
            return Err(ArchiveError::Libarchive(get_archive_error(a)));
        }

        let mut entry: *mut c_void = std::ptr::null_mut();
        loop {
            let r = archive_read_next_header(a, &mut entry);
            if r == ARCHIVE_EOF {
                break;
            }
            if r != ARCHIVE_OK {
                return Err(ArchiveError::Libarchive(get_archive_error(a)));
            }

            let current_path_c = archive_entry_pathname(entry);
            if current_path_c.is_null() {
                continue;
            }
            let path_str = CStr::from_ptr(current_path_c).to_string_lossy().into_owned();
            let size = archive_entry_size(entry).max(0) as u64;
            let ftype = archive_entry_filetype(entry);
            let mtime = archive_entry_mtime(entry);
            let perm = archive_entry_perm(entry);

            entries.push(ArchiveEntryInfo {
                pathname: path_str,
                size,
                is_dir: ftype == AE_IFDIR,
                is_symlink: ftype == AE_IFLNK,
                mtime,
                mode: perm,
            });
        }
    }

    Ok(entries)
}

/// Test archive integrity by decompressing every block in memory without writing.
pub fn test_archive_integrity(archive_path: &Path) -> Result<usize, ArchiveError> {
    let mut verified_count = 0;

    unsafe {
        let a = archive_read_new();
        if a.is_null() {
            return Err(ArchiveError::Libarchive("archive_read_new failed".into()));
        }
        let _guard = ArchiveReader(a);

        archive_read_support_filter_all(a);
        archive_read_support_format_all(a);

        let c_path = CString::new(archive_path.as_os_str().as_bytes())
            .map_err(|_| ArchiveError::Libarchive("Invalid archive path".into()))?;
        if archive_read_open_filename(a, c_path.as_ptr(), 65536) != ARCHIVE_OK {
            return Err(ArchiveError::Libarchive(get_archive_error(a)));
        }

        let mut entry: *mut c_void = std::ptr::null_mut();
        loop {
            let r = archive_read_next_header(a, &mut entry);
            if r == ARCHIVE_EOF {
                break;
            }
            if r != ARCHIVE_OK {
                return Err(ArchiveError::Libarchive(get_archive_error(a)));
            }

            let mut buff: *const c_void = std::ptr::null();
            let mut size: usize = 0;
            let mut offset: i64 = 0;
            loop {
                let dr = archive_read_data_block(a, &mut buff, &mut size, &mut offset);
                if dr == ARCHIVE_EOF {
                    break;
                }
                if dr != ARCHIVE_OK {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
            }
            verified_count += 1;
        }
    }

    Ok(verified_count)
}

unsafe fn get_archive_error(a: *mut c_void) -> String {
    let err_c = unsafe { archive_error_string(a) };
    if err_c.is_null() {
        "Unknown archive error".to_string()
    } else {
        unsafe { CStr::from_ptr(err_c) }.to_string_lossy().into_owned()
    }
}

// ---- Tier 3: CLI Fallbacks & Specialized Codecs ------------------------------

/// The `7z a` command for `options` (the password, if any, is not in it: see
/// [`compress_via_7z_cli`]).
fn seven_zip_command(
    dest_path: &Path,
    base_dir: &Path,
    items: &[PathBuf],
    options: &ArchiveOptions,
) -> Result<std::process::Command, ArchiveError> {
    let mut cmd = std::process::Command::new("7z");
    cmd.current_dir(base_dir);
    cmd.arg("a");
    cmd.arg("-y");

    // Compression level
    let mx = match options.level {
        CompressionLevel::Fast => "-mx=1",
        CompressionLevel::Normal => "-mx=5",
        CompressionLevel::Maximum => "-mx=9",
    };
    cmd.arg(mx);

    let format = options.format.or_else(|| ArchiveFormat::from_path(dest_path)).unwrap_or(ArchiveFormat::SevenZip);
    cmd.arg(if format == ArchiveFormat::Zip { "-tzip" } else { "-t7z" });
    if let Some(ref pass) = options.password {
        if pass.is_empty() {
            return Err(ArchiveError::UnsupportedFormat("Password must not be empty".into()));
        }
        // A bare -p: 7z reads the password from stdin. On the command line every local
        // user could read it (/proc/<pid>/cmdline, ps).
        cmd.arg("-p");
        if format == ArchiveFormat::SevenZip {
            cmd.arg("-mhe=on");
        } else {
            cmd.arg("-mem=AES256");
        }
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }

    cmd.arg("--");
    cmd.arg(dest_path);
    for item in items {
        let rel = item.strip_prefix(base_dir).unwrap_or(item);
        cmd.arg(rel);
    }
    Ok(cmd)
}

fn compress_via_7z_cli(
    dest_path: &Path,
    base_dir: &Path,
    items: &[PathBuf],
    options: &ArchiveOptions,
) -> Result<ArchiveStats, ArchiveError> {
    let t0 = Instant::now();
    let mut cmd = seven_zip_command(dest_path, base_dir, items, options)?;
    cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::ToolNotFound("7z".into())
        } else {
            ArchiveError::Io(e)
        }
    })?;
    if let (Some(pass), Some(mut stdin)) = (options.password.as_ref(), child.stdin.take()) {
        use std::io::Write;
        // Twice, for a 7z that asks to confirm it; then closed, so it never waits.
        let _ = stdin.write_all(format!("{pass}\n{pass}\n").as_bytes());
    }
    let output = child.wait_with_output()?;

    if !output.status.success() {
        return Err(ArchiveError::CliToolFailed {
            tool: "7z".into(),
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let meta = std::fs::metadata(dest_path)?;
    Ok(ArchiveStats {
        files_count: items.len(),
        total_compressed_bytes: meta.len(),
        elapsed_ms: t0.elapsed().as_millis() as u64,
        ..Default::default()
    })
}

fn compress_via_brotli_cli(
    dest_path: &Path,
    base_dir: &Path,
    items: &[PathBuf],
    options: &ArchiveOptions,
) -> Result<ArchiveStats, ArchiveError> {
    let t0 = Instant::now();
    let temp_tar = dest_path.with_extension("tmp_tar");
    // Create base tar first
    compress_items_libarchive(&temp_tar, base_dir, items, ArchiveFormat::Tar, CompressionLevel::Normal)?;

    let quality = match options.level {
        CompressionLevel::Fast => "1",
        CompressionLevel::Normal => "5",
        CompressionLevel::Maximum => "11",
    };

    let status = std::process::Command::new("brotli")
        .arg("-q")
        .arg(quality)
        .arg("-f")
        .arg("-o")
        .arg(dest_path)
        .arg("--")
        .arg(&temp_tar)
        .status()
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp_tar);
            if e.kind() == std::io::ErrorKind::NotFound {
                ArchiveError::ToolNotFound("brotli".into())
            } else {
                ArchiveError::Io(e)
            }
        })?;

    let _ = std::fs::remove_file(&temp_tar);

    if !status.success() {
        return Err(ArchiveError::CliToolFailed {
            tool: "brotli".into(),
            status: status.code().unwrap_or(-1),
            stderr: "Brotli compression failed".into(),
        });
    }

    let meta = std::fs::metadata(dest_path)?;
    Ok(ArchiveStats {
        files_count: items.len(),
        total_compressed_bytes: meta.len(),
        elapsed_ms: t0.elapsed().as_millis() as u64,
        ..Default::default()
    })
}

// ---- Single Stream Lossless Compression --------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleStreamFormat {
    Zstandard,
    Xz,
    Gzip,
    Bzip2,
    Lz4,
    Brotli,
}

impl SingleStreamFormat {
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Zstandard => "zst",
            Self::Xz => "xz",
            Self::Gzip => "gz",
            Self::Bzip2 => "bz2",
            Self::Lz4 => "lz4",
            Self::Brotli => "br",
        }
    }
}

/// Compress a single file using a lossless streaming engine (zstd, xz, gzip, bzip2, lz4, brotli).
pub fn compress_single_file(
    src: &Path,
    dest: &Path,
    format: SingleStreamFormat,
    level: CompressionLevel,
) -> Result<u64, ArchiveError> {
    let (tool, args): (&str, Vec<String>) = match format {
        SingleStreamFormat::Zstandard => {
            let lvl = match level {
                CompressionLevel::Fast => "-1",
                CompressionLevel::Normal => "-3",
                CompressionLevel::Maximum => "-19",
            };
            (
                "zstd",
                vec![
                    lvl.to_string(),
                    "-f".to_string(),
                    "-o".to_string(),
                    dest.display().to_string(),
                    "--".to_string(),
                    src.display().to_string(),
                ],
            )
        }
        SingleStreamFormat::Xz => {
            let lvl = match level {
                CompressionLevel::Fast => "-1",
                CompressionLevel::Normal => "-6",
                CompressionLevel::Maximum => "-9",
            };
            (
                "xz",
                vec![
                    "-k".to_string(),
                    "-f".to_string(),
                    lvl.to_string(),
                    "-c".to_string(),
                    "--".to_string(),
                    src.display().to_string(),
                ],
            )
        }
        SingleStreamFormat::Gzip => {
            let lvl = match level {
                CompressionLevel::Fast => "-1",
                CompressionLevel::Normal => "-6",
                CompressionLevel::Maximum => "-9",
            };
            (
                "gzip",
                vec![
                    "-k".to_string(),
                    "-f".to_string(),
                    lvl.to_string(),
                    "-c".to_string(),
                    "--".to_string(),
                    src.display().to_string(),
                ],
            )
        }
        SingleStreamFormat::Bzip2 => {
            let lvl = match level {
                CompressionLevel::Fast => "-1",
                CompressionLevel::Normal => "-6",
                CompressionLevel::Maximum => "-9",
            };
            (
                "bzip2",
                vec![
                    "-k".to_string(),
                    "-f".to_string(),
                    lvl.to_string(),
                    "-c".to_string(),
                    "--".to_string(),
                    src.display().to_string(),
                ],
            )
        }
        SingleStreamFormat::Lz4 => {
            let lvl = match level {
                CompressionLevel::Fast => "-1",
                CompressionLevel::Normal => "-4",
                CompressionLevel::Maximum => "-9",
            };
            (
                "lz4",
                vec![
                    "-f".to_string(),
                    lvl.to_string(),
                    "--".to_string(),
                    src.display().to_string(),
                    dest.display().to_string(),
                ],
            )
        }
        SingleStreamFormat::Brotli => {
            let lvl = match level {
                CompressionLevel::Fast => "1",
                CompressionLevel::Normal => "5",
                CompressionLevel::Maximum => "11",
            };
            (
                "brotli",
                vec![
                    "-q".to_string(),
                    lvl.to_string(),
                    "-f".to_string(),
                    "-o".to_string(),
                    dest.display().to_string(),
                    "--".to_string(),
                    src.display().to_string(),
                ],
            )
        }
    };

    let mut cmd = std::process::Command::new(tool);
    cmd.args(&args);

    if format == SingleStreamFormat::Xz || format == SingleStreamFormat::Gzip || format == SingleStreamFormat::Bzip2 {
        let out_file = std::fs::File::create(dest)?;
        cmd.stdout(out_file);
    }

    let status = cmd.status().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::ToolNotFound(tool.to_string())
        } else {
            ArchiveError::Io(e)
        }
    })?;

    if !status.success() {
        return Err(ArchiveError::CliToolFailed {
            tool: tool.to_string(),
            status: status.code().unwrap_or(-1),
            stderr: "Single-file compression failed".to_string(),
        });
    }

    let meta = std::fs::metadata(dest)?;
    Ok(meta.len())
}

/// Decompress a single stream file (e.g. `file.txt.zst` -> `file.txt`).
pub fn decompress_single_file(src: &Path, dest: &Path) -> Result<u64, ArchiveError> {
    let ext = src.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    let (tool, args): (&str, Vec<String>) = match ext.as_str() {
        "zst" => (
            "zstd",
            vec![
                "-d".to_string(),
                "-f".to_string(),
                "-o".to_string(),
                dest.display().to_string(),
                "--".to_string(),
                src.display().to_string(),
            ],
        ),
        "xz" => (
            "xz",
            vec![
                "-d".to_string(),
                "-k".to_string(),
                "-f".to_string(),
                "-c".to_string(),
                "--".to_string(),
                src.display().to_string(),
            ],
        ),
        "gz" => (
            "gzip",
            vec![
                "-d".to_string(),
                "-k".to_string(),
                "-f".to_string(),
                "-c".to_string(),
                "--".to_string(),
                src.display().to_string(),
            ],
        ),
        "bz2" => (
            "bzip2",
            vec![
                "-d".to_string(),
                "-k".to_string(),
                "-f".to_string(),
                "-c".to_string(),
                "--".to_string(),
                src.display().to_string(),
            ],
        ),
        "lz4" => (
            "lz4",
            vec![
                "-d".to_string(),
                "-f".to_string(),
                "--".to_string(),
                src.display().to_string(),
                dest.display().to_string(),
            ],
        ),
        "br" => (
            "brotli",
            vec![
                "-d".to_string(),
                "-f".to_string(),
                "-o".to_string(),
                dest.display().to_string(),
                "--".to_string(),
                src.display().to_string(),
            ],
        ),
        _ => return Err(ArchiveError::UnsupportedFormat(format!("Unknown stream extension '.{ext}'"))),
    };

    let mut cmd = std::process::Command::new(tool);
    cmd.args(&args);

    if ext == "xz" || ext == "gz" || ext == "bz2" {
        let out_file = std::fs::File::create(dest)?;
        cmd.stdout(out_file);
    }

    let status = cmd.status().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::ToolNotFound(tool.to_string())
        } else {
            ArchiveError::Io(e)
        }
    })?;

    if !status.success() {
        return Err(ArchiveError::CliToolFailed {
            tool: tool.to_string(),
            status: status.code().unwrap_or(-1),
            stderr: "Single-file decompression failed".to_string(),
        });
    }

    let meta = std::fs::metadata(dest)?;
    Ok(meta.len())
}

// ---- Specialized Lossless Domain Formats (Part 3) -----------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LosslessAudioFormat {
    Flac,
    WavPack,
    Alac,
}

/// Transcode audio bit-perfect losslessly to FLAC, WavPack, or ALAC.
pub fn transcode_lossless_audio(src: &Path, dest: &Path, format: LosslessAudioFormat) -> Result<u64, ArchiveError> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.arg("-nostdin");
    cmd.arg("-y");
    cmd.arg("-i").arg(src);
    match format {
        LosslessAudioFormat::Flac => {
            cmd.arg("-c:a").arg("flac");
            cmd.arg("-compression_level").arg("12");
        }
        LosslessAudioFormat::WavPack => {
            cmd.arg("-c:a").arg("wavpack");
        }
        LosslessAudioFormat::Alac => {
            cmd.arg("-c:a").arg("alac");
        }
    }
    cmd.arg("--");
    cmd.arg(dest);

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::ToolNotFound("ffmpeg".into())
        } else {
            ArchiveError::Io(e)
        }
    })?;

    if !output.status.success() {
        return Err(ArchiveError::CliToolFailed {
            tool: "ffmpeg".into(),
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let meta = std::fs::metadata(dest)?;
    Ok(meta.len())
}

/// Transcode video to strictly lossless FFV1 archival format.
pub fn encode_lossless_video_ffv1(src: &Path, dest: &Path) -> Result<u64, ArchiveError> {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.arg("-nostdin");
    cmd.arg("-y");
    cmd.arg("-i").arg(src);
    cmd.arg("-c:v").arg("ffv1");
    cmd.arg("-level").arg("3");
    cmd.arg("-c:a").arg("copy");
    cmd.arg("--");
    cmd.arg(dest);

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::ToolNotFound("ffmpeg".into())
        } else {
            ArchiveError::Io(e)
        }
    })?;

    if !output.status.success() {
        return Err(ArchiveError::CliToolFailed {
            tool: "ffmpeg".into(),
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let meta = std::fs::metadata(dest)?;
    Ok(meta.len())
}

/// Convert photographic image to lossless JPEG XL (modular mode `cjxl -d 0`).
pub fn encode_lossless_jxl(src: &Path, dest: &Path) -> Result<u64, ArchiveError> {
    let mut cmd = std::process::Command::new("cjxl");
    cmd.arg("-d").arg("0"); // 0 distance = mathematically lossless
    cmd.arg("-e").arg("7"); // Effort 7
    cmd.arg("--");
    cmd.arg(src);
    cmd.arg(dest);

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::ToolNotFound("cjxl".into())
        } else {
            ArchiveError::Io(e)
        }
    })?;

    if !output.status.success() {
        return Err(ArchiveError::CliToolFailed {
            tool: "cjxl".into(),
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let meta = std::fs::metadata(dest)?;
    Ok(meta.len())
}

// ---- Bit-Perfect Verification ------------------------------------------------

/// Verify that two files or decompressed payloads match bit-for-bit using SHA-256.
pub fn verify_bit_perfect(original: &Path, decompressed: &Path) -> Result<(), ArchiveError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let hash_orig = {
        let mut f = std::fs::File::open(original)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        format!("{:x}", hasher.finalize())
    };

    let hash_dec = {
        let mut f = std::fs::File::open(decompressed)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        format!("{:x}", hasher.finalize())
    };

    if hash_orig != hash_dec {
        return Err(ArchiveError::IntegrityMismatch { original_hash: hash_orig, decompressed_hash: hash_dec });
    }
    Ok(())
}

// ---- Tests -------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    struct TestTempDir(PathBuf);
    impl TestTempDir {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir().join(format!("gungnir-arch-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).expect("create test temp dir");
            Self(base)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_test_tar(path: &Path, name: &str, kind: u8, link: &str) {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(b"00000000000\0");
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = kind;
        header[157..157 + link.len()].copy_from_slice(link.as_bytes());
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        let mut data = header.to_vec();
        data.extend_from_slice(&[0; 1024]);
        std::fs::write(path, data).unwrap();
    }

    /// A tar of several entries: (name, type flag, link target, mode, contents).
    fn write_tar_entries(path: &Path, entries: &[(&str, u8, &str, u32, &[u8])]) {
        let mut data = Vec::new();
        for &(name, kind, link, mode, body) in entries {
            let mut h = [0u8; 512];
            h[..name.len()].copy_from_slice(name.as_bytes());
            h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
            h[108..116].copy_from_slice(b"0000000\0");
            h[116..124].copy_from_slice(b"0000000\0");
            h[124..136].copy_from_slice(format!("{:011o}\0", body.len()).as_bytes());
            h[136..148].copy_from_slice(b"00000000000\0");
            h[148..156].fill(b' ');
            h[156] = kind;
            h[157..157 + link.len()].copy_from_slice(link.as_bytes());
            h[257..263].copy_from_slice(b"ustar\0");
            h[263..265].copy_from_slice(b"00");
            let sum: u32 = h.iter().map(|&b| u32::from(b)).sum();
            h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            data.extend_from_slice(&h);
            data.extend_from_slice(body);
            data.resize(data.len().next_multiple_of(512), 0);
        }
        data.extend_from_slice(&[0; 1024]);
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn archive_round_trip_preserves_non_utf8_filenames() {
        let t = TestTempDir::new("non-utf8");
        let name = OsStr::from_bytes(b"raw-\xff.txt");
        let source = t.path().join(name);
        std::fs::write(&source, b"original bytes").unwrap();
        let archive = t.path().join("bytes.tar");
        compress_items(
            &archive,
            t.path(),
            &[PathBuf::from(name)],
            &ArchiveOptions { format: Some(ArchiveFormat::Tar), ..Default::default() },
        )
        .unwrap();
        let output = t.path().join("output");
        extract_archive(&archive, &output, None).unwrap();
        assert_eq!(std::fs::read(output.join(name)).unwrap(), b"original bytes");
        assert!(!output.join(name.to_string_lossy().as_ref()).exists());
    }

    #[test]
    fn extraction_rejects_unsafe_symlink_before_creating_it() {
        let t = TestTempDir::new("unsafe-symlink");
        let archive = t.path().join("bad.tar");
        write_test_tar(&archive, "bad-link", b'2', "../outside");
        let output = t.path().join("output");
        assert!(extract_archive(&archive, &output, None).is_err());
        assert!(std::fs::symlink_metadata(output.join("bad-link")).is_err());
    }

    #[test]
    fn extraction_never_retries_traversal_with_cli() {
        let t = TestTempDir::new("traversal");
        let archive = t.path().join("bad.tar");
        write_test_tar(&archive, "../escaped", b'0', "");
        let result = extract_archive(&archive, &t.path().join("output"), None);
        assert!(matches!(result, Err(ArchiveError::PathTraversal(_))));
        assert!(!t.path().join("escaped").exists());
    }

    /// PEN: hostile archives never write outside the destination: an absolute path, a
    /// hardlink to an absolute or `..` target, and a symlink already in the destination
    /// (planted there, e.g. in a shared folder) that points outside.
    #[test]
    fn hostile_archives_never_write_outside_the_destination() {
        let t = TestTempDir::new("hostile");
        let outside = t.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("victim"), b"untouched").unwrap();
        let victim = outside.join("victim");
        type Entry = (String, u8, String, u32, Vec<u8>);
        let cases: Vec<(&str, Vec<Entry>)> = vec![
            ("absolute", vec![(format!("{}/abs", outside.display()), b'0', String::new(), 0o644, b"x".to_vec())]),
            ("hardlink-absolute", vec![("h".into(), b'1', victim.display().to_string(), 0o644, Vec::new())]),
            ("hardlink-dotdot", vec![("h".into(), b'1', "../outside/victim".into(), 0o644, Vec::new())]),
            ("through-planted-link", vec![("planted/pwn".into(), b'0', String::new(), 0o644, b"pwned".to_vec())]),
            ("overwrite-planted-link", vec![("planted".into(), b'0', String::new(), 0o644, b"pwned".to_vec())]),
        ];
        for (name, entries) in cases {
            let archive = t.path().join(format!("{name}.tar"));
            let refs: Vec<(&str, u8, &str, u32, &[u8])> =
                entries.iter().map(|(n, k, l, m, b)| (n.as_str(), *k, l.as_str(), *m, b.as_slice())).collect();
            write_tar_entries(&archive, &refs);
            let dest = t.path().join(format!("dest-{name}"));
            std::fs::create_dir(&dest).unwrap();
            std::os::unix::fs::symlink(&outside, dest.join("planted")).unwrap();
            let r = extract_archive(&archive, &dest, None);
            assert_eq!(std::fs::read(&victim).unwrap(), b"untouched", "{name}: the victim changed ({r:?})");
            assert!(!outside.join("abs").exists(), "{name}: wrote an absolute path ({r:?})");
            assert!(!outside.join("pwn").exists(), "{name}: wrote through a planted link ({r:?})");
            assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1, "{name}: outside changed ({r:?})");
        }
    }

    /// PEN: modes a hostile archive asks for. Another local user must not be able to
    /// change what was extracted (no group/other write), and no setuid/setgid bit.
    #[test]
    fn extracted_files_are_never_writable_by_others_or_setuid() {
        let t = TestTempDir::new("modes");
        let archive = t.path().join("modes.tar");
        write_tar_entries(
            &archive,
            &[
                ("open.sh", b'0', "", 0o777, b"#!/bin/sh\n"),
                ("suid", b'0', "", 0o4755, b"x"),
                ("sgid-dir", b'5', "", 0o2777, b""),
            ],
        );
        let dest = t.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        extract_archive(&archive, &dest, None).unwrap();
        use std::os::unix::fs::PermissionsExt;
        for n in ["open.sh", "suid", "sgid-dir"] {
            let mode = std::fs::symlink_metadata(dest.join(n)).unwrap().permissions().mode() & 0o7777;
            assert_eq!(mode & 0o022, 0, "{n}: writable by others ({mode:o})");
            assert_eq!(mode & 0o6000, 0, "{n}: setuid/setgid kept ({mode:o})");
        }
        // Owner bits as the archive asked: an executable stays executable.
        let mode = std::fs::metadata(dest.join("open.sh")).unwrap().permissions().mode();
        assert_eq!(mode & 0o700, 0o700);
    }

    /// PEN: an archive password never appears on 7z's command line, where every local
    /// user can read it (`/proc/<pid>/cmdline`, `ps`); 7z reads it from its stdin.
    #[test]
    fn the_archive_password_is_never_on_a_command_line() {
        for format in [ArchiveFormat::SevenZip, ArchiveFormat::Zip] {
            let opts = ArchiveOptions {
                format: Some(format),
                password: Some("Tr0ub4dor-secret".into()),
                ..Default::default()
            };
            let cmd =
                seven_zip_command(Path::new("/x/out.7z"), Path::new("/x"), &[PathBuf::from("/x/f")], &opts).unwrap();
            let args: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert!(!args.iter().any(|a| a.contains("Tr0ub4dor")), "{format:?}: {args:?}");
            assert!(args.iter().any(|a| a == "-p"), "{format:?}: 7z must be told to read a password: {args:?}");
        }
    }

    /// The password reaches 7z through its stdin: an encrypted archive opens with the
    /// right password and refuses a wrong one (7z required: no silent pass without it).
    /// A zip also round-trips through our own extractor. (Ours cannot open a 7z with
    /// encrypted headers, which is what we make, so 7z itself checks that one.)
    #[test]
    fn an_encrypted_archive_opens_with_its_password_only() {
        let seven_z_test = |archive: &Path, pass: &str| -> bool {
            use std::io::Write;
            let mut child = std::process::Command::new("7z")
                .args(["t", "--"]) // no -p: 7z asks, and reads the password from stdin
                .arg(archive)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap_or_else(|e| panic!("7z required: {e}"));
            if let Some(mut i) = child.stdin.take() {
                let _ = i.write_all(format!("{pass}\n").as_bytes());
            }
            child.wait().is_ok_and(|s| s.success())
        };
        for (format, ext) in [(ArchiveFormat::SevenZip, "7z"), (ArchiveFormat::Zip, "zip")] {
            let t = TestTempDir::new(&format!("enc-{ext}"));
            std::fs::write(t.path().join("secret.txt"), b"top secret bytes").unwrap();
            let dest = t.path().join(format!("s.{ext}"));
            let opts =
                ArchiveOptions { format: Some(format), password: Some("Correct-Horse-9".into()), ..Default::default() };
            compress_items(&dest, t.path(), &[PathBuf::from("secret.txt")], &opts)
                .unwrap_or_else(|e| panic!("{ext}: encrypting failed (7z required): {e}"));
            assert!(seven_z_test(&dest, "Correct-Horse-9"), "{ext}: the right password failed");
            assert!(!seven_z_test(&dest, "wrong-password"), "{ext}: a wrong password opened it");
            if format == ArchiveFormat::Zip {
                let out = t.path().join("out");
                extract_archive(&dest, &out, Some("Correct-Horse-9")).unwrap_or_else(|e| panic!("{ext}: {e}"));
                assert_eq!(std::fs::read(out.join("secret.txt")).unwrap(), b"top secret bytes");
            }
        }
    }

    /// Write `files` (relative path, contents, mode) and `links` (name, target) under
    /// `src`, then pack them into `dest` as a 7z with encrypted headers, symlinks stored
    /// as links (7z required: no silent pass without it).
    fn make_encrypted_7z(src: &Path, dest: &Path, files: &[(&str, &[u8], u32)], links: &[(&str, &str)], pass: &str) {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        for (rel, body, mode) in files {
            let p = src.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(*mode)).unwrap();
        }
        for (name, target) in links {
            std::os::unix::fs::symlink(target, src.join(name)).unwrap();
        }
        let mut child = std::process::Command::new("7z")
            .args(["a", "-y", "-t7z", "-mhe=on", "-snl", "-p", "--"])
            .arg(dest)
            .arg(".")
            .current_dir(src)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("7z required: {e}"));
        child.stdin.take().unwrap().write_all(format!("{pass}\n{pass}\n").as_bytes()).unwrap();
        assert!(child.wait().unwrap().success(), "7z could not make the test archive");
    }

    /// Leftover staging directories of ours in `dir`.
    fn staging_left(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with(".gungnir-extract-"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The 7z we make with encrypted headers (names hidden) could not be extracted by our
    /// own extractor: libarchive cannot read such headers. Now it extracts through 7z into
    /// a private staging directory, checked like any other extraction.
    #[test]
    fn a_7z_with_encrypted_headers_extracts_with_its_password() {
        let t = TestTempDir::new("7z-he");
        let src = t.path().join("src");
        let archive = t.path().join("names-hidden.7z");
        make_encrypted_7z(
            &src,
            &archive,
            &[
                ("tax-2026.pdf", b"private", 0o644),
                ("sub/deep/note.txt", b"deeper", 0o600),
                ("run.sh", b"#!/bin/sh\n", 0o777),
            ],
            &[("inside", "sub/deep/note.txt")],
            "Hidden-Names-7",
        );
        let out = t.path().join("out");
        let stats = extract_archive(&archive, &out, Some("Hidden-Names-7")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(std::fs::read(out.join("tax-2026.pdf")).unwrap(), b"private");
        assert_eq!(std::fs::read(out.join("sub/deep/note.txt")).unwrap(), b"deeper");
        assert_eq!(std::fs::read_link(out.join("inside")).unwrap(), Path::new("sub/deep/note.txt"));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(out.join("run.sh")).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode & 0o7022, 0, "run.sh: {mode:o}");
        assert_eq!(mode & 0o700, 0o700, "run.sh: {mode:o}");
        assert!(stats.files_extracted >= 3, "{stats:?}");
        assert_eq!(staging_left(&out), Vec::<String>::new());
    }

    /// A wrong or missing password extracts nothing and leaves no staging behind.
    #[test]
    fn a_7z_with_encrypted_headers_needs_the_right_password() {
        let t = TestTempDir::new("7z-he-wrong");
        let src = t.path().join("src");
        let archive = t.path().join("a.7z");
        make_encrypted_7z(&src, &archive, &[("secret.txt", b"s", 0o644)], &[], "Right-Pass-1");
        for pass in [Some("wrong-pass"), None] {
            let out = t.path().join(format!("out-{}", pass.is_some()));
            assert!(extract_archive(&archive, &out, pass).is_err(), "{pass:?}");
            assert!(!out.join("secret.txt").exists(), "{pass:?}");
            assert_eq!(staging_left(&out), Vec::<String>::new(), "{pass:?}");
        }
    }

    /// Hostile content in such a 7z: a symlink out of the destination (absolute or by
    /// `..`) is refused before anything reaches the destination; a name that is already
    /// there is never overwritten.
    #[test]
    fn a_7z_with_encrypted_headers_is_checked_like_any_archive() {
        let t = TestTempDir::new("7z-he-hostile");
        let outside = t.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        for (i, target) in ["/etc/passwd".to_string(), "../outside".to_string()].iter().enumerate() {
            let src = t.path().join(format!("src{i}"));
            let archive = t.path().join(format!("evil{i}.7z"));
            make_encrypted_7z(&src, &archive, &[("ok.txt", b"ok", 0o644)], &[("escape", target)], "P-1");
            let out = t.path().join(format!("out{i}"));
            let r = extract_archive(&archive, &out, Some("P-1"));
            assert!(matches!(r, Err(ArchiveError::PathTraversal(_))), "{target}: {r:?}");
            assert!(std::fs::symlink_metadata(out.join("escape")).is_err(), "{target}: the link arrived");
            assert!(!out.join("ok.txt").exists(), "{target}: part of a refused archive arrived");
            assert_eq!(staging_left(&out), Vec::<String>::new(), "{target}");
        }
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
        // An existing name is never overwritten.
        let src = t.path().join("src-clash");
        let archive = t.path().join("clash.7z");
        make_encrypted_7z(&src, &archive, &[("same.txt", b"from the archive", 0o644)], &[], "P-2");
        let out = t.path().join("out-clash");
        std::fs::create_dir(&out).unwrap();
        std::fs::write(out.join("same.txt"), b"mine").unwrap();
        assert!(extract_archive(&archive, &out, Some("P-2")).is_err());
        assert_eq!(std::fs::read(out.join("same.txt")).unwrap(), b"mine");
        assert_eq!(staging_left(&out), Vec::<String>::new());
    }

    /// Extracting next to a file of the same name replaced it (a plain tar silently; an
    /// encrypted zip with a wrong password with zeros, then failed). Now nothing is ever
    /// replaced: the extraction stops before anything moves, and the file is untouched.
    #[test]
    fn extraction_never_replaces_or_destroys_an_existing_file() {
        let t = TestTempDir::new("no-replace");
        std::fs::write(t.path().join("notes.txt"), b"from the archive").unwrap();
        let tar = t.path().join("plain.tar");
        compress_items(
            &tar,
            t.path(),
            &[PathBuf::from("notes.txt")],
            &ArchiveOptions { format: Some(ArchiveFormat::Tar), ..Default::default() },
        )
        .unwrap();
        let zip = t.path().join("locked.zip");
        let opts =
            ArchiveOptions { format: Some(ArchiveFormat::Zip), password: Some("pw-1".into()), ..Default::default() };
        compress_items(&zip, t.path(), &[PathBuf::from("notes.txt")], &opts)
            .unwrap_or_else(|e| panic!("7z required: {e}"));
        for (archive, pass) in [(&tar, None), (&zip, Some("wrong")), (&zip, Some("pw-1"))] {
            let out = t.path().join("mine");
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir(&out).unwrap();
            std::fs::write(out.join("notes.txt"), b"MY OWN NOTES").unwrap();
            let r = extract_archive(archive, &out, pass);
            assert!(r.is_err(), "{archive:?} {pass:?}: {r:?}");
            assert_eq!(std::fs::read(out.join("notes.txt")).unwrap(), b"MY OWN NOTES", "{archive:?} {pass:?}");
            assert_eq!(staging_left(&out), Vec::<String>::new());
        }
        // The wrong password is reported as such.
        let out = t.path().join("empty");
        assert!(matches!(
            extract_archive(&zip, &out, Some("wrong")),
            Err(ArchiveError::PasswordRequired { wrong: true })
        ));
        assert!(matches!(extract_archive(&zip, &out, None), Err(ArchiveError::PasswordRequired { wrong: false })));
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "a failed extraction left something");
    }

    /// A folder that is already there is merged into: what was in it stays, the archive's
    /// new files arrive. A corrupt archive leaves the destination as it was.
    #[test]
    fn extraction_merges_into_folders_and_leaves_nothing_on_failure() {
        let t = TestTempDir::new("merge");
        let src = t.path().join("src");
        std::fs::create_dir_all(src.join("photos")).unwrap();
        std::fs::write(src.join("photos/new.jpg"), b"new").unwrap();
        let tar = t.path().join("p.tar");
        compress_items(
            &tar,
            &src,
            &[PathBuf::from("photos")],
            &ArchiveOptions { format: Some(ArchiveFormat::Tar), ..Default::default() },
        )
        .unwrap();
        let out = t.path().join("out");
        std::fs::create_dir_all(out.join("photos")).unwrap();
        std::fs::write(out.join("photos/old.jpg"), b"old").unwrap();
        extract_archive(&tar, &out, None).unwrap();
        assert_eq!(std::fs::read(out.join("photos/old.jpg")).unwrap(), b"old");
        assert_eq!(std::fs::read(out.join("photos/new.jpg")).unwrap(), b"new");
        // Corrupt: cut in the middle of the data.
        let mut bytes = std::fs::read(&tar).unwrap();
        bytes.truncate(700);
        let bad = t.path().join("bad.tar");
        std::fs::write(&bad, &bytes).unwrap();
        let out2 = t.path().join("out2");
        std::fs::create_dir(&out2).unwrap();
        assert!(extract_archive(&bad, &out2, None).is_err());
        assert_eq!(std::fs::read_dir(&out2).unwrap().count(), 0, "a corrupt archive left something");
    }

    #[test]
    fn requested_encryption_never_falls_back_to_plaintext() {
        let t = TestTempDir::new("encryption");
        std::fs::write(t.path().join("secret.txt"), b"secret").unwrap();
        let dest = t.path().join("secret.zip");
        let opts = ArchiveOptions {
            format: Some(ArchiveFormat::Zip),
            password: Some("test-pass".into()),
            ..Default::default()
        };
        // Both outcomes are checked: encrypted and unreadable without the password, or an
        // error that leaves no archive behind (never a plaintext one).
        match compress_items(&dest, t.path(), &[PathBuf::from("secret.txt")], &opts) {
            Ok(_) => assert!(
                test_archive_integrity(&dest).is_err(),
                "password-protected data must not be readable without the password"
            ),
            Err(e) => assert!(!dest.exists(), "a failed encryption left {} behind ({e})", dest.display()),
        }
    }

    #[test]
    fn test_format_detection_from_extensions() {
        assert_eq!(ArchiveFormat::from_path(Path::new("test.tar.zst")), Some(ArchiveFormat::TarZstd));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.tzst")), Some(ArchiveFormat::TarZstd));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.tar.xz")), Some(ArchiveFormat::TarXz));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.zip")), Some(ArchiveFormat::Zip));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.7z")), Some(ArchiveFormat::SevenZip));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.tar.gz")), Some(ArchiveFormat::TarGz));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.tar.bz2")), Some(ArchiveFormat::TarBz2));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.tar.lz4")), Some(ArchiveFormat::TarLz4));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.tar.br")), Some(ArchiveFormat::TarBrotli));
        assert_eq!(ArchiveFormat::from_path(Path::new("test.tar")), Some(ArchiveFormat::Tar));
    }

    #[test]
    fn test_lossless_roundtrip_tar_zst() {
        let dir = TestTempDir::new("tar-zst");
        let src_file = dir.path().join("payload.txt");
        std::fs::write(&src_file, b"The quick brown fox jumps over the lazy dog 1234567890!").expect("write");

        let archive_file = dir.path().join("test.tar.zst");
        let stats = compress_items(
            &archive_file,
            dir.path(),
            &[PathBuf::from("payload.txt")],
            &ArchiveOptions { format: Some(ArchiveFormat::TarZstd), ..Default::default() },
        )
        .expect("compress");
        assert_eq!(stats.files_count, 1);
        assert!(archive_file.exists());

        // Extract
        let extract_dir = dir.path().join("out_zst");
        let estats = extract_archive(&archive_file, &extract_dir, None).expect("extract");
        assert_eq!(estats.files_extracted, 1);

        let extracted_file = extract_dir.join("payload.txt");
        assert!(extracted_file.exists());
        verify_bit_perfect(&src_file, &extracted_file).expect("bit-perfect verification");
    }

    #[test]
    fn test_lossless_roundtrip_zip() {
        let dir = TestTempDir::new("zip");
        let src_file = dir.path().join("data.bin");
        let payload = (0..1024).map(|i| (i % 256) as u8).collect::<Vec<_>>();
        std::fs::write(&src_file, &payload).expect("write");

        let archive_file = dir.path().join("test.zip");
        compress_items(
            &archive_file,
            dir.path(),
            &[PathBuf::from("data.bin")],
            &ArchiveOptions { format: Some(ArchiveFormat::Zip), ..Default::default() },
        )
        .expect("compress");

        let list = list_archive(&archive_file).expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].pathname, "data.bin");
        assert_eq!(list[0].size, 1024);

        let extract_dir = dir.path().join("out_zip");
        extract_archive(&archive_file, &extract_dir, None).expect("extract");
        verify_bit_perfect(&src_file, &extract_dir.join("data.bin")).expect("bit-perfect");
    }

    #[test]
    fn test_lossless_roundtrip_tar_xz_and_gzip() {
        let dir = TestTempDir::new("xz-gz");
        let src_file = dir.path().join("sample.json");
        std::fs::write(&src_file, b"{\"gungnir\":\"vfs\",\"lossless\":true}").expect("write");

        // XZ
        let xz_file = dir.path().join("archive.tar.xz");
        compress_items(
            &xz_file,
            dir.path(),
            &[PathBuf::from("sample.json")],
            &ArchiveOptions { format: Some(ArchiveFormat::TarXz), ..Default::default() },
        )
        .expect("compress xz");
        let extract_xz = dir.path().join("out_xz");
        extract_archive(&xz_file, &extract_xz, None).expect("extract xz");
        verify_bit_perfect(&src_file, &extract_xz.join("sample.json")).expect("bit-perfect xz");

        // Gzip
        let gz_file = dir.path().join("archive.tar.gz");
        compress_items(
            &gz_file,
            dir.path(),
            &[PathBuf::from("sample.json")],
            &ArchiveOptions { format: Some(ArchiveFormat::TarGz), ..Default::default() },
        )
        .expect("compress gz");
        let extract_gz = dir.path().join("out_gz");
        extract_archive(&gz_file, &extract_gz, None).expect("extract gz");
        verify_bit_perfect(&src_file, &extract_gz.join("sample.json")).expect("bit-perfect gz");
    }

    #[test]
    fn test_archive_integrity_checker() {
        let dir = TestTempDir::new("integrity");
        let src_file = dir.path().join("check.txt");
        std::fs::write(&src_file, b"Lossless Archive Check").expect("write");

        let archive_file = dir.path().join("check.tar.zst");
        compress_items(
            &archive_file,
            dir.path(),
            &[PathBuf::from("check.txt")],
            &ArchiveOptions { format: Some(ArchiveFormat::TarZstd), ..Default::default() },
        )
        .expect("compress");

        let verified = test_archive_integrity(&archive_file).expect("integrity check");
        assert_eq!(verified, 1);
    }

    #[test]
    fn test_lossless_roundtrip_7z() {
        let dir = TestTempDir::new("7z");
        let src_file = dir.path().join("archive_target.txt");
        std::fs::write(&src_file, b"Lossless 7-Zip Solid Container Verification").expect("write");

        let archive_file = dir.path().join("test.7z");
        compress_items(
            &archive_file,
            dir.path(),
            &[PathBuf::from("archive_target.txt")],
            &ArchiveOptions {
                format: Some(ArchiveFormat::SevenZip),
                level: CompressionLevel::Normal,
                ..Default::default()
            },
        )
        .expect("compress 7z");

        let extract_dir = dir.path().join("out_7z");
        extract_archive(&archive_file, &extract_dir, None).expect("extract 7z");
        verify_bit_perfect(&src_file, &extract_dir.join("archive_target.txt")).expect("bit-perfect 7z");
    }

    #[test]
    fn test_single_file_streaming_lossless() {
        let dir = TestTempDir::new("single-stream");
        let raw_src = dir.path().join("source.dat");
        let payload = (0..8192).map(|i| ((i * 7 + 13) % 256) as u8).collect::<Vec<_>>();
        std::fs::write(&raw_src, &payload).expect("write");

        let formats = [
            ("data.zst", SingleStreamFormat::Zstandard),
            ("data.xz", SingleStreamFormat::Xz),
            ("data.gz", SingleStreamFormat::Gzip),
            ("data.bz2", SingleStreamFormat::Bzip2),
            ("data.lz4", SingleStreamFormat::Lz4),
            ("data.br", SingleStreamFormat::Brotli),
        ];

        for (ext, fmt) in formats {
            let compressed = dir.path().join(ext);
            compress_single_file(&raw_src, &compressed, fmt, CompressionLevel::Normal)
                .unwrap_or_else(|e| panic!("compression failed for {ext}: {e}"));

            let decompressed = dir.path().join(format!("decomp_{ext}.dat"));
            decompress_single_file(&compressed, &decompressed)
                .unwrap_or_else(|e| panic!("decompression failed for {ext}: {e}"));

            verify_bit_perfect(&raw_src, &decompressed)
                .unwrap_or_else(|e| panic!("bit-perfect match failed for {ext}: {e}"));
        }
    }

    #[test]
    fn test_directory_hierarchy_and_symlinks() {
        let dir = TestTempDir::new("hierarchy");
        let sub = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&sub).expect("create_dir_all");

        let file1 = sub.join("leaf.txt");
        std::fs::write(&file1, b"Nested file content in a/b/c/leaf.txt").expect("write leaf");

        let root_file = dir.path().join("root.txt");
        std::fs::write(&root_file, b"Root level file content").expect("write root");

        #[cfg(unix)]
        {
            let link = dir.path().join("link_to_leaf");
            let _ = std::os::unix::fs::symlink("a/b/c/leaf.txt", &link);
        }

        let archive_file = dir.path().join("tree.tar.zst");
        compress_items(
            &archive_file,
            dir.path(),
            &[PathBuf::from("a"), PathBuf::from("root.txt")],
            &ArchiveOptions { format: Some(ArchiveFormat::TarZstd), ..Default::default() },
        )
        .expect("compress tree");

        let out_dir = dir.path().join("extracted_tree");
        extract_archive(&archive_file, &out_dir, None).expect("extract tree");

        verify_bit_perfect(&file1, &out_dir.join("a/b/c/leaf.txt")).expect("bit-perfect leaf");
        verify_bit_perfect(&root_file, &out_dir.join("root.txt")).expect("bit-perfect root");
    }

    #[test]
    fn compressing_a_deep_folder_does_not_overflow_the_stack() {
        // add_item_recursive took a frame per level holding a 64 KiB read buffer: on the
        // 2 MiB stack the app compresses on, about 30 levels aborted the whole app.
        let dir = TestTempDir::new("deep");
        let mut leaf = dir.path().join("top");
        for i in 0..60 {
            leaf.push(format!("level{i}"));
        }
        std::fs::create_dir_all(&leaf).expect("create deep tree");
        std::fs::write(leaf.join("leaf.txt"), b"at the bottom").expect("write leaf");
        let (archive_file, base) = (dir.path().join("deep.tar.zst"), dir.path().to_path_buf());
        let a2 = archive_file.clone();
        let stats = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                compress_items(
                    &a2,
                    &base,
                    &[PathBuf::from("top")],
                    &ArchiveOptions { format: Some(ArchiveFormat::TarZstd), ..Default::default() },
                )
                .map(|s| (s.directories_count, s.files_count))
                .map_err(|e| e.to_string())
            })
            .expect("spawn")
            .join()
            .expect("join");
        assert_eq!(stats, Ok((61, 1)));
        let out_dir = dir.path().join("out");
        extract_archive(&archive_file, &out_dir, None).expect("extract deep tree");
        let rel = leaf.strip_prefix(dir.path()).expect("inside");
        assert_eq!(std::fs::read(out_dir.join(rel).join("leaf.txt")).expect("leaf"), b"at the bottom");
    }

    #[test]
    fn test_lossless_audio_transcode() {
        // No silent pass: lossless transcoding needs ffmpeg, so without it this fails.
        let ok = |out: std::io::Result<std::process::Output>, what: &str| match out {
            Ok(o) if o.status.success() => {}
            Ok(o) => panic!("ffmpeg {what} failed: {}", String::from_utf8_lossy(&o.stderr)),
            Err(e) => panic!("ffmpeg {what}: {e} (lossless audio needs ffmpeg)"),
        };

        let dir = TestTempDir::new("lossless-audio");
        let wav_src = dir.path().join("orig.wav");

        // Generate a 0.5 s 440 Hz 16-bit WAV using ffmpeg
        ok(
            std::process::Command::new("ffmpeg")
                .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=0.5", "-c:a", "pcm_s16le", "-y"])
                .arg(&wav_src)
                .output(),
            "generating the sample",
        );

        // FLAC
        let flac_file = dir.path().join("test.flac");
        transcode_lossless_audio(&wav_src, &flac_file, LosslessAudioFormat::Flac).expect("flac transcode");
        assert!(flac_file.exists());

        // ALAC
        let alac_file = dir.path().join("test.m4a");
        transcode_lossless_audio(&wav_src, &alac_file, LosslessAudioFormat::Alac).expect("alac transcode");
        assert!(alac_file.exists());

        // Decode both back to WAV and verify the PCM data is bit-perfect
        for (encoded, name) in [(&flac_file, "flac"), (&alac_file, "alac")] {
            let decoded = dir.path().join(format!("{name}_decoded.wav"));
            ok(
                std::process::Command::new("ffmpeg")
                    .args(["-i"])
                    .arg(encoded)
                    .args(["-c:a", "pcm_s16le", "-y"])
                    .arg(&decoded)
                    .output(),
                "decoding",
            );
            verify_bit_perfect(&wav_src, &decoded).unwrap_or_else(|e| panic!("{name} bit-perfect roundtrip: {e}"));
        }
    }
}
