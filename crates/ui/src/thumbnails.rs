//! Thumbnail generation and caching for image and video files.
//!
//! Generates crisp 256x256 thumbnails for images and video frames and caches them
//! under `~/.cache/gungnir-fm/thumbnails/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use core_vfs::format::Category;

static PINNED_CACHE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Fixes the thumbnail cache for the rest of the process (the first call wins; returns
/// the directory in force). Tests pin a sandbox so they never write to `~/.cache`.
#[cfg(test)]
pub fn pin_cache_dir(dir: PathBuf) -> &'static Path {
    PINNED_CACHE_DIR.get_or_init(|| dir)
}

/// Directory where thumbnails are cached on disk.
#[must_use]
pub fn thumbnail_cache_dir() -> PathBuf {
    if let Some(dir) = PINNED_CACHE_DIR.get() {
        return dir.clone();
    }
    if let Ok(dir) = std::env::var("GUNGNIR_THUMBNAIL_CACHE_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Some(mut dir) = dirs::cache_dir() {
        dir.push("gungnir-fm");
        dir.push("thumbnails");
        dir
    } else {
        let uid = unsafe { libc::getuid() };
        let run_dir = PathBuf::from(format!("/run/user/{uid}/gungnir-fm/thumbnails"));
        if run_dir.parent().map(|p| p.exists()).unwrap_or(false) {
            run_dir
        } else {
            std::env::temp_dir().join(format!("gungnir-fm-thumbnails-{uid}"))
        }
    }
}

/// The cache holds pictures of private files, so it must be ours alone: a directory
/// owned by us (made 0700 if it is looser), reached directly or through a link that is
/// itself ours. The fallback is a fixed name in /tmp, which another user could create
/// first, as a directory or a link to one of theirs; such a cache is never used.
fn cache_dir_is_private(dir: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    let Ok(link) = std::fs::symlink_metadata(dir) else { return false };
    if link.uid() != me {
        return false;
    }
    let Ok(md) = std::fs::metadata(dir) else { return false };
    if !md.is_dir() || md.uid() != me {
        return false;
    }
    md.mode() & 0o077 == 0 || std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_ok()
}

/// ffmpeg reads local files only (-protocol_whitelist file): no playlist or concat input
/// may make a thumbnail reach the network.
///
/// How long one external thumbnail tool (magick, ffmpeg, dcraw, exiftool, rsvg-convert)
/// may run: a hostile file can make a decoder hang, and it held its worker for good.
const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// ImageMagick's own bounds, before the input: an image it cannot fit is refused, never
/// cached on disk (its default cache, /tmp, is memory here) or worked on for minutes.
/// (Its disk cache is /tmp, which may be memory: the three together are the ceiling.)
const MAGICK_LIMITS: [&str; 18] = [
    "-limit", "memory", "256MiB", "-limit", "map", "256MiB", "-limit", "disk", "256MiB", "-limit", "time", "30",
    "-limit", "width", "20KP", "-limit", "height", "20KP",
];

/// Images larger than this get no thumbnail (no tool is started for them): a PNG of a
/// few hundred kilobytes can say 12000x12000, and ImageMagick took over a gigabyte of
/// memory for it.
const MAX_THUMBNAIL_PIXELS: u64 = 100_000_000;
const MAX_THUMBNAIL_SIDE: u32 = 20_000;

/// Decode an image from memory within the thumbnail bounds (64 MiB, the size limits).
fn decode_limited(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader =
        image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    limits.max_image_width = Some(MAX_THUMBNAIL_SIDE);
    limits.max_image_height = Some(MAX_THUMBNAIL_SIDE);
    reader.limits(limits);
    reader.decode().map_err(|e| e.to_string())
}

/// A [`Command`] run with a deadline: killed and reaped once it passes.
trait TimedCommand {
    fn timed_status_within(&mut self, limit: std::time::Duration) -> std::io::Result<std::process::ExitStatus>;
    fn timed_status(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.timed_status_within(TOOL_TIMEOUT)
    }
}

impl TimedCommand for Command {
    fn timed_status_within(&mut self, limit: std::time::Duration) -> std::io::Result<std::process::ExitStatus> {
        let mut child = self.spawn()?;
        let deadline = std::time::Instant::now() + limit;
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "thumbnail tool timed out"));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

/// [`TimedCommand`] for a tool whose output is the result (exiftool's embedded preview):
/// at most `max` bytes are kept, and a longer output is a failure.
fn timed_output(cmd: &mut Command, max: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut out).take(max as u64 + 1).read_to_end(&mut b);
        b
    });
    let deadline = std::time::Instant::now() + TOOL_TIMEOUT;
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let bytes = reader.join().ok()?;
    (status?.success() && bytes.len() <= max).then_some(bytes)
}

