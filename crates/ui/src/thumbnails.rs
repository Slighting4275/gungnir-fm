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
        std::env::temp_dir().join("gungnir-fm-thumbnails")
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
    matches!(
        e.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "bmp"
            | "ico"
            | "tiff"
            | "tif"
            | "svg"
            | "avif"
            | "pnm"
            | "tga"
            | "mp4"
            | "mkv"
            | "webm"
            | "mov"
            | "avi"
            | "flv"
            | "wmv"
            | "m4v"
            | "mpg"
            | "mpeg"
            | "3gp"
            | "ts"
            | "ogv"
    )
}

/// Checks whether an extension corresponds to a video file.
#[must_use]
pub fn is_video_ext(ext: Option<&str>) -> bool {
    let Some(ext) = ext else { return false };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "flv" | "wmv" | "m4v" | "mpg" | "mpeg" | "3gp" | "ts" | "ogv"
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

    if let Err(e) = std::fs::create_dir_all(cache_dir) {
        return Err(format!("failed to create thumbnail dir {}: {e}", cache_dir.display()));
    }

    let target_file = cache_dir.join(cache_filename(uri, mtime_secs));
    if target_file.is_file() && target_file.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Ok(target_file);
    }
    let _ = std::fs::remove_file(&target_file);

    let stem = target_file.file_stem().and_then(|s| s.to_str()).unwrap_or("thumb");
    let temp_target = cache_dir.join(format!("{stem}.tmp-{}.png", std::process::id()));

    let ext = source_path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);

    let result = if is_video || is_video_ext(ext.as_deref()) {
        generate_video_thumbnail(source_path, &temp_target)
    } else if ext.as_deref() == Some("svg") {
        generate_svg_thumbnail(source_path, &temp_target)
    } else {
        generate_image_thumbnail(source_path, &temp_target)
    };

    match result {
        Ok(()) => {
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
pub fn generate_thumbnail(
    source_path: &Path,
    uri: &str,
    mtime_secs: i64,
    is_video: bool,
) -> Result<PathBuf, String> {
    generate_thumbnail_in(&thumbnail_cache_dir(), source_path, uri, mtime_secs, is_video)
}

fn generate_image_thumbnail(source: &Path, target: &Path) -> Result<(), String> {
    // Primary path: fast in-process image crate
    match image::open(source) {
        Ok(img) => {
            let thumb = img.thumbnail(256, 256);
            thumb
                .save_with_format(target, image::ImageFormat::Png)
                .map_err(|e| format!("failed to save thumbnail {}: {e}", target.display()))
        }
        Err(err) => {
            let target_str = target.to_str().unwrap_or("");
            let target_png = format!("png:{target_str}");
            // Fallback to ImageMagick if installed
            if let Ok(status) = Command::new("magick")
                .arg(source)
                .args(["-resize", "256x256", &target_png])
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
                .args(["-resize", "256x256", &target_png])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                && status.success()
                && target.exists()
            {
                return Ok(());
            }
            Err(format!("image thumbnailing failed: {err}"))
        }
    }
}

fn generate_svg_thumbnail(source: &Path, target: &Path) -> Result<(), String> {
    let target_str = target.to_str().unwrap_or("");
    let target_png = format!("png:{target_str}");
    if let Ok(status) = Command::new("magick")
        .arg(source)
        .args(["-background", "none", "-resize", "256x256", &target_png])
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
        .args(["-background", "none", "-resize", "256x256", &target_png])
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
        .args(["-y", "-i"])
        .arg(source)
        .args([
            "-vf",
            "scale=256:256:force_original_aspect_ratio=decrease",
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
        .args(["-s", "256", "-t", "10%"])
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
        .args(["-y", "-i"])
        .arg(source)
        .args([
            "-vf",
            "thumbnail,scale=256:256:force_original_aspect_ratio=decrease",
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
        .args(["-y", "-i"])
        .arg(source)
        .args([
            "-vf",
            "scale=256:256:force_original_aspect_ratio=decrease",
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
        assert!(!is_previewable(Category::File, Some("txt")));
        assert!(!is_previewable(Category::Folder, None));

        assert!(is_video_ext(Some("mp4")));
        assert!(is_video_ext(Some("MKV")));
        assert!(!is_video_ext(Some("png")));
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
}
