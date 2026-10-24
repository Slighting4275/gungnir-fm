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
        // Universal & Web Raster
        "png" | "jpg" | "jpeg" | "jpe" | "jif" | "jfif" | "jfi" | "gif" | "bmp" | "dib"
        | "tiff" | "tif" | "bigtiff" | "webp" | "ico" | "cur" | "icns"
        // High-Efficiency & Modern Still Encodings
        | "avif" | "avis" | "heic" | "heif" | "heifs" | "jxl" | "jp2" | "j2k" | "jpx" | "jpm" | "j2c"
        | "jxr" | "hdp" | "wdp" | "jxs" | "flif" | "fuif" | "bpg" | "qoi" | "ff" | "farbfeld"
        // Vector & Procedural Graphics
        | "svg" | "svgz" | "eps" | "ps" | "ai" | "cdr" | "wmf" | "emf" | "cgm" | "dxf" | "dwg"
        | "gbr" | "gerber" | "xar" | "sk1" | "fla" | "swf" | "vml" | "pgml"
        // RAW Sensor Formats
        | "dng" | "crw" | "cr2" | "cr3" | "nef" | "nrw" | "arw" | "srf" | "sr2" | "raf"
        | "orf" | "om" | "rw2" | "pef" | "kdc" | "dcr" | "dcs" | "mos" | "mef" | "3fr"
        | "fff" | "iiq" | "x3f" | "erf" | "rwl" | "srw" | "gpr" | "mrw" | "bay" | "cap"
        // Professional HDR, VFX, 3D Textures & Compositing
        | "exr" | "hdr" | "pic" | "rgbe" | "dpx" | "cin" | "dds" | "ktx" | "ktx2" | "astc"
        | "pvr" | "pvrtc" | "pkm" | "vtf" | "tga" | "targa" | "psd" | "psb" | "xcf" | "cpt"
        | "afphoto" | "pdn" | "kra" | "ora" | "iff" | "ilbm" | "lbm" | "sgi" | "rgb" | "rgba" | "bw"
        // Scientific, Medical, Geospatial
        | "fits" | "fit" | "fts" | "dcm" | "dicom" | "geotiff" | "nitf" | "ntf" | "sid" | "mrsid"
        | "ecw" | "h5" | "hdf5" | "hdf" | "nc" | "netcdf" | "svs" | "ndpi" | "ometiff" | "dm3" | "dm4"
        | "mrc" | "nii" | "vtk"
        // Legacy, Retro & Specialized Bitmaps
        | "pcx" | "pbm" | "pgm" | "ppm" | "pam" | "pnm" | "xbm" | "xpm" | "xwd" | "ras" | "sun"
        | "mac" | "pnt" | "gem" | "wbmp" | "pict" | "pct" | "cut" | "art" | "iwi" | "blp"
        | "tim" | "tim2" | "gxt"
        // Animated & Multi-Picture Containers
        | "apng" | "mng" | "jng" | "mpo" | "fli" | "flc"
        // 3D / Spatial / Splatting
        | "splat" | "spz" | "ply" => Category::Image,

        // Audio Formats
        "flac" | "wav" | "mp3" | "ogg" | "opus" | "m4a" | "aac" | "aiff" | "alac" | "mid" | "midi" => Category::Audio,

        // Universal, Web & Consumer Containers
        "mkv" | "mp4" | "m4v" | "webm" | "mov" | "qt" | "avi" | "wmv" | "asf" | "flv" | "f4v"
        | "ogv" | "3gp" | "3g2" | "divx" | "xvid"
        // Broadcast, Optical Disc & Transport Streams
        | "m2ts" | "mts" | "m2t" | "mpeg" | "mpg" | "mpe" | "m1v" | "m2v"
        | "vob" | "evo" | "mxf" | "gxf" | "vvs" | "dvr-ms" | "wtv" | "mod" | "tod"
        // Editing, Acquisition & Cinema Mezzanine
        | "prores" | "dnxhd" | "dnxhr" | "braw" | "r3d" | "ari" | "arriraw" | "crm"
        | "xavc" | "xavcs" | "dv" | "dif" | "hdv"
        // Retro, Interactive & Streaming Containers
        | "rm" | "rmvb" | "bik" | "bik2" | "smk" | "roq" | "amv" | "mtv" | "nsv" | "ivf"
        | "y4m" | "yuv" | "dmb" | "pva" | "vqa" | "vmd"
        // 2026+ Next-Gen Video
        | "av2" | "av2s" => Category::Video,
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
        // Universal & Web Raster
        "png" => "PNG image",
        "jpg" | "jpeg" | "jpe" | "jif" | "jfif" | "jfi" => "JPEG image",
        "gif" => "GIF image",
        "bmp" | "dib" => "Bitmap image",
        "tiff" | "tif" | "bigtiff" => "TIFF image",
        "webp" => "WebP image",
        "ico" => "Icon resource",
        "cur" => "Cursor resource",
        "icns" => "Apple Icon resource",

        // High-Efficiency & Modern Still Encodings
        "avif" => "AVIF image",
        "avis" => "AVIF sequence",
        "heic" | "heif" => "HEIF image",
        "heifs" => "HEIF sequence",
        "jxl" => "JPEG XL image",
        "jp2" | "j2k" | "jpx" | "jpm" | "j2c" => "JPEG 2000 image",
        "jxr" | "hdp" | "wdp" => "JPEG XR image",
        "jxs" => "JPEG XS image",
        "flif" => "FLIF image",
        "fuif" => "FUIF image",
        "bpg" => "BPG image",
        "qoi" => "QOI image",
        "ff" | "farbfeld" => "Farbfeld image",

        // Vector & Procedural Graphics
        "svg" => "SVG vector image",
        "svgz" => "Compressed SVG image",
        "eps" => "Encapsulated PostScript",
        "ps" => "PostScript document",
        "ai" => "Adobe Illustrator artwork",
        "cdr" => "CorelDRAW vector drawing",
        "wmf" => "Windows Metafile",
        "emf" => "Enhanced Metafile",
        "cgm" => "Computer Graphics Metafile",
        "dxf" => "AutoCAD Drawing Exchange",
        "dwg" => "AutoCAD Drawing",
        "gbr" | "gerber" => "Gerber PCB format",
        "xar" => "Xara drawing",
        "sk1" => "sK1 vector graphics",
        "fla" | "swf" => "Flash animation",
        "vml" => "Vector Markup Language",
        "pgml" => "Precision Graphics Markup",

        // RAW Sensor Formats
        "dng" => "Adobe Digital Negative",
        "crw" | "cr2" | "cr3" => "Canon Raw image",
        "nef" | "nrw" => "Nikon Raw image",
        "arw" | "srf" | "sr2" => "Sony Raw image",
        "raf" => "Fujifilm Raw image",
        "orf" | "om" => "Olympus Raw image",
        "rw2" => "Panasonic Lumix Raw",
        "pef" => "Pentax Raw image",
        "kdc" | "dcr" | "dcs" => "Kodak Raw image",
        "mos" | "mef" => "Mamiya / Leaf Raw",
        "3fr" | "fff" => "Hasselblad Raw image",
        "iiq" => "Phase One Raw image",
        "x3f" => "Sigma Foveon Raw",
        "erf" => "Epson Raw image",
        "rwl" => "Leica Raw image",
        "srw" => "Samsung Raw image",
        "gpr" => "GoPro Protune Raw",
        "mrw" => "Minolta Raw image",
        "bay" => "Casio Raw image",
        "cap" => "Capture One Raw",

        // Professional HDR, VFX, 3D Textures & Compositing
        "exr" => "OpenEXR HDR image",
        "hdr" | "pic" | "rgbe" => "Radiance HDR image",
        "dpx" => "DPX digital picture exchange",
        "cin" => "Cineon film image",
        "dds" => "DirectDraw Surface texture",
        "ktx" | "ktx2" => "Khronos texture container",
        "astc" => "ASTC texture",
        "pvr" | "pvrtc" => "PowerVR texture",
        "pkm" => "Ericsson texture",
        "vtf" => "Valve texture format",
        "tga" | "targa" => "Truevision Targa image",
        "psd" | "psb" => "Adobe Photoshop document",
        "xcf" => "GIMP image document",
        "cpt" => "Corel Photo-Paint document",
        "afphoto" => "Affinity Photo document",
        "pdn" => "Paint.NET document",
        "kra" => "Krita document",
        "ora" => "OpenRaster image",
        "iff" | "ilbm" | "lbm" => "Amiga IFF/ILBM bitmap",
        "sgi" | "rgb" | "rgba" | "bw" => "Silicon Graphics image",

        // Scientific, Medical, Geospatial
        "fits" | "fit" | "fts" => "FITS astronomical image",
        "dcm" | "dicom" => "DICOM medical image",
        "geotiff" => "GeoTIFF raster",
        "nitf" | "ntf" => "NITF imagery",
        "sid" | "mrsid" => "MrSID geospatial image",
        "ecw" => "ECW wavelet image",
        "h5" | "hdf5" | "hdf" | "nc" | "netcdf" => "Scientific dataset",
        "svs" => "Aperio pathology image",
        "ndpi" => "Hamamatsu pathology image",
        "ometiff" => "OME-TIFF microscopy image",
        "dm3" | "dm4" => "Gatan microscopy image",
        "mrc" => "MRC cryo-EM map",
        "nii" => "NIfTI neuroimaging volume",
        "vtk" => "VTK structured grid",

        // Legacy, Retro & Specialized Bitmaps
        "pcx" => "ZSoft Paintbrush bitmap",
        "pbm" => "Portable BitMap",
        "pgm" => "Portable GrayMap",
        "ppm" => "Portable PixMap",
        "pam" | "pnm" => "Portable Anymap",
        "xbm" => "X11 bitmap",
        "xpm" => "X11 pixmap",
        "xwd" => "X Window dump",
        "ras" | "sun" => "Sun raster image",
        "mac" | "pnt" => "MacPaint image",
        "gem" => "GEM bitmap",
        "wbmp" => "Wireless bitmap",
        "pict" | "pct" => "Macintosh QuickDraw picture",
        "cut" => "Dr. Halo cut",
        "art" => "AOL Johnson-Grace image",
        "iwi" => "Infinity Ward image",
        "blp" => "Blizzard texture",
        "tim" | "tim2" => "PlayStation texture",
        "gxt" => "PlayStation Vita texture",

        // Animated & Multi-Picture Containers
        "apng" => "Animated PNG",
        "mng" => "Multiple-image Network Graphics",
        "jng" => "JPEG Network Graphics",
        "mpo" => "Multi-Picture 3D image",
        "fli" | "flc" => "Autodesk Animator FLIC",

        // 3D / Spatial / Splatting
        "splat" | "spz" => "3D Gaussian Splatting scene",
        "ply" => "Point Cloud polygon file",

        // Universal, Web & Consumer Video Containers
        "mkv" => "Matroska video",
        "mp4" | "m4v" => "MP4 video",
        "webm" => "WebM video",
        "mov" | "qt" => "QuickTime movie",
        "avi" => "AVI video",
        "wmv" | "asf" => "Windows Media video",
        "flv" | "f4v" => "Flash video",
        "ogv" => "Ogg video",
        "3gp" | "3g2" => "3GPP mobile video",
        "divx" | "xvid" => "DivX / Xvid video",

        // Broadcast, Optical Disc & Transport Streams
        "ts" | "m2ts" | "mts" | "m2t" => "MPEG transport stream",
        "mpeg" | "mpg" | "mpe" | "m1v" | "m2v" => "MPEG program stream",
        "vob" => "DVD Video Object",
        "evo" => "HD DVD Video Object",
        "mxf" => "Material Exchange Format",
        "gxf" => "General Exchange Format",
        "vvs" => "Variable Video Stream",
        "dvr-ms" | "wtv" => "Recorded TV show",
        "mod" | "tod" => "Camcorder video stream",

        // Editing, Acquisition & Cinema Mezzanine
        "prores" => "Apple ProRes video",
        "dnxhd" | "dnxhr" => "Avid DNxHD video",
        "braw" => "Blackmagic RAW video",
        "r3d" => "REDCODE RAW video",
        "ari" | "arriraw" => "ARRIRAW video",
        "crm" => "Canon Cinema RAW Light",
        "xavc" | "xavcs" => "Sony XAVC video",
        "dv" | "dif" => "Digital Video stream",
        "hdv" => "HDV video stream",

        // Retro, Interactive & Streaming Containers
        "rm" | "rmvb" => "RealMedia video",
        "bik" | "bik2" => "Bink video",
        "smk" => "Smacker video",
        "roq" => "Id Software RoQ video",
        "amv" | "mtv" => "MTV portable media video",
        "nsv" => "Nullsoft Streaming Video",
        "ivf" => "Indeo / IVF video",
        "y4m" => "YUV4MPEG2 video stream",
        "yuv" => "Raw YUV video stream",
        "dmb" | "pva" => "DVB broadcast stream",
        "vqa" => "Westwood VQA video",
        "vmd" => "Sierra VMD video",

        // 2026+ Next-Gen Video
        "av2" | "av2s" => "AOMedia Video 2 (AV2)",

        // Code and Documents
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
        assert_eq!(category(&entry("photo.cr3", FileKind::File, 0o644)), Category::Image);
        assert_eq!(category(&entry("render.exr", FileKind::File, 0o644)), Category::Image);
        assert_eq!(category(&entry("scan.dcm", FileKind::File, 0o644)), Category::Image);
        assert_eq!(category(&entry("cinema.braw", FileKind::File, 0o644)), Category::Video);
        assert_eq!(category(&entry("stream.mxf", FileKind::File, 0o644)), Category::Video);
        assert_eq!(category(&entry("nextgen.av2", FileKind::File, 0o644)), Category::Video);
        assert_eq!(type_label(&entry("main.rs", FileKind::File, 0o644)), "Rust source");
        assert_eq!(type_label(&entry("photo.cr3", FileKind::File, 0o644)), "Canon Raw image");
        assert_eq!(type_label(&entry("render.exr", FileKind::File, 0o644)), "OpenEXR HDR image");
        assert_eq!(type_label(&entry("cinema.braw", FileKind::File, 0o644)), "Blackmagic RAW video");
        assert_eq!(type_label(&entry("a.qux", FileKind::File, 0o644)), "QUX file");
        assert_eq!(extension(".bashrc"), None);
    }
}