/// Computes a deterministic cache filename based on URI and modified timestamp.
#[must_use]
pub fn cache_filename(uri: &str, mtime_secs: i64) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(uri.as_bytes());
    hasher.update(mtime_secs.to_le_bytes());
    let hash = hasher.finalize();
    let slice: [u8; 8] = hash[0..8].try_into().unwrap_or([0; 8]);
    format!("{:016x}.png", u64::from_le_bytes(slice))
}

/// Checks if a file is an image or video based on category or extension.
#[must_use]
pub fn is_previewable(category: Category, ext: Option<&str>) -> bool {
    if matches!(category, Category::Image | Category::Video) {
        return true;
    }
    let Some(ext) = ext else { return false };
    let e = ext.to_ascii_lowercase();
    is_video_ext(Some(e.as_str()))
        || is_raw_sensor_ext(Some(e.as_str()))
        || matches!(
            e.as_str(),
            // Universal & Web Raster
            "png"
                | "jpg"
                | "jpeg"
                | "jpe"
                | "jif"
                | "jfif"
                | "jfi"
                | "gif"
                | "bmp"
                | "dib"
                | "tiff"
                | "tif"
                | "bigtiff"
                | "webp"
                | "ico"
                | "cur"
                | "icns"
                // High-Efficiency & Modern Still Encodings
                | "avif"
                | "avis"
                | "heic"
                | "heif"
                | "heifs"
                | "jxl"
                | "jp2"
                | "j2k"
                | "jpx"
                | "jpm"
                | "j2c"
                | "jxr"
                | "hdp"
                | "wdp"
                | "jxs"
                | "flif"
                | "fuif"
                | "bpg"
                | "qoi"
                | "ff"
                | "farbfeld"
                // Vector & Procedural Graphics
                | "svg"
                | "svgz"
                | "eps"
                | "ps"
                | "ai"
                | "cdr"
                | "wmf"
                | "emf"
                | "cgm"
                | "dxf"
                | "dwg"
                | "gbr"
                | "gerber"
                | "xar"
                | "sk1"
                | "fla"
                | "swf"
                | "vml"
                | "pgml"
                // Professional HDR, VFX, 3D Textures & Compositing
                | "exr"
                | "hdr"
                | "pic"
                | "rgbe"
                | "dpx"
                | "cin"
                | "dds"
                | "ktx"
                | "ktx2"
                | "astc"
                | "pvr"
                | "pvrtc"
                | "pkm"
                | "vtf"
                | "tga"
                | "targa"
                | "psd"
                | "psb"
                | "xcf"
                | "cpt"
                | "afphoto"
                | "pdn"
                | "kra"
                | "ora"
                | "iff"
                | "ilbm"
                | "lbm"
                | "sgi"
                | "rgb"
                | "rgba"
                | "bw"
                // Scientific, Medical, Geospatial
                | "fits"
                | "fit"
                | "fts"
                | "dcm"
                | "dicom"
                | "geotiff"
                | "nitf"
                | "ntf"
                | "sid"
                | "mrsid"
                | "ecw"
                | "h5"
                | "hdf5"
                | "hdf"
                | "nc"
                | "netcdf"
                | "svs"
                | "ndpi"
                | "ometiff"
                | "dm3"
                | "dm4"
                | "mrc"
                | "nii"
                | "vtk"
                // Legacy, Retro & Specialized Bitmaps
                | "pcx"
                | "pbm"
                | "pgm"
                | "ppm"
                | "pam"
                | "pnm"
                | "xbm"
                | "xpm"
                | "xwd"
                | "ras"
                | "sun"
                | "mac"
                | "pnt"
                | "gem"
                | "wbmp"
                | "pict"
                | "pct"
                | "cut"
                | "art"
                | "iwi"
                | "blp"
                | "tim"
                | "tim2"
                | "gxt"
                // Animated & Multi-Picture Containers
                | "apng"
                | "mng"
                | "jng"
                | "mpo"
                | "fli"
                | "flc"
                // 3D / Spatial / Splatting
                | "splat"
                | "spz"
                | "ply"
        )
}

/// Checks whether an extension corresponds to a RAW camera sensor file.
#[must_use]
pub fn is_raw_sensor_ext(ext: Option<&str>) -> bool {
    let Some(ext) = ext else { return false };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "dng"
            | "crw"
            | "cr2"
            | "cr3"
            | "nef"
            | "nrw"
            | "arw"
            | "srf"
            | "sr2"
            | "raf"
            | "orf"
            | "om"
            | "rw2"
            | "pef"
            | "kdc"
            | "dcr"
            | "dcs"
            | "mos"
            | "mef"
            | "3fr"
            | "fff"
            | "iiq"
            | "x3f"
            | "erf"
            | "rwl"
            | "srw"
            | "gpr"
            | "mrw"
            | "bay"
            | "cap"
    )
}

