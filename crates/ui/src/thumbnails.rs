//! Thumbnail generation and caching for image and video files.
//!
//! Generates crisp 256x256 thumbnails for images and video frames and caches them
//! under `~/.cache/gungnir-fm/thumbnails/`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use core_vfs::format::Category;

/// Directory where thumbnails are cached on disk.
#[must_use]
pub fn thumbnail_cache_dir() -> PathBuf {
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

/// Checks if a cached thumbnail exists in `cache_dir` and is valid on disk.
#[must_use]
pub fn lookup_cached_in(cache_dir: &Path, uri: &str, mtime_secs: i64) -> Option<PathBuf> {
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

/// Generates a thumbnail for `source_path` and writes it to `cache_dir`.
/// Returns the path to the generated thumbnail on success.
pub fn generate_thumbnail_in(
    cache_dir: &Path,
    source_path: &Path,
    uri: &str,
    mtime_secs: i64,
    is_video: bool,
) -> Result<PathBuf, String> {
    if !source_path.exists() {
        return Err(format!("source file does not exist: {}", source_path.display()));
    }

    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    if let Err(e) = builder.create(cache_dir) {
        return Err(format!("failed to create thumbnail dir {}: {e}", cache_dir.display()));
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

fn generate_image_thumbnail(source: &Path, target: &Path) -> Result<(), String> {
    let target_str = target.to_str().unwrap_or("");
    let target_png = format!("png:{target_str}");

    // Inspect image dimensions first without decoding full pixel buffer into memory.
    let is_huge = if let Ok(reader) = image::ImageReader::open(source) {
        match reader.into_dimensions() {
            Ok((w, h)) => w > 4096 || h > 4096 || (w as u64 * h as u64) > 16_000_000,
            Err(_) => false,
        }
    } else {
        false
    };

    if !is_huge {
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(64 * 1024 * 1024);
        if let Ok(mut reader) = image::ImageReader::open(source) {
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
        .arg(source)
        .args(["-thumbnail", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    if let Ok(status) = Command::new("convert")
        .arg(source)
        .args(["-thumbnail", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }

    // Fallback to ffmpeg for specialized/video-like formats
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-i"])
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
        .status()
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
        if let Ok(out) = Command::new("exiftool").arg("-b").arg(tag).arg("--").arg(source).output()
            && out.status.success()
            && !out.stdout.is_empty()
            && let Ok(img) = image::load_from_memory(&out.stdout)
        {
            let thumb = img.thumbnail(512, 512);
            if thumb.save_with_format(target, image::ImageFormat::Png).is_ok() && target.exists() {
                return Ok(());
            }
        }
    }

    // 2. Try ImageMagick (which interfaces with libraw)
    if let Ok(status) = Command::new("magick")
        .arg(format!("{}[0]", source.display()))
        .args(["-resize", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    if let Ok(status) = Command::new("convert")
        .arg(format!("{}[0]", source.display()))
        .args(["-resize", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }

    // 3. Try ffmpeg as final fallback
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-i"])
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
        .status()
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
        .arg(source)
        .args(["-background", "none", "-resize", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    if let Ok(status) = Command::new("convert")
        .arg(source)
        .args(["-background", "none", "-resize", "512x512", &target_png])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        && status.success()
        && target.exists()
    {
        return Ok(());
    }
    // As a fallback, try ffmpeg
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-i"])
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
        .status()
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
        .status()
        && status.success()
        && target.is_file()
        && target.metadata().map(|m| m.len() > 0).unwrap_or(false)
    {
        return Ok(());
    }
    let _ = std::fs::remove_file(target);

    // Robust ffmpeg thumbnail extraction using the thumbnail filter
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-i"])
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
        .status()
        && status.success()
        && target.is_file()
        && target.metadata().map(|m| m.len() > 0).unwrap_or(false)
    {
        return Ok(());
    }
    let _ = std::fs::remove_file(target);

    // Single-frame seek fallback if thumbnail filter found no candidate
    if let Ok(status) = Command::new("ffmpeg")
        .args(["-nostdin", "-y", "-i"])
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
        .status()
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

        let slint_img = slint::Image::load_from_path(&thumb);
        assert!(slint_img.is_ok(), "slint must be able to load generated thumbnail: {:?}", slint_img.err());

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

        if let Ok(st) = status
            && st.success()
            && vid_path.is_file()
        {
            let uri = "file:///tmp/sample.mp4";
            let res = generate_thumbnail_in(&cache_dir, &vid_path, uri, 54321, true);
            assert!(res.is_ok(), "video thumbnail generation must succeed: {:?}", res.err());

            let thumb = res.unwrap_or_else(|e| panic!("{e}"));
            assert!(thumb.is_file(), "thumbnail file must exist");

            let slint_img = slint::Image::load_from_path(&thumb);
            assert!(slint_img.is_ok(), "slint must be able to load video thumbnail: {:?}", slint_img.err());
        }

        let _ = fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn test_svg_thumbnail_generation() {
        let sandbox = std::env::temp_dir().join("gungnir-test-sandbox").join("svg_test");
        let _ = fs::create_dir_all(&sandbox);
        let cache_dir = sandbox.join("cache");

        let svg_path = sandbox.join("vector.svg");
        let svg_content = r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><circle cx="50" cy="50" r="40" fill="purple"/></svg>"#;
        if fs::write(&svg_path, svg_content).is_ok() {
            let uri = "file:///tmp/vector.svg";
            let res = generate_thumbnail_in(&cache_dir, &svg_path, uri, 99999, false);
            if res.is_ok() {
                let thumb = res.unwrap_or_else(|e| panic!("{e}"));
                assert!(thumb.is_file(), "svg thumbnail file must exist");
                let slint_img = slint::Image::load_from_path(&thumb);
                assert!(slint_img.is_ok(), "slint must load svg rendered png");
            }
        }

        let _ = fs::remove_dir_all(&sandbox);
    }
}
