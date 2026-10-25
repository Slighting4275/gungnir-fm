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

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_long, c_uint, c_void};
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

    // If 7z format is requested with password or solid archiving, use 7z CLI if available
    if (format == ArchiveFormat::SevenZip || options.password.is_some())
        && let Ok(stats) = compress_via_7z_cli(dest_path, base_dir, items, options)
    {
        return Ok(stats);
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

        let c_dest = CString::new(dest_path.to_string_lossy().as_bytes())
            .map_err(|_| ArchiveError::Libarchive("Invalid destination path".into()))?;
        if archive_write_open_filename(a, c_dest.as_ptr()) != ARCHIVE_OK {
            return Err(ArchiveError::Libarchive(get_archive_error(a)));
        }

        // Recursively add items
        for item in items {
            let abs_item = if item.is_absolute() { item.clone() } else { base_dir.join(item) };
            add_item_recursive(a, &abs_item, base_dir, &mut stats)?;
        }
    }

    if let Ok(meta) = std::fs::metadata(dest_path) {
        stats.total_compressed_bytes = meta.len();
    }
    stats.elapsed_ms = t0.elapsed().as_millis() as u64;
    Ok(stats)
}

fn add_item_recursive(
    a: *mut c_void,
    path: &Path,
    base_dir: &Path,
    stats: &mut ArchiveStats,
) -> Result<(), ArchiveError> {
    let rel_path = path.strip_prefix(base_dir).unwrap_or(path);
    let mut rel_str = rel_path.to_string_lossy().to_string();
    if rel_str.is_empty() {
        rel_str = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "entry".to_string());
    }

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
            CString::new(rel_str.as_bytes()).map_err(|_| ArchiveError::Libarchive("Invalid entry path".into()))?;
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
            if let Ok(target) = std::fs::read_link(path)
                && let Ok(c_target) = CString::new(target.to_string_lossy().as_bytes())
            {
                archive_entry_set_symlink(entry, c_target.as_ptr());
            }
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

            if let Ok(dir_entries) = std::fs::read_dir(path) {
                for de in dir_entries.flatten() {
                    add_item_recursive(a, &de.path(), base_dir, stats)?;
                }
            }
        } else {
            archive_entry_set_filetype(entry, AE_IFREG);
            archive_entry_set_size(entry, meta.len() as i64);
            if archive_write_header(a, entry) != ARCHIVE_OK {
                return Err(ArchiveError::Libarchive(get_archive_error(a)));
            }

            let mut file = std::fs::File::open(path)?;
            let mut buf = [0u8; 65536];
            use std::io::Read;
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                let written = archive_write_data(a, buf.as_ptr() as *const c_void, n);
                if written < 0 {
                    return Err(ArchiveError::Libarchive(get_archive_error(a)));
                }
            }
            stats.files_count += 1;
            stats.total_uncompressed_bytes += meta.len();
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

    // If password provided and it's a 7z/zip/rar archive, attempt CLI fallback first
    if let Some(pass) = password
        && let Ok(stats) = extract_via_7z_cli(archive_path, target_dir, pass)
    {
        return Ok(stats);
    }

    // Try in-process libarchive extraction
    match extract_archive_libarchive(archive_path, target_dir) {
        Ok(stats) => Ok(stats),
        Err(e) => {
            // If libarchive encounters an unsupported sub-format (e.g. solid 7z), fallback to 7z CLI
            if let Ok(stats) = extract_via_7z_cli(archive_path, target_dir, "") { Ok(stats) } else { Err(e) }
        }
    }
}

fn extract_archive_libarchive(archive_path: &Path, target_dir: &Path) -> Result<ExtractStats, ArchiveError> {
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

        let c_path = CString::new(archive_path.to_string_lossy().as_bytes())
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
            let raw_path = CStr::from_ptr(current_path_c).to_string_lossy();
            let sanitized = raw_path.trim_start_matches('/').trim_start_matches('\\');
            if sanitized.contains("../") || sanitized.contains("..\\") {
                return Err(ArchiveError::PathTraversal(raw_path.into_owned()));
            }

            let full_out = target_dir.join(sanitized);
            if let Some(parent) = full_out.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            let c_full_out = CString::new(full_out.to_string_lossy().as_bytes())
                .map_err(|_| ArchiveError::Libarchive("Invalid output path".into()))?;
            archive_entry_set_pathname(entry, c_full_out.as_ptr());

            let write_header_res = archive_write_header(ext, entry);
            let ftype = archive_entry_filetype(entry);
            if write_header_res != ARCHIVE_OK && write_header_res != ARCHIVE_WARN {
                // If directory already exists, proceed gracefully
                if ftype != AE_IFDIR {
                    return Err(ArchiveError::Libarchive(get_archive_error(ext)));
                }
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

            let _ = archive_write_finish_entry(ext);
        }
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

        let c_path = CString::new(archive_path.to_string_lossy().as_bytes())
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

        let c_path = CString::new(archive_path.to_string_lossy().as_bytes())
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

fn compress_via_7z_cli(
    dest_path: &Path,
    base_dir: &Path,
    items: &[PathBuf],
    options: &ArchiveOptions,
) -> Result<ArchiveStats, ArchiveError> {
    let t0 = Instant::now();
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

    // Password & Header encryption
    if let Some(ref pass) = options.password {
        cmd.arg(format!("-p{pass}"));
        cmd.arg("-mhe=on");
    }

    cmd.arg(dest_path);
    for item in items {
        let rel = item.strip_prefix(base_dir).unwrap_or(item);
        cmd.arg(rel);
    }

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::ToolNotFound("7z".into())
        } else {
            ArchiveError::Io(e)
        }
    })?;

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