/// Checks whether an extension corresponds to a video file.
#[must_use]
pub fn is_video_ext(ext: Option<&str>) -> bool {
    let Some(ext) = ext else { return false };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        // Universal, Web & Consumer Containers
        "mp4"
            | "m4v"
            | "mkv"
            | "webm"
            | "mov"
            | "qt"
            | "avi"
            | "wmv"
            | "asf"
            | "flv"
            | "f4v"
            | "ogv"
            | "ogg"
            | "3gp"
            | "3g2"
            | "divx"
            | "xvid"
            // Broadcast, Optical Disc & Transport Streams
            | "ts"
            | "m2ts"
            | "mts"
            | "m2t"
            | "ps"
            | "mpeg"
            | "mpg"
            | "mpe"
            | "m1v"
            | "m2v"
            | "vob"
            | "evo"
            | "mxf"
            | "gxf"
            | "vvs"
            | "dvr-ms"
            | "wtv"
            | "mod"
            | "tod"
            // Editing, Acquisition & Cinema Mezzanine
            | "prores"
            | "dnxhd"
            | "dnxhr"
            | "braw"
            | "r3d"
            | "ari"
            | "arriraw"
            | "crm"
            | "xavc"
            | "xavcs"
            | "dv"
            | "dif"
            | "hdv"
            // Retro, Interactive & Streaming Containers
            | "rm"
            | "rmvb"
            | "bik"
            | "bik2"
            | "smk"
            | "roq"
            | "amv"
            | "mtv"
            | "nsv"
            | "ivf"
            | "y4m"
            | "yuv"
            | "dmb"
            | "pva"
            | "vqa"
            | "vmd"
            // 2026+ Next-Gen Video
            | "av2"
            | "av2s"
    )
}

/// True for a regular file (links followed). Only those are thumbnailed or shown as
/// their own preview: the UI loads a preview image on its own thread, and a FIFO there
/// would wait for a writer forever.
#[must_use]
pub fn is_regular_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// Most the thumbnail cache may hold; past it the oldest thumbnails go, down to 80%.
pub const CACHE_MAX_BYTES: u64 = 512 << 20;
/// A temporary thumbnail this old belongs to a run that crashed or was killed.
const STALE_TEMP: std::time::Duration = std::time::Duration::from_secs(3600);

/// Keeps `cache_dir` bounded. Nothing ever removed a thumbnail before: every image edit
/// (a new mtime, so a new name) and every file seen once stayed for good, and the
/// temporaries of a killed run were never cleared. Removes temporaries older than an
/// hour, then, while the cache is over `max_bytes`, the oldest thumbnails (they are made
/// again when needed) down to 80% of it. Only our own names are touched (16 hex digits
/// `.png`, or one of their temporaries), only regular files, nothing below. Returns how
/// many files went.
pub fn prune_cache_in(cache_dir: &Path, max_bytes: u64) -> usize {
    if !cache_dir_is_private(cache_dir) {
        return 0; // never delete in someone else's directory
    }
    let Ok(entries) = std::fs::read_dir(cache_dir) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    let mut kept = Vec::new();
    let mut total = 0u64;
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(hex) = name.get(..16).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit())) else { continue };
        let rest = &name[hex.len()..];
        let temp = rest.starts_with(".tmp-") && rest.ends_with(".png");
        if !(rest == ".png" || temp) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue }; // DirEntry::metadata does not follow links
        if !md.is_file() {
            continue;
        }
        let age = md.modified().ok().and_then(|m| now.duration_since(m).ok()).unwrap_or_default();
        if temp {
            if age >= STALE_TEMP && std::fs::remove_file(e.path()).is_ok() {
                removed += 1;
            }
            continue;
        }
        total += md.len();
        kept.push((md.modified().unwrap_or(std::time::UNIX_EPOCH), md.len(), e.path()));
    }
    if total > max_bytes {
        kept.sort_by_key(|(modified, _, _)| *modified);
        let goal = max_bytes / 10 * 8;
        for (_, len, path) in kept {
            if total <= goal {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
                removed += 1;
            }
        }
    }
    removed
}

/// [`prune_cache_in`] on the thumbnail cache, with [`CACHE_MAX_BYTES`].
pub fn prune_cache() -> usize {
    prune_cache_in(&thumbnail_cache_dir(), CACHE_MAX_BYTES)
}

/// Checks if a cached thumbnail exists in `cache_dir` and is valid on disk.
#[must_use]
pub fn lookup_cached_in(cache_dir: &Path, uri: &str, mtime_secs: i64) -> Option<PathBuf> {
    if !cache_dir_is_private(cache_dir) {
        return None;
    }
    let file = cache_dir.join(cache_filename(uri, mtime_secs));
    if file.is_file() && file.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Some(file);
    }
    None
}

/// Checks if a cached thumbnail already exists and is valid on disk.
#[must_use]
pub fn lookup_cached(uri: &str, mtime_secs: i64) -> Option<PathBuf> {
    lookup_cached_in(&thumbnail_cache_dir(), uri, mtime_secs)
}

pub type ThumbnailPixels = slint::SharedPixelBuffer<slint::Rgba8Pixel>;

/// Decode a cached or newly generated thumbnail on a worker. The UI receives pixels.
pub fn load_pixels(path: &Path) -> Option<ThumbnailPixels> {
    let mut reader = open_image(path)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(512);
    limits.max_image_height = Some(512);
    limits.max_alloc = Some(16 * 1024 * 1024);
    reader.limits(limits);
    let rgba = reader.decode().ok()?.into_rgba8();
    Some(ThumbnailPixels::clone_from_slice(rgba.as_raw(), rgba.width(), rgba.height()))
}

/// Generates a thumbnail for `source_path` and writes it to `cache_dir`.
/// Returns the path to the generated thumbnail on success.
pub fn generate_thumbnail_in(
    cache_dir: &Path,
    source_path: &Path,
    uri: &str,
    mtime_secs: i64,
    is_video: bool,
) -> Result<PathBuf, String> {
    // Regular files only (following links). A FIFO named like an image blocked the
    // decoder's open() for a writer that never comes, forever: on Quick Preview that was
    // the controller task, so the window froze. Devices and sockets are no images either.
    match std::fs::metadata(source_path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Err(format!("not a regular file: {}", source_path.display())),
        Err(e) => return Err(format!("cannot read {}: {e}", source_path.display())),
    }

    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    if let Err(e) = builder.create(cache_dir) {
        return Err(format!("failed to create thumbnail dir {}: {e}", cache_dir.display()));
    }
    if !cache_dir_is_private(cache_dir) {
        return Err(format!("thumbnail dir {} is not private to this user", cache_dir.display()));
    }

    let target_file = cache_dir.join(cache_filename(uri, mtime_secs));
    if target_file.is_file() && target_file.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(target_file);
    }
    let _ = std::fs::remove_file(&target_file);

    static THUMB_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = THUMB_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stem = target_file.file_stem().and_then(|s| s.to_str()).unwrap_or("thumb");
    let temp_target = cache_dir.join(format!("{stem}.tmp-{}-{count}.png", std::process::id()));

    // Ensure source path is absolute to prevent any switch injection vulnerabilities in external CLI tools
    let abs_source;
    let source_path = if source_path.is_absolute() {
        source_path
    } else if let Ok(cwd) = std::env::current_dir() {
        abs_source = cwd.join(source_path);
        &abs_source
    } else {
        source_path
    };

    let ext = source_path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);

    let result = if is_video || is_video_ext(ext.as_deref()) {
        generate_video_thumbnail(source_path, &temp_target)
    } else if ext.as_deref() == Some("svg") || ext.as_deref() == Some("svgz") {
        generate_svg_thumbnail(source_path, &temp_target)
    } else if is_raw_sensor_ext(ext.as_deref()) {
        generate_raw_thumbnail(source_path, &temp_target)
    } else {
        generate_image_thumbnail(source_path, &temp_target)
    };

    match result {
        Ok(()) => {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&temp_target, std::fs::Permissions::from_mode(0o600));
            if let Err(e) = std::fs::rename(&temp_target, &target_file) {
                let _ = std::fs::remove_file(&temp_target);
                return Err(format!("failed to store thumbnail {}: {e}", target_file.display()));
            }
            Ok(target_file)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&temp_target);
            Err(e)
        }
    }
}

/// Generates a thumbnail for `source_path` and writes it to the thumbnail cache.
/// Returns the path to the generated thumbnail on success.
pub fn generate_thumbnail(source_path: &Path, uri: &str, mtime_secs: i64, is_video: bool) -> Result<PathBuf, String> {
    generate_thumbnail_in(&thumbnail_cache_dir(), source_path, uri, mtime_secs, is_video)
}

/// The image decoder's reader for `source`: opened non-blocking and used only if it is
/// still a regular file, so a file swapped for a FIFO after the check cannot block it.
fn open_image(source: &Path) -> Option<image::ImageReader<std::io::BufReader<std::fs::File>>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(source).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut reader = image::ImageReader::new(std::io::BufReader::new(file));
    match image::ImageFormat::from_path(source) {
        Ok(format) => reader.set_format(format),
        Err(_) => reader = reader.with_guessed_format().ok()?,
    }
    Some(reader)
}