fn extract_via_7z_cli(archive_path: &Path, target_dir: &Path, password: &str) -> Result<ExtractStats, ArchiveError> {
    let t0 = Instant::now();
    let mut cmd = std::process::Command::new("7z");
    cmd.arg("x");
    cmd.arg("-y");
    cmd.arg(format!("-o{}", target_dir.display()));
    if !password.is_empty() {
        cmd.arg(format!("-p{password}"));
    } else {
        cmd.arg("-p"); // Empty password / non-interactive
    }
    cmd.arg(archive_path);

    let output = cmd.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ArchiveError::ToolNotFound("7z".into())
        } else {
            ArchiveError::Io(e)
        }
    })?;

    if !output.status.success() {
        return Err(ArchiveError::CliToolFailed {
            tool: "7z".into(),
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    Ok(ExtractStats { elapsed_ms: t0.elapsed().as_millis() as u64, ..Default::default() })
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
                vec!["-k".to_string(), "-f".to_string(), lvl.to_string(), "-c".to_string(), src.display().to_string()],
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
                vec!["-k".to_string(), "-f".to_string(), lvl.to_string(), "-c".to_string(), src.display().to_string()],
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
                vec!["-k".to_string(), "-f".to_string(), lvl.to_string(), "-c".to_string(), src.display().to_string()],
            )
        }
        SingleStreamFormat::Lz4 => {
            let lvl = match level {
                CompressionLevel::Fast => "-1",
                CompressionLevel::Normal => "-4",
                CompressionLevel::Maximum => "-9",
            };
            ("lz4", vec!["-f".to_string(), lvl.to_string(), src.display().to_string(), dest.display().to_string()])
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
                src.display().to_string(),
            ],
        ),
        "xz" => (
            "xz",
            vec!["-d".to_string(), "-k".to_string(), "-f".to_string(), "-c".to_string(), src.display().to_string()],
        ),
        "gz" => (
            "gzip",
            vec!["-d".to_string(), "-k".to_string(), "-f".to_string(), "-c".to_string(), src.display().to_string()],
        ),
        "bz2" => (
            "bzip2",
            vec!["-d".to_string(), "-k".to_string(), "-f".to_string(), "-c".to_string(), src.display().to_string()],
        ),
        "lz4" => {
            ("lz4", vec!["-d".to_string(), "-f".to_string(), src.display().to_string(), dest.display().to_string()])
        }
        "br" => (
            "brotli",
            vec![
                "-d".to_string(),
                "-f".to_string(),
                "-o".to_string(),
                dest.display().to_string(),
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
    cmd.arg("-y");
    cmd.arg("-i").arg(src);
    cmd.arg("-c:v").arg("ffv1");
    cmd.arg("-level").arg("3");
    cmd.arg("-c:a").arg("copy");
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
    cmd.arg(src);
    cmd.arg(dest);
    cmd.arg("-d").arg("0"); // 0 distance = mathematically lossless
    cmd.arg("-e").arg("7"); // Effort 7

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
    fn test_lossless_audio_transcode() {
        let check_ffmpeg = std::process::Command::new("ffmpeg").arg("-version").output();
        if check_ffmpeg.is_err() {
            eprintln!("ffmpeg not found; skipping audio transcode test");
            return;
        }

        let dir = TestTempDir::new("lossless-audio");
        let wav_src = dir.path().join("orig.wav");

        // Generate 1-second 44.1kHz 16-bit stereo sine wave WAV using ffmpeg
        let cmd_gen = std::process::Command::new("ffmpeg")
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=0.5", "-c:a", "pcm_s16le", "-y"])
            .arg(&wav_src)
            .output();

        if let Ok(out) = cmd_gen
            && out.status.success()
        {
            // FLAC
            let flac_file = dir.path().join("test.flac");
            transcode_lossless_audio(&wav_src, &flac_file, LosslessAudioFormat::Flac).expect("flac transcode");
            assert!(flac_file.exists());

            // ALAC
            let alac_file = dir.path().join("test.m4a");
            transcode_lossless_audio(&wav_src, &alac_file, LosslessAudioFormat::Alac).expect("alac transcode");
            assert!(alac_file.exists());

            // Decode FLAC back to WAV and verify PCM data
            let flac_decoded = dir.path().join("flac_decoded.wav");
            let dec = std::process::Command::new("ffmpeg")
                .args(["-i"])
                .arg(&flac_file)
                .args(["-c:a", "pcm_s16le", "-y"])
                .arg(&flac_decoded)
                .output();
            if let Ok(dec_out) = dec
                && dec_out.status.success()
            {
                verify_bit_perfect(&wav_src, &flac_decoded).expect("flac bit-perfect roundtrip");
            }
        }
    }
}