fn generate_image_thumbnail(source: &Path, target: &Path) -> Result<(), String> {
    let target_str = target.to_str().unwrap_or("");
    let target_png = format!("png:{target_str}");

    // Inspect image dimensions first without decoding full pixel buffer into memory.
    let dims = open_image(source).and_then(|r| r.into_dimensions().ok());
    if let Some((w, h)) = dims
        && (w > MAX_THUMBNAIL_SIDE || h > MAX_THUMBNAIL_SIDE || u64::from(w) * u64::from(h) > MAX_THUMBNAIL_PIXELS)
    {
        return Err(format!("{w}x{h} is too large for a thumbnail"));
    }
    let is_huge = dims.is_some_and(|(w, h)| w > 4096 || h > 4096 || (u64::from(w) * u64::from(h)) > 16_000_000);

    if !is_huge {
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(64 * 1024 * 1024);
        if let Some(mut reader) = open_image(source) {
            reader.limits(limits);
            if let Ok(img) = reader.decode() {
                let thumb = img.thumbnail(512, 512);
                if thumb.save_with_format(target, image::ImageFormat::Png).is_ok() {
                    return Ok(());
                }
            }
        }
    }

    // Fallback to ImageMagick if installed (efficient streaming and downsampling)
    if let Ok(status) = Command::new("magick")
        .args(MAGICK_LIMITS)
        .arg(source)
        .args(["-thumbnail", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    if let Ok(status) = Command::new("convert")
        .args(MAGICK_LIMITS)
        .arg(source)
        .args(["-thumbnail", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }

    // Fallback to ffmpeg for specialized/video-like formats
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-protocol_whitelist", "file", "-i"])
        .arg(source)
        .args([
            "-vf",
            "scale=512:512:force_original_aspect_ratio=decrease",
            "-frames:v",
            "1",
            "-update",
            "1",
            target_str,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.is_file()
        && target.metadata().map(|m| m.len() > 0).unwrap_or(false)
    {
        return Ok(());
    }

    Err(format!("image thumbnailing failed for {}", source.display()))
}

fn generate_raw_thumbnail(source: &Path, target: &Path) -> Result<(), String> {
    let target_str = target.to_str().unwrap_or("");
    let target_png = format!("png:{target_str}");

    // 1. Try exiftool to extract embedded JPEG preview (instant, handles CR2, CR3, NEF, ARW, DNG, RAF, etc.)
    for tag in ["-PreviewImage", "-JpgFromRaw", "-ThumbnailImage", "-OtherImage"] {
        // The embedded preview is decoded under the same memory bound as any image: a
        // hostile RAW can carry a "preview" that says it is gigapixels.
        if let Some(bytes) = timed_output(Command::new("exiftool").arg("-b").arg(tag).arg("--").arg(source), 32 << 20)
            && !bytes.is_empty()
            && let Ok(img) = decode_limited(&bytes)
        {
            let thumb = img.thumbnail(512, 512);
            if thumb.save_with_format(target, image::ImageFormat::Png).is_ok() && target.exists() {
                return Ok(());
            }
        }
    }

    // 2. Try ImageMagick (which interfaces with libraw)
    if let Ok(status) = Command::new("magick")
        .args(MAGICK_LIMITS)
        .arg(format!("{}[0]", source.display()))
        .args(["-resize", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    if let Ok(status) = Command::new("convert")
        .args(MAGICK_LIMITS)
        .arg(format!("{}[0]", source.display()))
        .args(["-resize", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }

    // 3. Try ffmpeg as final fallback
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-protocol_whitelist", "file", "-i"])
        .arg(source)
        .args([
            "-vf",
            "scale=512:512:force_original_aspect_ratio=decrease",
            "-frames:v",
            "1",
            "-update",
            "1",
            target_str,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.is_file()
        && target.metadata().map(|m| m.len() > 0).unwrap_or(false)
    {
        return Ok(());
    }

    Err(format!("raw thumbnailing failed: {}", source.display()))
}

fn generate_svg_thumbnail(source: &Path, target: &Path) -> Result<(), String> {
    let target_str = target.to_str().unwrap_or("");
    let target_png = format!("png:{target_str}");
    if let Ok(status) = Command::new("magick")
        .args(MAGICK_LIMITS)
        .arg(source)
        .args(["-background", "none", "-resize", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    if let Ok(status) = Command::new("convert")
        .args(MAGICK_LIMITS)
        .arg(source)
        .args(["-background", "none", "-resize", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    // As a fallback, try ffmpeg
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-protocol_whitelist", "file", "-i"])
        .arg(source)
        .args([
            "-vf",
            "scale=512:512:force_original_aspect_ratio=decrease",
            "-frames:v",
            "1",
            "-update",
            "1",
            "-f",
            "image2",
            "-c:v",
            "png",
            target_str,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    Err("failed to render SVG thumbnail".to_string())
}

fn generate_video_thumbnail(source: &Path, target: &Path) -> Result<(), String> {
    let target_str = target.to_str().unwrap_or("");

    // Try ffmpegthumbnailer first (using direct /usr/bin/ffmpegthumbnailer if present)
    let binary = if Path::new("/usr/bin/ffmpegthumbnailer").exists() {
        "/usr/bin/ffmpegthumbnailer"
    } else {
        "ffmpegthumbnailer"
    };

    if let Ok(status) = Command::new(binary)
        .arg("-i")
        .arg(source)
        .arg("-o")
        .arg(target)
        .args(["-s", "512", "-t", "10%"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.is_file()
        && target.metadata().map(|m| m.len() > 0).unwrap_or(false)
    {
        return Ok(());
    }
    let _ = std::fs::remove_file(target);

    // Robust ffmpeg thumbnail extraction using the thumbnail filter
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-protocol_whitelist", "file", "-i"])
        .arg(source)
        .args([
            "-vf",
            "thumbnail,scale=512:512:force_original_aspect_ratio=decrease",
            "-frames:v",
            "1",
            "-update",
            "1",
            target_str,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.is_file()
        && target.metadata().map(|m| m.len() > 0).unwrap_or(false)
    {
        return Ok(());
    }
    let _ = std::fs::remove_file(target);

    // Single-frame seek fallback if thumbnail filter found no candidate
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-protocol_whitelist", "file", "-i"])
        .arg(source)
        .args([
            "-vf",
            "scale=512:512:force_original_aspect_ratio=decrease",
            "-frames:v",
            "1",
            "-update",
            "1",
            target_str,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .timed_status()
        && status.success()
        && target.is_file()
        && target.metadata().map(|m| m.len() > 0).unwrap_or(false)
    {
        return Ok(());
    }
    let _ = std::fs::remove_file(target);

    Err("ffmpeg video frame extraction failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_candidate_detection() {
        assert!(is_previewable(Category::Image, None));
        assert!(is_previewable(Category::Video, None));
        assert!(is_previewable(Category::File, Some("png")));
        assert!(is_previewable(Category::File, Some("MP4")));
        assert!(is_previewable(Category::File, Some("heic")));
        assert!(is_previewable(Category::File, Some("jxl")));
        assert!(is_previewable(Category::File, Some("avif")));
        assert!(is_previewable(Category::File, Some("cr3")));
        assert!(is_previewable(Category::File, Some("nef")));
        assert!(is_previewable(Category::File, Some("exr")));
        assert!(is_previewable(Category::File, Some("dcm")));
        assert!(is_previewable(Category::File, Some("fits")));
        assert!(is_previewable(Category::File, Some("mxf")));
        assert!(is_previewable(Category::File, Some("braw")));
        assert!(is_previewable(Category::File, Some("av2")));
        assert!(!is_previewable(Category::File, Some("txt")));
        assert!(!is_previewable(Category::Folder, None));

        assert!(is_video_ext(Some("mp4")));
        assert!(is_video_ext(Some("MKV")));
        assert!(is_video_ext(Some("mxf")));
        assert!(is_video_ext(Some("braw")));
        assert!(is_video_ext(Some("av2")));
        assert!(!is_video_ext(Some("png")));

        assert!(is_raw_sensor_ext(Some("cr3")));
        assert!(is_raw_sensor_ext(Some("nef")));
        assert!(is_raw_sensor_ext(Some("dng")));
        assert!(!is_raw_sensor_ext(Some("png")));
    }

    fn red_png(dir: &Path) -> PathBuf {
        let p = dir.join("sample.png");
        image::RgbImage::from_pixel(16, 16, image::Rgb([255, 0, 0])).save(&p).unwrap_or_else(|e| panic!("{e}"));
        p
    }

    /// A PNG of a few hundred kilobytes that says it is 12000x12000: it went straight to
    /// ImageMagick (as "huge"), which took 1.15 GB of memory for it (about 8 bytes a
    /// pixel, so a 1 MB file could ask for gigabytes) on every folder view. Images this
    /// large get no thumbnail now, and no tool is started for them.
    #[test]
    fn a_decompression_bomb_gets_no_thumbnail_and_starts_no_tool() {
        use std::io::Write;
        let sandbox = std::env::temp_dir().join(format!("gungnir-thumb-bomb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sandbox);
        fs::create_dir_all(&sandbox).unwrap_or_else(|e| panic!("{e}"));
        let bomb = sandbox.join("bomb.png");
        let (w, h) = (12_000u32, 12_000u32);
        let chunk = |t: &[u8], d: &[u8]| {
            let mut c = (d.len() as u32).to_be_bytes().to_vec();
            c.extend_from_slice(t);
            c.extend_from_slice(d);
            let mut crc = crc32fast::Hasher::new();
            crc.update(t);
            crc.update(d);
            c.extend_from_slice(&crc.finalize().to_be_bytes());
            c
        };
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        let row = vec![0u8; w as usize + 1];
        for _ in 0..h {
            z.write_all(&row).unwrap_or_else(|e| panic!("{e}"));
        }
        let idat = z.finish().unwrap_or_else(|e| panic!("{e}"));
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend(chunk(b"IHDR", &ihdr));
        png.extend(chunk(b"IDAT", &idat));
        png.extend(chunk(b"IEND", b""));
        fs::write(&bomb, &png).unwrap_or_else(|e| panic!("{e}"));
        assert!(png.len() < 1 << 20, "the bomb is small: {} bytes", png.len());
        let cache = sandbox.join("cache");
        let t0 = std::time::Instant::now();
        let r = generate_thumbnail_in(&cache, &bomb, "file:///bomb.png", 1, false);
        assert!(r.is_err(), "a thumbnail was made of a {w}x{h} image");
        assert!(t0.elapsed() < std::time::Duration::from_secs(2), "{:?}", t0.elapsed());
        let _ = fs::remove_dir_all(&sandbox);
    }

    /// A tool that never finishes (a hostile file can hang a decoder) is stopped at its
    /// deadline and reaped, so it cannot hold a thumbnail worker forever.
    #[test]
    fn a_thumbnail_tool_that_hangs_is_stopped_at_its_deadline() {
        let t0 = std::time::Instant::now();
        let r = Command::new("sleep").arg("60").timed_status_within(std::time::Duration::from_millis(300));
        assert!(r.is_err_and(|e| e.kind() == std::io::ErrorKind::TimedOut));
        assert!(t0.elapsed() < std::time::Duration::from_secs(3), "{:?}", t0.elapsed());
        let ok = Command::new("true").timed_status_within(std::time::Duration::from_secs(5));
        assert!(ok.is_ok_and(|s| s.success()));
    }

    /// The fallback cache is a fixed name in /tmp: one planted there, a link to a
    /// directory of someone else's, received thumbnails of private files and its planted
    /// "thumbnails" were shown.
    #[test]
    fn a_cache_reached_through_a_link_to_someone_elses_directory_is_never_used() {
        let sandbox = std::env::temp_dir().join(format!("gungnir-thumb-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sandbox);
        fs::create_dir_all(&sandbox).unwrap_or_else(|e| panic!("{e}"));
        let img = red_png(&sandbox);
        // /tmp: root's, and writable by everyone, like a directory another user planted.
        let tmp = Path::new("/tmp");
        assert_ne!(std::os::unix::fs::MetadataExt::uid(&fs::metadata(tmp).unwrap_or_else(|e| panic!("{e}"))), unsafe {
            libc::geteuid()
        });
        let foreign = sandbox.join("foreign");
        std::os::unix::fs::symlink(tmp, &foreign).unwrap_or_else(|e| panic!("{e}"));
        let uri = format!("file:///private/photo-{}.png", std::process::id());
        let leaked = tmp.join(cache_filename(&uri, 7));
        let _ = fs::remove_file(&leaked);
        let written = generate_thumbnail_in(&foreign, &img, &uri, 7, false);
        let was_written = leaked.exists();
        let _ = fs::remove_file(&leaked);
        assert!(written.is_err() && !was_written, "a thumbnail of a private file went to /tmp");
        fs::write(&leaked, b"planted").unwrap_or_else(|e| panic!("{e}"));
        let shown = lookup_cached_in(&foreign, &uri, 7);
        let _ = fs::remove_file(&leaked);
        assert_eq!(shown, None, "a planted thumbnail was shown");
        assert_eq!(prune_cache_in(&foreign, 0), 0);
        // A link of the user's own to a directory of theirs (a cache moved to another
        // disk) keeps working.
        fs::create_dir(sandbox.join("ssd")).unwrap_or_else(|e| panic!("{e}"));
        let own = sandbox.join("own");
        std::os::unix::fs::symlink(sandbox.join("ssd"), &own).unwrap_or_else(|e| panic!("{e}"));
        let thumb = generate_thumbnail_in(&own, &img, "file:///p.png", 7, false).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(lookup_cached_in(&own, "file:///p.png", 7), Some(thumb));
        let _ = fs::remove_dir_all(&sandbox);
    }

    /// A cache directory of ours that others can enter or write is made 0700 before use.
    #[test]
    fn a_loose_cache_directory_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let sandbox = std::env::temp_dir().join(format!("gungnir-thumb-loose-{}", std::process::id()));
        let _ = fs::remove_dir_all(&sandbox);
        let cache = sandbox.join("cache");
        fs::create_dir_all(&cache).unwrap_or_else(|e| panic!("{e}"));
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o777)).unwrap_or_else(|e| panic!("{e}"));
        let img = red_png(&sandbox);
        assert!(generate_thumbnail_in(&cache, &img, "file:///p.png", 1, false).is_ok());
        let mode = fs::metadata(&cache).map(|m| m.permissions().mode() & 0o777).unwrap_or(0);
        assert_eq!(mode, 0o700);
        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn test_image_thumbnail_generation() {
        let sandbox = std::env::temp_dir().join("gungnir-test-sandbox").join("img_test");
        let _ = fs::create_dir_all(&sandbox);
        let cache_dir = sandbox.join("cache");

        let img_path = sandbox.join("sample.png");
        // Create a 16x16 red PNG
        let img = image::RgbImage::from_pixel(16, 16, image::Rgb([255, 0, 0]));
        img.save(&img_path).unwrap_or_else(|e| panic!("{e}"));

        let uri = "file:///tmp/sample.png";
        let res = generate_thumbnail_in(&cache_dir, &img_path, uri, 12345, false);
        assert!(res.is_ok(), "thumbnail generation must succeed");

        let thumb = res.unwrap_or_else(|e| panic!("{e}"));
        assert!(thumb.is_file(), "thumbnail file must exist");

        let cached = lookup_cached_in(&cache_dir, uri, 12345);
        assert_eq!(cached, Some(thumb.clone()));

        let pixels = load_pixels(&thumb).unwrap_or_else(|| panic!("generated thumbnail did not decode"));
        fs::remove_file(&thumb).unwrap_or_else(|e| panic!("{e}"));
        let size = slint::Image::from_rgba8(pixels).size();
        assert_eq!((size.width, size.height), (512, 512));

        // Cleanup
        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn test_video_thumbnail_generation() {
        let sandbox = std::env::temp_dir().join("gungnir-test-sandbox").join("vid_test");
        let _ = fs::create_dir_all(&sandbox);
        let cache_dir = sandbox.join("cache");

        let vid_path = sandbox.join("sample.mp4");
        // Create a 1-second synthetic MP4 using ffmpeg
        let status = Command::new("ffmpeg")
            .args(["-y", "-f", "lavfi", "-i", "testsrc=duration=1:size=320x240:rate=1"])
            .arg(&vid_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        // No silent pass: without ffmpeg this test fails instead of checking nothing.
        assert!(
            status.as_ref().is_ok_and(std::process::ExitStatus::success) && vid_path.is_file(),
            "ffmpeg could not make the sample video ({status:?}); video thumbnails need ffmpeg"
        );
        let uri = "file:///tmp/sample.mp4";
        let res = generate_thumbnail_in(&cache_dir, &vid_path, uri, 54321, true);
        assert!(res.is_ok(), "video thumbnail generation must succeed: {:?}", res.err());

        let thumb = res.unwrap_or_else(|e| panic!("{e}"));
        assert!(thumb.is_file(), "thumbnail file must exist");

        let slint_img = slint::Image::load_from_path(&thumb);
        assert!(slint_img.is_ok(), "slint must be able to load video thumbnail: {:?}", slint_img.err());

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn test_svg_thumbnail_generation() {
        let sandbox = std::env::temp_dir().join("gungnir-test-sandbox").join("svg_test");
        let _ = fs::create_dir_all(&sandbox);
        let cache_dir = sandbox.join("cache");

        let svg_path = sandbox.join("vector.svg");
        let svg_content = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><circle cx="50" cy="50" r="40" fill="purple"/></svg>"#;
        fs::write(&svg_path, svg_content).unwrap_or_else(|e| panic!("{e}"));
        let uri = "file:///tmp/vector.svg";
        // No silent pass: a failed render fails the test.
        let thumb = generate_thumbnail_in(&cache_dir, &svg_path, uri, 99999, false)
            .unwrap_or_else(|e| panic!("svg thumbnail generation must succeed: {e}"));
        assert!(thumb.is_file(), "svg thumbnail file must exist");
        let slint_img = slint::Image::load_from_path(&thumb);
        assert!(slint_img.is_ok(), "slint must load svg rendered png");

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn the_cache_is_pruned_oldest_first_and_only_our_own_files() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join("gungnir-test-sandbox").join(format!("prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let aged = |name: &str, len: usize, ago_secs: u64| {
            let p = dir.join(name);
            fs::write(&p, vec![7u8; len]).unwrap();
            let f = fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(SystemTime::now() - Duration::from_secs(ago_secs)).unwrap();
            p
        };
        // Five thumbnails of 1000 bytes, oldest first.
        let thumbs: Vec<_> = (0..5).map(|i| aged(&format!("{i:016x}.png"), 1000, 5000 - i * 100)).collect();
        let stale_tmp = aged("aaaaaaaaaaaaaaaa.tmp-1-0.png", 10, 7200);
        let fresh_tmp = aged("bbbbbbbbbbbbbbbb.tmp-2-0.png", 10, 10);
        let foreign = aged("notes.txt", 50_000, 9000);
        let outside = aged("outside.bin", 10, 9000);
        std::os::unix::fs::symlink(&outside, dir.join("cccccccccccccccc.png")).unwrap();
        fs::create_dir(dir.join("dddddddddddddddd.png")).unwrap();

        // 5000 bytes over a 3000 cap: down to 80% (2400), so the 3 oldest go, plus the stale temp.
        assert_eq!(prune_cache_in(&dir, 3000), 4);
        assert!(thumbs[..3].iter().all(|p| !p.exists()));
        assert!(thumbs[3..].iter().all(|p| p.exists()));
        assert!(!stale_tmp.exists() && fresh_tmp.exists());
        assert!(foreign.exists() && outside.exists(), "only our own files are touched");
        assert!(dir.join("cccccccccccccccc.png").is_symlink() && dir.join("dddddddddddddddd.png").is_dir());
        // Under the cap: nothing more goes.
        assert_eq!(prune_cache_in(&dir, 3000), 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
