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
    let name_lower = e.name.to_ascii_lowercase();
    match name_lower.as_str() {
        "makefile" | "dockerfile" | "vagrantfile" | "justfile" | "gemfile" | "poscar" | "potcar" | "incar"
        | "kpoints" => return Category::Code,
        "perf.data" | "manifest" | "core" => return Category::Document,
        _ => {}
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
        | "fff" | "iiq" | "x3f" | "erf" | "rwl" | "srw" | "gpr" | "mrw" | "bay"
        // Professional HDR, VFX, 3D Textures & Compositing
        | "exr" | "hdr" | "pic" | "rgbe" | "dpx" | "cin" | "dds" | "ktx" | "ktx2" | "astc"
        | "pvr" | "pvrtc" | "pkm" | "vtf" | "tga" | "targa" | "psd" | "psb" | "xcf" | "cpt"
        | "afphoto" | "pdn" | "kra" | "ora" | "iff" | "ilbm" | "lbm" | "sgi" | "rgb" | "rgba" | "bw"
        // Scientific, Medical, Geospatial
        | "fits" | "fit" | "fts" | "dcm" | "dicom" | "geotiff" | "nitf" | "ntf" | "mrsid"
        | "ecw" | "h5" | "hdf5" | "hdf" | "netcdf" | "svs" | "ndpi" | "ometiff" | "dm3" | "dm4"
        | "mrc" | "nii" | "vtk"
        // Legacy, Retro & Specialized Bitmaps
        | "pcx" | "pbm" | "pgm" | "ppm" | "pam" | "pnm" | "xbm" | "xpm" | "xwd" | "ras" | "sun"
        | "mac" | "pnt" | "gem" | "wbmp" | "pict" | "pct" | "cut" | "art" | "iwi" | "blp"
        | "tim" | "tim2" | "gxt"
        // Animated & Multi-Picture Containers
        | "apng" | "mng" | "jng" | "mpo" | "fli" | "flc"
        // 3D / Spatial / Splatting
        | "splat" | "spz" | "ply" => Category::Image,

        // Audio Formats (Lossless, Lossy, Trackers, Chiptunes, Soundfonts, Synthesizers)
        "flac" | "alac" | "wav" | "aiff" | "aif" | "aifc" | "ape" | "wv" | "tta" | "shn"
        | "tak" | "ofr" | "ofs" | "dsd" | "dsf" | "dff" | "dxd" | "mlp" | "wma" | "bwf"
        | "rf64" | "caf" | "au" | "snd" | "w64" | "mp3" | "aac" | "m4a" | "opus" | "ogg"
        | "oga" | "ac3" | "a52" | "eac3" | "truehd" | "dts" | "dtshd" | "dtsx" | "oma"
        | "omg" | "aa3" | "mpc" | "mpp" | "mp+" | "spx" | "amr" | "awb" | "gsm" | "g711"
        | "alaw" | "ulaw" | "g722" | "g726" | "g729" | "ilbc" | "silk" | "qdm" | "qdm2"
        | "vselp" | "voc" | "qcp" | "mid" | "midi" | "smf" | "rmi" | "rmid" | "kar"
        | "mod" | "xm" | "s3m" | "it" | "mtm" | "umx" | "669" | "far" | "okt" | "sid"
        | "nsf" | "nsfe" | "spc" | "vgm" | "vgz" | "gym" | "hes" | "gbs" | "kss" | "ay"
        | "sap" | "sf2" | "sf3" | "sfz" | "nki" | "gig" | "8svx" | "rol" | "cmf" | "imf"
        | "mus" | "hmi" | "hmp" | "xmi" | "rad" | "hsc" | "sng" | "dwd" | "kmp" | "ksf"
        | "adm" | "sofa" | "sir" | "adv" => Category::Audio,

        // Universal, Web & Consumer Containers
        "mkv" | "mp4" | "m4v" | "webm" | "mov" | "qt" | "avi" | "wmv" | "asf" | "flv" | "f4v"
        | "ogv" | "3gp" | "3g2" | "divx" | "xvid"
        // Broadcast, Optical Disc & Transport Streams
        | "m2ts" | "m2t" | "mpeg" | "mpg" | "mpe" | "m1v" | "m2v"
        | "vob" | "evo" | "mxf" | "gxf" | "vvs" | "dvr-ms" | "wtv" | "tod"
        // Editing, Acquisition & Cinema Mezzanine
        | "prores" | "dnxhd" | "dnxhr" | "braw" | "r3d" | "ari" | "arriraw" | "crm"
        | "xavc" | "xavcs" | "dv" | "hdv"
        // Retro, Interactive & Streaming Containers
        | "rm" | "rmvb" | "bik" | "bik2" | "smk" | "roq" | "amv" | "mtv" | "nsv" | "ivf"
        | "y4m" | "yuv" | "dmb" | "pva" | "vqa" | "vmd"
        // 2026+ Next-Gen Video
        | "av2" | "av2s" => Category::Video,

        // Compressed Archives, Disk Images, Software Packages, Game Archives & Forensics
        "zip" | "zipx" | "tar" | "gz" | "gzip" | "bz2" | "xz" | "zst" | "zstd" | "7z"
        | "rar" | "cab" | "arj" | "lha" | "lzh" | "alz" | "egg" | "z" | "lz4" | "br"
        | "brotli" | "cpio" | "ar" | "ace" | "pak" | "wad" | "sqx" | "zoo" | "arc"
        | "iso" | "img" | "dmg" | "vhd" | "vhdx" | "vmdk" | "vdi" | "qcow" | "qcow2"
        | "mds" | "nrg" | "cdi" | "dsk" | "adf" | "chd" | "cso" | "wbfs"
        | "deb" | "rpm" | "apk" | "aab" | "ipa" | "msi" | "msix" | "appx" | "pkg"
        | "flatpak" | "flatpakref" | "snap" | "tgz" | "tbz2" | "txz" | "tzst"
        | "cbr" | "cbz" | "cb7" | "cbt" | "kmz" | "webdataset" | "npz"
        | "vpk" | "pk3" | "pk4" | "bsa" | "ba2" | "mpq" | "big" | "viv" | "pck"
        | "assets" | "unity3d" | "wad2" | "wad3" | "gcf" | "cas" | "npk" | "cpk" | "grp"
        | "ips" | "ups" | "bps" | "xci" | "nsp" | "cia" | "e01" | "l01" | "aff" | "aff4"
        | "dd" | "pmtiles" | "tia" | "ap14" | "ap15" | "ap16" | "ap17" | "ap18" | "ap19"
        | "bag" | "mcap" | "gtfs" | "warc" | "wacz" | "car" => Category::Archive,

        // Native Binaries, Compiled Libraries, Bytecode, ROMs & Hardware Bitstreams
        "exe" | "dll" | "so" | "dylib" | "bin" | "run" | "appimage" | "elf" | "macho"
        | "coff" | "aout" | "com" | "o" | "sys" | "wasm" | "class" | "dex"
        | "odex" | "vdex" | "beam" | "luac" | "cil" | "msil" | "spv" | "dxbc" | "dxil"
        | "smx" | "abc" | "pyc" | "pyo" | "nds" | "gba" | "nes" | "sfc" | "smc"
        | "z64" | "n64" | "v64" | "bit" | "rbf" | "sof" | "pof" | "dfu"
        | "fd" | "me" | "txe" | "dtbo" | "dsdt" | "ssdt" | "aml" | "orom" | "bc" => Category::Executable,

        // Documents, Office, Spreadsheets, Presentations, Fonts, 3D Models, CAD, Bioinformatics, Databases, AI & Forensics
        "pdf" | "pdfa" | "pdfx" | "pdfe" | "djvu" | "djv" | "epub" | "mobi" | "azw" | "azw3"
        | "kf8" | "fb2" | "chm" | "lit" | "dvi" | "rtf" | "doc" | "docx" | "docm" | "dot"
        | "dotx" | "dotm" | "odt" | "ott" | "pages" | "wpd" | "abw" | "sxw" | "stw"
        | "xls" | "xlsx" | "xlsm" | "xlsb" | "xltx" | "xltm" | "ods" | "ots" | "numbers"
        | "dif" | "slk" | "sylk" | "wk1" | "wk3" | "wk4" | "wks" | "qpw" | "wb1" | "wb2"
        | "ppt" | "pptx" | "pptm" | "pps" | "ppsx" | "odp" | "otp" | "keynote"
        | "sxi" | "ttf" | "otf" | "woff" | "woff2" | "eot" | "ttc" | "otc" | "dfont"
        | "pfb" | "pfm" | "afm" | "bdf" | "pcf" | "sfnt" | "fnt" | "fon" | "bmf"
        | "gltf" | "glb" | "usd" | "usda" | "usdc" | "usdz" | "obj" | "fbx" | "dae"
        | "3ds" | "blend" | "c4d" | "max" | "ma" | "mb" | "lwo" | "lws" | "lxo" | "x"
        | "smd" | "dmx" | "md2" | "md3" | "md5" | "step" | "stp" | "iges" | "igs"
        | "stl" | "3mf" | "amf" | "sat" | "sab" | "x_t" | "x_b" | "jt" | "sldprt"
        | "sldasm" | "catpart" | "catproduct" | "ipt" | "iam" | "prt" | "fcstd"
        | "ifc" | "rvt" | "rfa" | "nwd" | "nwc" | "nwf" | "pln" | "bcf" | "pcd"
        | "las" | "e57" | "vdb" | "openvdb" | "sqlite" | "sqlite3" | "db" | "wal"
        | "shm" | "mdb" | "accdb" | "dbf" | "mdf" | "ldf" | "ibd" | "frm" | "fdb"
        | "gdb" | "dat" | "parquet" | "orc" | "feather" | "arrow" | "zarr" | "root"
        | "dta" | "sav" | "por" | "rdata" | "rds" | "npy" | "shp" | "shx" | "gpkg"
        | "dem" | "safetensors" | "gguf" | "ggml" | "onnx" | "pt" | "pth" | "ckpt"
        | "tflite" | "keras" | "mlmodel" | "mlpackage" | "engine" | "plan" | "blob"
        | "lance" | "faiss" | "ann" | "annoy" | "usearch" | "hnsw" | "der" | "crt"
        | "cer" | "p12" | "pfx" | "jks" | "bks" | "crl" | "gpg" | "pgp" | "age"
        | "kdbx" | "keepass" | "pcap" | "pcapng" | "cap" | "evtx" | "etl" | "pprof"
        | "otlp" | "als" | "cpr" | "flp" | "rpp" | "ptx" | "ptf" | "song" | "logicx"
        | "reason" | "cwb" | "sesx" | "fasta" | "fas" | "fa" | "fna" | "faa"
        | "fastq" | "fq" | "sam" | "bam" | "cram" | "vcf" | "vcard" | "gff"
        | "gff3" | "gtf" | "bed" | "wig" | "bigwig" | "bigbed" | "bb" | "pdb" | "cif"
        | "mmcif" | "pdbx" | "genbank" | "gb" | "gbk" | "embl" | "ab1" | "abi" | "sff"
        | "h5ad" | "mgf" | "mzml" | "gds" | "gdsii" | "gds2" | "oasis" | "brd" | "sch"
        | "kicad_pcb" | "kicad_sch" | "kicad_mod" | "ipc2581" | "odb" | "odbpp"
        | "hpgl" | "plt" | "mat" | "nb" | "mw" | "mws" | "sm" | "smath" | "cdf"
        | "grib" | "grib1" | "grib2" | "grb" | "grb2" | "bufr" | "hdfeos" | "rinex" | "mbtiles"
        | "wkb" | "sdts" | "e00" | "bil" | "bip" | "bsq" | "dmp" | "mdmp" | "vmem"
        | "mem" | "vsl" | "regf" | "pf" | "edb" | "ics" | "ical" | "vcs" | "mbox"
        | "eml" | "emlx" | "msg" | "pst" | "ost" | "dbx" | "bvh" | "c3d" | "amc"
        | "trc" | "csm" | "htr" | "mvn" | "mvnx" | "bsp" | "brf" | "brl"
        | "daisy" | "dtb" | "bdx" | "pac"
        | "acd" | "s7p" | "s7s" | "mwp" | "mol" | "inchi" | "inchikey" | "cdx" | "pdbqt"
        | "gro" | "top" | "trr" | "xtc" | "dcd" | "crd" | "prmtop" | "pbrtop" | "chk" | "out"
        | "asdf" | "votable" | "vot" | "spk" | "ik" | "ck" | "fk" | "sclk" | "tsc" | "lsk" | "tls" | "ecsv"
        | "segy" | "sgy" | "segd" | "sgd" | "dlis" | "lis" | "resqml" | "seed" | "mseed" | "miniseed" | "sac" | "win" | "win32"
        | "sigmf" | "sigmf-data" | "ciq" | "iq" | "s1p" | "s2p" | "s3p" | "s4p" | "snp" | "cfile" | "vrt" | "v49" | "ss7" | "gtp" | "diameter"
        | "a424" | "arinc424" | "arinc" | "000" | "s57" | "s63" | "s100" | "s101" | "ais" | "gtfsrt"
        | "icc" | "icm" | "afp" | "afpds" | "pcl" | "pclx" | "pxl" | "escp" | "epson" | "spl" | "shd" | "cups"
        | "pdr" | "dwarf" | "trace" | "hprof" | "sst" | "sstable" | "rdb" | "aof" | "pgdump" | "mrk" | "mrk2" | "mrk3"
        | "opt" | "marc" | "marc21" | "ris" | "enw" | "blk" | "rev" => Category::Document,

        // Code, Markup, Configs, Subtitles, EDA Netlists, Languages, Build Rules & Keys
        "rs" | "zig" | "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "hxx" | "py" | "pyw"
        | "pyi" | "ts" | "tsx" | "js" | "mjs" | "cjs" | "jsx" | "go" | "lua" | "sh"
        | "bash" | "zsh" | "fish" | "ksh" | "slint" | "json" | "jsonc" | "json5"
        | "jsonld" | "toml" | "yaml" | "yml" | "xml" | "ini" | "conf" | "cfg"
        | "properties" | "env" | "plist" | "hcl" | "kdl" | "ron" | "proto" | "pb"
        | "fbs" | "msgpack" | "bson" | "avro" | "cbor" | "asn1" | "asn" | "thrift"
        | "capnp" | "smile" | "ion" | "html" | "htm" | "xhtml" | "css" | "scss"
        | "sass" | "less" | "graphql" | "gql" | "wsdl" | "rss" | "atom" | "sql"
        | "geojson" | "kml" | "gpx" | "gml" | "osm" | "txt" | "md" | "markdown"
        | "adoc" | "asciidoc" | "org" | "rst" | "tex" | "latex" | "nfo" | "diz"
        | "log" | "pod" | "csv" | "tsv" | "tab" | "scad" | "gcode" | "nc" | "xyz"
        | "pts" | "cue" | "ebuild" | "wat" | "mtl" | "pem" | "key" | "csr" | "pub"
        | "asc" | "har" | "syslog" | "mlkem" | "kem" | "mldsa" | "slhdsa" | "wit"
        | "c2pa" | "srt" | "vtt" | "webvtt" | "ass" | "ssa" | "sub" | "idx" | "smi"
        | "sami" | "ttml" | "dfxp" | "scc" | "mcc" | "usf" | "sbv" | "lrc" | "lef"
        | "def" | "edif" | "spice" | "cir" | "sdc" | "sdf" | "drl" | "tap" | "ipynb"
        | "m" | "wl" | "wls" | "mps" | "lp" | "ampl" | "gms" | "sbml" | "cellml"
        | "nmea" | "wkt" | "ldif" | "ofx" | "qfx" | "qif" | "camt" | "mt940"
        | "mt942" | "bai2" | "xbrl" | "ixbrl" | "fix" | "hex" | "srec" | "s19"
        | "s28" | "s37" | "jed" | "jedec" | "rb" | "php" | "cs" | "vb" | "vbs"
        | "fs" | "fsi" | "java" | "kt" | "kts" | "swift" | "scala" | "clj" | "cljs"
        | "edn" | "hs" | "lhs" | "ml" | "mli" | "elm" | "ex" | "exs" | "erl"
        | "hrl" | "lisp" | "lsp" | "scm" | "ss" | "r" | "jl" | "pl" | "pm" | "awk"
        | "sed" | "tcl" | "asm" | "s" | "f" | "for" | "f90" | "f95" | "f03"
        | "cob" | "cbl" | "ada" | "adb" | "ads" | "pas" | "pp" | "d" | "nim"
        | "v" | "odin" | "dart" | "verilog" | "sv" | "svh" | "vhdl"
        | "makefile" | "mk" | "cmake" | "nix" | "guix" | "bazel" | "buck" | "tf"
        | "tfvars" | "lock"
        | "plcopen" | "l5x" | "pro" | "project" | "urdf" | "xacro" | "nodeset" | "nodeset2" | "mtconnect" | "gsd" | "gsdml" | "eds" | "xdd"
        | "smiles" | "cml" | "cdxml" | "itp" | "inp" | "nw" | "poscar" | "potcar" | "incar" | "kpoints"
        | "witsml" | "prodml" | "sigmf-meta"
        | "stix" | "stix2" | "taxii" | "sarif" | "spdx" | "cyclonedx" | "csaf" | "cvrf" | "yara" | "yar" | "sigma" | "rules" | "misp" | "kape" | "tkape" | "gkape"
        | "aixm" | "fixm" | "wxxm"
        | "ppd" | "jdf" | "jmf" | "dtsi" | "dsl" | "asl"
        | "ll" | "gimple" | "speedscope" | "ahap"
        | "edrm" | "bib" | "bibtex" | "mets" | "premis"
        | "sol" | "vy" => Category::Code,
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
    let name_lower = e.name.to_ascii_lowercase();
    match name_lower.as_str() {
        "makefile" => return "Makefile build rules".into(),
        "dockerfile" => return "Dockerfile container build specification".into(),
        "vagrantfile" => return "Vagrantfile virtual environment spec".into(),
        "justfile" => return "Justfile command runner script".into(),
        "gemfile" => return "Ruby Gemfile dependency manifest".into(),
        "poscar" => return "VASP POSCAR structure geometry".into(),
        "potcar" => return "VASP POTCAR pseudopotential library".into(),
        "incar" => return "VASP INCAR input parameters".into(),
        "kpoints" => return "VASP KPOINTS Brillouin zone sampling".into(),
        "perf.data" => return "Linux perf event execution recording".into(),
        "manifest" => return "LSM-Tree version manifest log".into(),
        "core" => return "POSIX process crash core dump".into(),
        _ => {}
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
        "cdr" => "CorelDRAW vector drawing / Telecom Call Detail Record (CDR)",
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
        "mrsid" => "MrSID geospatial image",
        "ecw" => "ECW wavelet image",
        "h5" | "hdf5" | "hdf" | "netcdf" => "Scientific dataset",
        "svs" => "Aperio pathology image",
        "ndpi" => "Hamamatsu pathology image",
        "ometiff" => "OME-TIFF microscopy image",
        "dm3" | "dm4" => "Gatan microscopy image",
        "mrc" => "MRC cryo-EM map / MARC21 record",
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
        "m2ts" | "m2t" => "MPEG transport stream",
        "mpeg" | "mpg" | "mpe" | "m1v" | "m2v" => "MPEG program stream",
        "vob" => "DVD Video Object",
        "evo" => "HD DVD Video Object",
        "mxf" => "Material Exchange Format",
        "gxf" => "General Exchange Format",
        "vvs" => "Variable Video Stream",
        "dvr-ms" | "wtv" => "Recorded TV show",
        "tod" => "Camcorder video stream",

        // Editing, Acquisition & Cinema Mezzanine
        "prores" => "Apple ProRes video",
        "dnxhd" | "dnxhr" => "Avid DNxHD video",
        "braw" => "Blackmagic RAW video",
        "r3d" => "REDCODE RAW video",
        "ari" | "arriraw" => "ARRIRAW video",
        "crm" => "Canon Cinema RAW Light",
        "xavc" | "xavcs" => "Sony XAVC video",
        "dv" => "Digital Video stream",
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

        // PART 1: Audio Formats, Codecs, Trackers & DAW Sessions
        "flac" => "FLAC lossless audio",
        "alac" => "Apple Lossless audio",
        "wav" => "Waveform audio (WAV)",
        "aiff" | "aif" | "aifc" => "Audio Interchange format (AIFF)",
        "ape" => "Monkey's Audio",
        "wv" => "WavPack lossless audio",
        "tta" => "True Audio lossless",
        "shn" => "Shorten lossless audio",
        "tak" => "Tom's Lossless Audio (TAK)",
        "ofr" | "ofs" => "OptimFROG lossless audio",
        "dsd" | "dsf" | "dff" => "Direct Stream Digital (DSD)",
        "dxd" => "Digital eXtreme Definition",
        "mlp" => "Meridian Lossless Packing",
        "wma" => "Windows Media Audio",
        "bwf" => "Broadcast Wave audio",
        "rf64" => "RIFF 64-bit audio",
        "caf" => "Apple Core Audio",
        "au" | "snd" => "Sun / NeXT audio",
        "w64" => "Sound Foundry Wave64",
        "mp3" => "MP3 audio",
        "aac" => "Advanced Audio Coding (AAC)",
        "m4a" => "MPEG-4 audio (M4A)",
        "opus" => "Opus interactive audio",
        "ogg" | "oga" => "Ogg Vorbis audio",
        "ac3" | "a52" => "Dolby Digital AC-3",
        "eac3" => "Dolby Digital Plus (E-AC-3)",
        "truehd" => "Dolby TrueHD audio",
        "dts" => "DTS audio / Device Tree Source",
        "dtshd" => "DTS-HD Master Audio",
        "dtsx" => "DTS:X spatial audio",
        "oma" | "omg" | "aa3" => "Sony ATRAC audio",
        "mpc" | "mpp" | "mp+" => "Musepack audio",
        "spx" => "Speex speech audio",
        "amr" => "AMR narrowband audio",
        "awb" => "AMR wideband audio",
        "gsm" => "GSM 06.10 speech audio",
        "g711" | "alaw" | "ulaw" => "G.711 PCM telephony audio",
        "g722" | "g726" | "g729" => "ITU-T telecommunication audio",
        "ilbc" => "internet Low Bitrate Codec (iLBC)",
        "silk" => "Skype SILK speech audio",
        "qdm" | "qdm2" => "QDesign Music Codec (QDMC)",
        "vselp" => "VSELP speech audio",
        "voc" => "Creative Voice audio",
        "qcp" => "Qualcomm PureVoice audio",
        "mid" | "midi" | "smf" => "Standard MIDI sequence",
        "rmi" | "rmid" => "RIFF MIDI sequence",
        "kar" => "Karaoke MIDI sequence",
        "mod" => "ProTracker / Soundtracker module",
        "xm" => "FastTracker 2 extended module",
        "s3m" => "Scream Tracker 3 module",
        "it" => "Impulse Tracker module",
        "mtm" => "MultiTracker module",
        "umx" => "Unreal audio package",
        "669" => "Composer 669 module",
        "far" => "Farandole Composer module",
        "okt" => "Oktalyzer Amiga tracker",
        "sid" => "Commodore 64 SID chiptune",
        "nsf" | "nsfe" => "NES Sound Format",
        "spc" => "SNES SPC700 sound format",
        "vgm" | "vgz" => "Video Game Music log",
        "gym" => "Sega Genesis YM2612 audio",
        "hes" => "PC Engine HES audio",
        "gbs" => "Game Boy Sound audio",
        "kss" => "MSX sound chip dump",
        "ay" => "ZX Spectrum AY sound",
        "sap" => "Atari POKEY sound",
        "als" => "Ableton Live set",
        "cpr" => "Steinberg Cubase project",
        "flp" => "FL Studio project",
        "rpp" => "REAPER project",
        "ptx" | "ptf" => "Pro Tools session",
        "song" => "Studio One song",
        "logicx" => "Apple Logic Pro project",
        "reason" => "Reason Studios project",
        "cwb" => "Cakewalk Sonar bundle",
        "sesx" => "Adobe Audition multitrack session",
        "sf2" | "sf3" => "SoundFont sample bank",
        "sfz" => "SFZ multisample instrument",
        "nki" => "Kontakt instrument",
        "gig" => "GigaStudio sample bank",

        // PART 2: Documents, Office, Text, Layout & Typography
        "txt" => "Plain text document",
        "md" | "markdown" => "Markdown document",
        "adoc" | "asciidoc" => "AsciiDoc document",
        "org" => "Emacs Org-mode document",
        "rst" => "reStructuredText document",
        "tex" | "latex" => "LaTeX document source",
        "rtf" => "Rich Text Format (RTF)",
        "nfo" => "NFO release metadata",
        "diz" => "Description in Zip (DIZ)",
        "log" => "Application / system log",
        "pod" => "Perl Plain Old Documentation",
        "pdf" => "PDF document",
        "pdfa" => "PDF/A archival document",
        "pdfx" => "PDF/X prepress document",
        "pdfe" => "PDF/E engineering document",
        "xps" => "XML Paper Specification",
        "oxps" => "Open XML Paper Specification",
        "djvu" | "djv" => "DjVu scanned document",
        "epub" => "EPUB electronic book",
        "mobi" => "Mobipocket eBook",
        "azw" | "azw3" | "kf8" => "Amazon Kindle eBook",
        "fb2" => "FictionBook eBook",
        "cbr" => "Comic book archive (RAR)",
        "cbz" => "Comic book archive (ZIP)",
        "cb7" => "Comic book archive (7z)",
        "cbt" => "Comic book archive (TAR)",
        "chm" => "Compiled HTML Help (CHM)",
        "lit" => "Microsoft Reader eBook",
        "dvi" => "TeX Device Independent format",
        "xlsx" => "Excel spreadsheet",
        "xls" => "Excel 97-2003 spreadsheet",
        "xlsm" => "Excel macro spreadsheet",
        "xlsb" => "Excel binary spreadsheet",
        "xltx" | "xltm" => "Excel template",
        "ods" => "OpenDocument spreadsheet",
        "ots" => "OpenDocument spreadsheet template",
        "numbers" => "Apple Numbers spreadsheet",
        "csv" => "Comma-Separated Values (CSV)",
        "tsv" | "tab" => "Tab-Separated Values (TSV)",
        "dif" => "Data Interchange Format",
        "slk" | "sylk" => "Symbolic Link spreadsheet",
        "wk1" | "wk3" | "wk4" | "wks" => "Lotus 1-2-3 spreadsheet",
        "qpw" | "wb1" | "wb2" => "Quattro Pro spreadsheet",
        "docx" => "Word document",
        "doc" => "Word 97-2003 document",
        "docm" => "Word macro document",
        "dot" | "dotx" | "dotm" => "Word document template",
        "odt" => "OpenDocument text",
        "ott" => "OpenDocument text template",
        "pages" => "Apple Pages document",
        "wpd" => "WordPerfect document",
        "abw" => "AbiWord document",
        "sxw" | "stw" => "OpenOffice text document",
        "pptx" => "PowerPoint presentation",
        "ppt" => "PowerPoint 97-2003 presentation",
        "pptm" => "PowerPoint macro presentation",
        "pps" | "ppsx" => "PowerPoint slide show",
        "odp" | "otp" => "OpenDocument presentation",
        "keynote" => "Apple Keynote presentation",
        "sxi" => "OpenOffice presentation",
        "ttf" => "TrueType font",
        "otf" => "OpenType font",
        "woff" => "Web Open Font 1.0 (WOFF)",
        "woff2" => "Web Open Font 2.0 (WOFF2)",
        "eot" => "Embedded OpenType font",
        "ttc" => "TrueType font collection",
        "otc" => "OpenType font collection",
        "dfont" => "macOS Data Fork font",
        "pfb" => "PostScript Type 1 binary font",
        "pfm" => "Printer Font Metrics",
        "afm" => "Adobe Font Metrics",
        "bdf" => "Bitmap Distribution Format font",
        "pcf" => "Portable Compiled Font (X11)",
        "sfnt" => "Spline font format",
        "fnt" | "fon" => "Windows bitmap font",
        "bmf" => "ByteMap font",

        // PART 3: 3D Models, CAD, CAM, BIM & Spatial Data
        "gltf" => "GL Transmission Format (GLTF)",
        "glb" => "Binary GLTF 3D model",
        "usd" | "usda" | "usdc" => "Universal Scene Description (USD)",
        "usdz" => "USDZ packaged 3D asset",
        "obj" => "Wavefront 3D object",
        "mtl" => "Material Template Library",
        "fbx" => "Autodesk Filmbox 3D asset",
        "dae" => "COLLADA 3D asset",
        "3ds" => "Autodesk 3D Studio mesh",
        "blend" => "Blender 3D project",
        "c4d" => "Cinema 4D project",
        "max" => "Autodesk 3ds Max scene",
        "ma" | "mb" => "Autodesk Maya scene",
        "lwo" | "lws" => "LightWave 3D model / scene",
        "lxo" => "Foundry Modo 3D asset",
        "x" => "DirectX 3D mesh",
        "smd" | "dmx" => "Source Engine model data",
        "md2" | "md3" | "md5" => "id Software Quake mesh",
        "step" | "stp" => "STEP 3D CAD model",
        "iges" | "igs" => "IGES CAD model",
        "stl" => "Stereolithography 3D mesh",
        "3mf" => "3D Manufacturing Format",
        "amf" => "Additive Manufacturing file",
        "sat" | "sab" => "ACIS solid model",
        "x_t" | "x_b" => "Parasolid model part",
        "jt" => "Jupiter Tessellation CAD model",
        "sldprt" | "sldasm" => "SolidWorks part / assembly",
        "catpart" | "catproduct" => "CATIA part / assembly",
        "ipt" | "iam" => "Autodesk Inventor part / assembly",
        "prt" => "PTC Creo parametric model",
        "fcstd" => "FreeCAD project",
        "scad" => "OpenSCAD geometry script",
        "gcode" | "nc" => "CNC / 3D printer G-code",
        "ifc" => "Industry Foundation Classes (BIM)",
        "rvt" => "Autodesk Revit project",
        "rfa" => "Autodesk Revit family",
        "nwd" | "nwc" | "nwf" => "Autodesk Navisworks review",
        "pln" => "Graphisoft ArchiCAD project",
        "bcf" => "BIM Collaboration / Binary Variant Call Format (BCF)",
        "pcd" => "Point Cloud Data (PCL)",
        "las" => "ASPRS LiDAR point cloud / Well Log ASCII (LAS)",
        "laz" => "Lossless LASzip LiDAR data",
        "e57" => "ASTM E2807 3D point cloud",
        "xyz" | "pts" => "ASCII coordinate point cloud",
        "vdb" | "openvdb" => "OpenVDB sparse volume",

        // PART 4: Archives, Compression, Packages & Virtual Disks
        "zip" | "zipx" => "ZIP compressed archive",
        "tar" => "Tape Archive (TAR)",
        "gz" | "gzip" => "Gzip compressed archive",
        "bz2" => "Bzip2 compressed archive",
        "xz" => "XZ compressed archive",
        "zst" | "zstd" => "Zstandard compressed archive",
        "7z" => "7-Zip compressed archive",
        "rar" => "RAR compressed archive",
        "cab" => "Microsoft Cabinet archive",
        "arj" => "ARJ compressed archive",
        "lha" | "lzh" => "LHA / LZH compressed archive",
        "alz" | "egg" => "ESTsoft compressed archive",
        "z" => "Unix compress LZW archive",
        "lz4" => "LZ4 compressed frame",
        "br" | "brotli" => "Brotli compressed archive",
        "cpio" => "CPIO archive",
        "ar" => "Unix archive library",
        "ace" => "WinACE compressed archive",
        "pak" => "Video game asset package",
        "wad" => "Doom WAD resource package",
        "sqx" => "Sequential Quick X-archive",
        "zoo" => "Zoo compressed archive",
        "arc" => "SEA ARC compressed archive",
        "iso" => "Optical disc ISO image",
        "img" => "Raw disk image",
        "dmg" => "Apple Disk Image",
        "vhd" => "Virtual Hard Disk / VHDL source",
        "vhdx" => "Virtual Hard Disk v2",
        "vmdk" => "VMware Virtual Disk",
        "vdi" => "VirtualBox Disk Image",
        "qcow" | "qcow2" => "QEMU Copy-On-Write disk",
        "cue" => "CUE sheet track metadata",
        "mds" => "Alcohol 120% disc image",
        "nrg" => "Nero Burning ROM disc image",
        "cdi" => "DiscJuggler disc image",
        "dsk" => "Generic floppy disk image",
        "adf" => "Amiga Disk File image",
        "nds" => "Nintendo DS ROM",
        "gba" => "Game Boy Advance ROM",
        "nes" => "NES ROM image",
        "sfc" | "smc" => "Super Nintendo ROM",
        "z64" | "n64" | "v64" => "Nintendo 64 ROM",
        "chd" => "Compressed Hunks of Data (MAME)",
        "cso" => "Compressed ISO disc image",
        "wbfs" => "Wii Backup File System image",
        "deb" => "Debian software package",
        "rpm" => "Red Hat package",
        "apk" => "Android / Alpine package",
        "aab" => "Android App Bundle",
        "ipa" => "iOS App Store package",
        "msi" => "Windows Installer package",
        "msix" | "appx" => "Windows App package",
        "pkg" => "macOS / BSD package",
        "flatpak" | "flatpakref" => "Flatpak package",
        "appimage" => "AppImage portable executable",
        "snap" => "Ubuntu Snap package",
        "ebuild" => "Gentoo Portage ebuild",

        // PART 5: Executables, Compiled Objects, Binaries & Runtimes
        "elf" => "ELF executable / binary",
        "exe" => "Windows executable (PE)",
        "dll" => "Dynamic Link Library (DLL)",
        "sys" => "Windows device driver",
        "macho" => "Mach-O binary",
        "coff" => "Common Object File (COFF)",
        "aout" => "Assembler output (a.out)",
        "com" => "DOS COM executable",
        "o" => "Compiled object file",
        "so" => "Shared Object library",
        "dylib" => "Mach-O dynamic library",
        "a" | "lib" => "Static library archive",
        "wasm" => "WebAssembly module",
        "wat" => "WebAssembly text format",
        "class" => "Java compiled bytecode",
        "jar" => "Java Archive",
        "war" | "ear" => "Java Enterprise archive",
        "pyc" | "pyo" => "Python compiled bytecode",
        "dex" | "odex" | "vdex" => "Dalvik executable bytecode",
        "beam" => "Erlang BEAM bytecode",
        "luac" => "Lua compiled bytecode",
        "cil" | "msil" => ".NET Common Intermediate Language",
        "spv" => "SPIR-V shader module",
        "dxbc" | "dxil" => "DirectX shader bytecode",
        "smx" => "SourcePawn compiled plugin",
        "abc" => "ActionScript Byte Code",

        // PART 6: Data Serialization, Configuration & Protocols
        "json" => "JSON data document",
        "jsonc" => "JSON with comments",
        "json5" => "JSON5 document",
        "jsonld" => "JSON-LD linked data",
        "yaml" | "yml" => "YAML data document",
        "toml" => "TOML configuration",
        "xml" => "XML document",
        "ini" => "INI configuration",
        "properties" => "Java properties file",
        "conf" | "cfg" => "Application configuration",
        "env" => "Environment variables file",
        "plist" => "Apple Property List",
        "hcl" => "HashiCorp Configuration (HCL)",
        "kdl" => "KDL document language",
        "ron" => "Rusty Object Notation (RON)",
        "proto" | "pb" => "Protocol Buffers schema / message",
        "fbs" => "FlatBuffers schema / binary",
        "msgpack" => "MessagePack binary object",
        "bson" => "Binary JSON (BSON)",
        "avro" => "Apache Avro data serialization",
        "cbor" => "CBOR binary object",
        "asn1" | "asn" => "ASN.1 schema / payload",
        "thrift" => "Apache Thrift RPC payload",
        "capnp" => "Cap'n Proto serialization",
        "smile" => "Jackson Smile binary JSON",
        "ion" => "Amazon Ion serialization",
        "html" | "htm" => "HTML document",
        "xhtml" => "XHTML document",
        "css" => "CSS stylesheet",
        "scss" | "sass" => "SASS / SCSS stylesheet",
        "less" => "LESS stylesheet",
        "js" | "mjs" | "cjs" => "JavaScript source",
        "ts" | "mts" | "cts" => "TypeScript source",
        "tsx" | "jsx" => "React JSX / TSX source",
        "graphql" | "gql" => "GraphQL schema / query",
        "wsdl" => "Web Services Description (WSDL)",
        "rss" => "RSS web feed",
        "atom" => "Atom syndication feed",

        // PART 7: Databases, Big Data, Storage Engines & GIS
        "sql" => "SQL database script",
        "sqlite" | "sqlite3" | "db" => "SQLite database",
        "wal" | "shm" => "SQLite auxiliary log / shared memory",
        "mdb" | "accdb" => "Microsoft Access database",
        "dbf" => "dBase table database",
        "mdf" | "ldf" => "SQL Server database / log",
        "ibd" | "frm" => "MySQL / MariaDB tablespace",
        "fdb" => "Firebird database",
        "gdb" => "InterBase database",
        "dat" => "Data record file",
        "parquet" => "Apache Parquet columnar data",
        "orc" => "Optimized Row Columnar (ORC) data",
        "feather" => "Apache Arrow Feather storage",
        "arrow" => "Apache Arrow IPC data",
        "zarr" => "Zarr chunked array storage",
        "root" => "CERN ROOT data file",
        "dta" => "Stata statistical dataset",
        "sav" | "por" => "SPSS statistical data",
        "rdata" | "rds" => "R serialized dataset",
        "npy" => "NumPy N-dimensional array",
        "npz" => "NumPy zipped array archive",
        "geojson" => "GeoJSON spatial data",
        "kml" => "Keyhole Markup Language (KML)",
        "kmz" => "Keyhole Markup Language (KMZ)",
        "shp" => "ESRI Shapefile geometry",
        "shx" => "ESRI Shapefile spatial index",
        "gpx" => "GPS Exchange Format (GPX)",
        "gpkg" => "OGC GeoPackage spatial database",
        "gml" => "Geography Markup Language",
        "osm" => "OpenStreetMap XML data",
        "dem" => "Digital Elevation Model",

        // PART 8: Machine Learning, AI Weights & Computational Tensors
        "safetensors" => "Safetensors neural model weights",
        "gguf" => "GGUF quantized model weights",
        "ggml" => "GGML tensor weights",
        "onnx" => "ONNX neural network model",
        "pt" | "pth" => "PyTorch model weights / state",
        "ckpt" => "Model training checkpoint",
        "tflite" => "TensorFlow Lite model",
        "keras" => "Keras neural network model",
        "mlmodel" | "mlpackage" => "Apple Core ML model",
        "engine" | "plan" => "TensorRT execution plan",
        "blob" => "MyriadX neural blob",
        "lance" => "Lance columnar vector data",
        "faiss" => "FAISS vector similarity index",
        "ann" | "annoy" => "Spotify Annoy vector index",
        "usearch" => "USearch vector index",
        "hnsw" => "HNSW graph vector index",
        "webdataset" => "WebDataset training container",

        // PART 9: Cryptography, Security, Identity & Telemetry
        "pem" => "PEM cryptographic certificate / key",
        "der" => "DER encoded binary certificate",
        "crt" | "cer" => "X.509 digital certificate",
        "key" => "Cryptographic private key",
        "p12" | "pfx" => "PKCS #12 personal identity exchange",
        "jks" => "Java KeyStore",
        "bks" => "Bouncy Castle KeyStore",
        "csr" => "Certificate Signing Request (CSR)",
        "crl" => "Certificate Revocation List (CRL)",
        "pub" => "Cryptographic public key",
        "asc" => "PGP ASCII armored key / signature",
        "gpg" | "pgp" => "GnuPG / PGP encrypted payload",
        "age" => "age encrypted file",
        "kdbx" | "keepass" => "KeePass password database",
        "pcap" => "libpcap packet capture",
        "pcapng" => "PCAP Next Generation capture",
        "cap" => "Packet capture / UEFI Capsule Update / Subtitle caption",
        "har" => "HTTP Archive (HAR) log",
        "evtx" => "Windows Event Log",
        "syslog" => "Syslog telemetry stream",
        "etl" => "Windows Event Trace Log",
        "pprof" => "Google pprof performance profile",
        "otlp" => "OpenTelemetry telemetry payload",

        // PART 10: 2026 Active Standards & Future Non-Image/Video File Formats
        "mlkem" | "kem" => "FIPS 203 ML-KEM post-quantum key",
        "mldsa" | "slhdsa" => "FIPS 204/205 post-quantum signature/key",
        "wit" => "WebAssembly Interface Type (WIT)",
        "c2pa" => "C2PA provenance manifest assertion",

        // SUPPLEMENTARY PART 1: Subtitles, Timed Text, Closed Captions & Lyrics
        "srt" => "SubRip subtitle",
        "vtt" | "webvtt" => "WebVTT subtitle",
        "ass" => "Advanced SubStation Alpha subtitle",
        "ssa" => "SubStation Alpha subtitle",
        "sub" => "MicroDVD / SubViewer subtitle",
        "idx" => "VobSub subtitle index",
        "smi" => "SAMI synchronized caption / SMILES chemical notation",
        "sami" => "SAMI synchronized caption",
        "ttml" | "dfxp" => "Timed Text Markup subtitle",
        "scc" => "Scenarist Closed Caption",
        "mcc" => "MacCaption Closed Caption",
        "usf" => "Universal Subtitle Format",
        "sbv" => "YouTube subtitle",
        "lrc" => "Lyrics file (LRC)",
        "pac" => "Screen Electronics PAC subtitle",

        // SUPPLEMENTARY PART 2: Bioinformatics, Genomics, Proteomics & Structural Biology
        "fasta" | "fas" | "fa" | "fna" | "faa" => "FASTA biological sequence",
        "fastq" | "fq" => "FASTQ biological sequence with quality",
        "sam" => "Sequence Alignment/Map (SAM)",
        "bam" => "Binary Alignment/Map (BAM)",
        "cram" => "CRAM compressed alignment container",
        "vcf" | "vcard" => "vCard / Variant Call Format (VCF)",
        "gff" | "gff3" => "Generic Feature Format (GFF3)",
        "gtf" => "Gene Transfer Format (GTF)",
        "bed" => "Browser Extensible Data (BED)",
        "wig" => "Wiggle genomic graphing data",
        "bigwig" => "BigWig indexed genomic data",
        "bigbed" | "bb" => "BigBed indexed genomic intervals",
        "pdb" => "Protein Data Bank / Microsoft Program Database (PDB)",
        "cif" | "mmcif" | "pdbx" => "Macromolecular CIF structure",
        "genbank" | "gb" | "gbk" => "GenBank nucleotide sequence",
        "embl" => "EMBL nucleotide sequence",
        "ab1" | "abi" => "Applied Biosystems DNA chromatogram / Contract ABI",
        "sff" => "Standard Flowgram Format read",
        "h5ad" => "AnnData single-cell genomics matrix",
        "mgf" => "Mascot Generic Format peak list",
        "mzml" => "mzML proteomics mass spectrometry",

        // SUPPLEMENTARY PART 3: Electronic Design Automation (EDA), Semiconductors & PCB Fabrication
        "gds" | "gdsii" | "gds2" => "GDSII IC layout stream",
        "oasis" => "OASIS IC layout stream",
        "lef" => "Library Exchange Format (LEF)",
        "def" => "Design Exchange Format (DEF)",
        "edif" => "Electronic Design Interchange Format",
        "spice" | "cir" => "SPICE circuit netlist",
        "sdc" => "Synopsys Design Constraints (SDC)",
        "sdf" => "Simulation Description / Structure-Data / Standard Delay (SDF)",
        "brd" => "EAGLE / Allegro PCB layout",
        "sch" => "Schematic capture file",
        "kicad_pcb" => "KiCad PCB layout",
        "kicad_sch" => "KiCad schematic layout",
        "kicad_mod" => "KiCad footprint module",
        "ipc2581" => "IPC-2581 digital PCB exchange",
        "odb" | "odbpp" => "ODB++ PCB manufacturing database",
        "drl" | "tap" => "Excellon NC drill instructions",
        "hpgl" | "plt" => "HPGL plotter vector data",

        // SUPPLEMENTARY PART 4: Mathematical Modeling, Symbolic Computation & Notebooks
        "ipynb" => "Jupyter Notebook",
        "mat" => "MATLAB binary workspace array",
        "m" => "MATLAB / Octave script",
        "nb" => "Mathematica notebook",
        "wl" | "wls" => "Wolfram Language script",
        "mw" | "mws" => "Maple worksheet",
        "sm" | "smath" => "SMath Studio worksheet",
        "mps" => "Mathematical Programming System format",
        "lp" => "CPLEX Linear Programming problem",
        "ampl" => "AMPL mathematical model",
        "gms" => "GAMS algebraic model",
        "cdf" => "NASA Common Data Format",
        "sbml" => "Systems Biology Markup Language",
        "cellml" => "CellML biological model",

        // SUPPLEMENTARY PART 5: Meteorology, Earth Observation, Oceanography & Remote Sensing
        "grib" | "grib1" | "grib2" | "grb" | "grb2" => "GRIB meteorological data",
        "bufr" => "WMO BUFR meteorological data",
        "hdfeos" => "HDF-EOS Earth Observing System",
        "rinex" => "RINEX GNSS satellite observables",
        "nmea" => "NMEA-0183 GPS telemetry log",
        "pmtiles" => "PMTiles cloud-optimized map tiles",
        "mbtiles" => "MBTiles Mapbox tileset database",
        "cog" => "Cloud Optimized GeoTIFF",
        "wkt" => "Well-Known Text geometry (WKT)",
        "wkb" => "Well-Known Binary geometry (WKB)",
        "sdts" => "Spatial Data Transfer Standard",
        "e00" => "ESRI ArcInfo coverage interchange",
        "bil" | "bsq" => "Raw multi-band raster data",

        // SUPPLEMENTARY PART 6: Digital Forensics, Incident Response & Memory Dumps
        "e01" | "l01" => "Expert Witness forensic image (E01)",
        "aff" | "aff4" => "Advanced Forensic Format image",
        "dd" => "Raw disk bitstream dump (dd)",
        "dmp" | "mdmp" => "Windows memory dump / minidump",
        "vmem" => "VMware guest memory snapshot",
        "mem" => "Raw physical memory dump",
        "vsl" => "Volatility framework symbol table",
        "regf" => "Windows registry hive binary",
        "pf" => "Windows Prefetch cache",
        "edb" => "Extensible Storage Engine database",

        // SUPPLEMENTARY PART 7: Personal Information, Contacts, Calendaring & Mail
        "ics" | "ical" => "iCalendar scheduling event",
        "vcs" => "vCalendar event",
        "mbox" => "MBOX mailbox archive",
        "eml" => "RFC 822 email message",
        "emlx" => "Apple Mail email message",
        "msg" => "Outlook mail message",
        "pst" => "Outlook personal storage table (PST)",
        "ost" => "Outlook offline storage table (OST)",
        "dbx" => "Outlook Express email folder",
        "ldif" => "LDAP Data Interchange Format",
        "ofx" => "Open Financial Exchange (OFX)",
        "qfx" => "Quicken Financial Exchange (QFX)",
        "qif" => "Quicken Interchange Format (QIF)",
        "camt" => "ISO 20022 XML bank statement",
        "mt940" | "mt942" => "SWIFT MT940 financial statement",
        "bai2" => "BAI2 cash management report",
        "xbrl" | "ixbrl" => "eXtensible Business Reporting (XBRL)",
        "fix" => "Financial Information eXchange (FIX)",

        // SUPPLEMENTARY PART 8: Motion Capture, Biomechanics & Skeletal Rigging
        "bvh" => "Biovision Hierarchy motion capture (BVH)",
        "c3d" => "Coordinate 3D biomechanics data",
        "amc" => "Acclaim motion capture data",
        "trc" => "Motion Analysis marker coordinate path",
        "csm" => "Character Studio Marker motion capture",
        "bip" => "Character Studio Biped animation / BIP raster",
        "htr" => "Hierarchical Translation-Rotation motion",
        "mvn" | "mvnx" => "Xsens inertial motion capture data",

        // SUPPLEMENTARY PART 9: Game Engine Asset Archives, Level Geometry & Packages
        "bsp" => "Binary Space Partitioning level geometry",
        "vpk" => "Valve Pack filesystem archive (VPK)",
        "pk3" | "pk4" => "Quake / id Tech game package",
        "bsa" | "ba2" => "Bethesda Softworks Archive",
        "mpq" => "MoPaQ Blizzard game data archive",
        "big" | "viv" => "EA game data archive",
        "pck" => "Godot Engine package / SPICE Planetary Constants Kernel",
        "assets" | "unity3d" => "Unity asset package",
        "wad2" | "wad3" => "Half-Life WAD texture package",
        "gcf" => "Steam Grid Cache File",
        "cas" => "Frostbite engine data chunk",
        "npk" => "NetEase game resource package",
        "cpk" => "CRI Middleware package",
        "grp" => "Build Engine graphic archive",

        // SUPPLEMENTARY PART 10: Firmware, PLD/FPGA Bitstreams & Hardware Emulation
        "hex" => "Intel HEX ASCII firmware record",
        "srec" | "s19" | "s28" | "s37" => "Motorola S-Record firmware",
        "bit" => "Xilinx / AMD FPGA bitstream",
        "rbf" => "Altera / Intel FPGA Raw Binary File",
        "sof" => "Altera / Intel SRAM Object File",
        "pof" => "Altera / Intel Programmer Object File",
        "jed" | "jedec" => "JEDEC PLD programming matrix",
        "dfu" => "Device Firmware Upgrade payload",
        "ips" => "International Patching System ROM patch",
        "ups" => "Universal Patching System ROM patch",
        "bps" => "Beat's Patching System ROM patch",
        "xci" => "Nintendo Switch optical card dump",
        "nsp" => "Nintendo Submission Package",
        "cia" => "Nintendo 3DS CTR Importable Archive",

        // SUPPLEMENTARY PART 11: Accessibility, Braille & Tactile Graphics
        "brf" => "Braille Ready Format (BRF)",
        "brl" => "Braille plain stream (BRL)",
        "daisy" | "dtb" => "DAISY digital book / Device Tree Blob (DTB)",
        "bdx" => "Braille Dot eXchange diagram",

        // SUPPLEMENTARY PART 12: Source Code, Hardware Description & Domain Languages
        "rs" => "Rust source",
        "zig" => "Zig source",
        "c" => "C source",
        "h" => "C header",
        "cpp" | "hpp" | "cc" | "cxx" | "hxx" => "C++ source",
        "py" | "pyw" | "pyi" => "Python source",
        "go" => "Go source",
        "rb" => "Ruby source",
        "php" => "PHP script",
        "cs" => "C# source",
        "vb" | "vbs" => "Visual Basic script",
        "fs" | "fsi" => "F# source",
        "java" => "Java source",
        "kt" | "kts" => "Kotlin source",
        "swift" => "Swift source",
        "scala" => "Scala source",
        "clj" | "cljs" | "edn" => "Clojure source",
        "hs" | "lhs" => "Haskell source",
        "ml" | "mli" => "OCaml source",
        "elm" => "Elm source",
        "ex" | "exs" => "Elixir source",
        "erl" | "hrl" => "Erlang source",
        "lisp" | "lsp" => "Common Lisp source",
        "scm" | "ss" => "Scheme source",
        "r" => "R statistical source",
        "jl" => "Julia source",
        "lua" => "Lua script",
        "pl" | "pm" => "Perl script / module",
        "sh" | "bash" | "zsh" | "ksh" | "fish" => "Shell script",
        "awk" => "AWK script",
        "sed" => "SED script",
        "tcl" => "Tcl script",
        "asm" | "s" => "Assembly source",
        "f" | "for" | "f90" | "f95" | "f03" => "Fortran source",
        "cob" | "cbl" => "COBOL source",
        "ada" | "adb" | "ads" => "Ada source",
        "pas" | "pp" => "Pascal source",
        "d" => "D source",
        "nim" => "Nim source",
        "v" => "Verilog / V source",
        "odin" => "Odin source",
        "dart" => "Dart source",
        "verilog" => "Verilog HDL source",
        "sv" | "svh" => "SystemVerilog source",
        "vhdl" => "VHDL hardware description",

        // SUPPLEMENTARY PART 13: Build Manifests, Declarative Specs & Infrastructure-as-Code
        "makefile" | "mk" => "Makefile build rules",
        "cmake" => "CMake build script",
        "nix" => "Nix package derivation",
        "guix" => "GNU Guix package definition",
        "bazel" => "Bazel build rule",
        "buck" => "Buck build rule",
        "tf" | "tfvars" => "Terraform infrastructure configuration",
        "slint" => "Slint markup",
        "lock" => "Lockfile",

        // SUPPLEMENTARY PART 14: Retro, Synthesizer & Legacy System Audio
        "8svx" => "Amiga 8SVX audio",
        "rol" => "AdLib Visual Composer song",
        "cmf" => "Creative Music File (FM synthesis)",
        "imf" => "id Software IMF music (OPL2)",
        "mus" => "Doom / Heretic MUS audio",
        "hmi" | "hmp" => "HMI MIDI music",
        "xmi" => "Miles Sound System XMI music",
        "rad" => "Reality Adlib Tracker module",
        "hsc" => "HSC-Tracker FM module",
        "sng" => "AdLib / Sound Club tracker song",
        "dwd" => "DiamondWare Digitized audio",
        "kmp" | "ksf" => "Korg Trinity / Triton multisample",

        // SUPPLEMENTARY SPECIALIZED PART 1: Industrial Automation, Robotics, PLCs & SCADA
        "plcopen" => "PLCopen XML automation exchange",
        "l5x" => "Rockwell Studio 5000 Logix XML",
        "acd" => "Rockwell Studio 5000 Logix project",
        "s7p" => "Siemens SIMATIC Step 7 project",
        "s7s" => "Siemens SIMATIC Step 7 subproject",
        "mwp" => "Siemens Micro/WIN PLC program",
        "tia" | "ap14" | "ap15" | "ap16" | "ap17" | "ap18" | "ap19" => "Siemens TIA Portal project archive",
        "pro" => "CoDeSys PLC / Qt project",
        "project" => "CoDeSys automation project",
        "urdf" => "Unified Robot Description Format (URDF)",
        "xacro" => "ROS Xacro robot macro",
        "bag" => "ROS 1 message recording bag",
        "mcap" => "MCAP robotics multimodal container",
        "nodeset" | "nodeset2" => "OPC UA NodeSet XML schema",
        "mtconnect" => "MTConnect manufacturing telemetry XML",
        "gsd" | "gsdml" => "PROFIBUS / PROFINET General Station Description",
        "eds" => "CANopen / DeviceNet Electronic Data Sheet",
        "xdd" => "CANopen XML Device Description",

        // SUPPLEMENTARY SPECIALIZED PART 2: Chemistry, Molecular Modeling & Materials Science
        "mol" => "MDL Molfile chemical structure",
        "smiles" => "SMILES chemical structure notation",
        "cml" => "Chemical Markup Language (CML)",
        "inchi" | "inchikey" => "IUPAC Chemical Identifier (InChI)",
        "cdx" => "ChemDraw chemical drawing",
        "cdxml" => "ChemDraw XML exchange",
        "pdbqt" => "AutoDock PDBQT molecular structure",
        "gro" => "GROMACS molecular structure coordinates",
        "top" => "GROMACS molecular topology",
        "itp" => "GROMACS included topology parameters",
        "trr" => "GROMACS full-precision trajectory",
        "xtc" => "GROMACS compressed portable trajectory",
        "dcd" => "CHARMM / NAMD molecular dynamics trajectory",
        "crd" => "AMBER molecular coordinates",
        "inp" => "Quantum chemistry / simulation input",
        "prmtop" | "pbrtop" => "AMBER molecular parameter topology",
        "out" => "Quantum chemistry calculation output log",
        "nw" => "NWChem computational chemistry input",
        "poscar" => "VASP POSCAR structure geometry",
        "potcar" => "VASP POTCAR pseudopotential library",
        "incar" => "VASP INCAR input parameters",
        "kpoints" => "VASP KPOINTS Brillouin zone sampling",
        "chk" => "Gaussian chemistry wavefunction checkpoint",

        // SUPPLEMENTARY SPECIALIZED PART 3: Astronomy, Astrophysics & Deep Space Missions
        "asdf" => "Advanced Scientific Data Format (ASDF)",
        "votable" | "vot" => "IVOA Virtual Observatory Table",
        "spk" => "NASA SPICE Ephemeris Kernel",
        "ik" => "NASA SPICE Instrument Kernel",
        "ck" => "NASA SPICE C-Matrix orientation Kernel",
        "fk" => "NASA SPICE Frame Kernel",
        "sclk" | "tsc" => "NASA SPICE Spacecraft Clock Kernel",
        "lsk" | "tls" => "NASA SPICE Leapseconds Kernel",
        "ecsv" => "Astropy Enhanced CSV table",

        // SUPPLEMENTARY SPECIALIZED PART 4: Geophysics, Seismology & Petroleum Exploration
        "segy" | "sgy" => "SEG-Y seismic reflection survey data",
        "segd" | "sgd" => "SEG-D field acquisition seismic data",
        "dlis" => "Digital Log Interchange Standard (DLIS) well log",
        "lis" => "Schlumberger Log Interchange Standard (LIS) well log",
        "resqml" => "RESQML earth resource and reservoir model",
        "witsml" => "WITSML wellsite drilling XML",
        "prodml" => "PRODML hydrocarbon production XML",
        "seed" => "SEED seismological earthquake data",
        "mseed" | "miniseed" => "MiniSEED continuous seismic time-series",
        "sac" => "Seismic Analysis Code (SAC) waveform",
        "win" | "win32" => "NIED WIN seismic waveform data",

        // SUPPLEMENTARY SPECIALIZED PART 5: RF Telecom, Software-Defined Radio (SDR) & Telephony
        "sigmf" => "SigMF RF recording",
        "sigmf-data" => "SigMF RF signal data",
        "sigmf-meta" => "SigMF RF signal metadata",
        "ciq" | "iq" => "Raw I/Q RF baseband digital samples",
        "s1p" | "s2p" | "s3p" | "s4p" | "snp" => "Touchstone RF S-parameter network data",
        "cfile" => "GNU Radio 32-bit float complex I/Q stream",
        "vrt" | "v49" => "VITA 49 Radio Transport (VRT) capture",
        "ss7" => "SS7 telecom protocol trace",
        "gtp" => "GTP mobile core telecom capture",
        "diameter" => "Diameter AAA telecom network capture",

        // SUPPLEMENTARY SPECIALIZED PART 6: Cybersecurity, Threat Intel, SBOM & Vulnerability Management
        "stix" | "stix2" => "OASIS STIX cyber threat intelligence JSON",
        "taxii" => "TAXII threat intelligence message",
        "sarif" => "OASIS SARIF static analysis security report",
        "spdx" => "SPDX Software Bill of Materials (SBOM)",
        "cyclonedx" => "OWASP CycloneDX SBOM",
        "csaf" => "CSAF JSON security advisory",
        "cvrf" => "CVRF XML vulnerability advisory",
        "yara" | "yar" => "YARA malware detection rule",
        "sigma" => "Sigma SIEM log detection rule",
        "rules" => "Snort / Suricata IDS network rule",
        "misp" => "MISP threat intelligence event",
        "kape" | "tkape" | "gkape" => "KAPE forensic triage target definition",

        // SUPPLEMENTARY SPECIALIZED PART 7: Aviation, Maritime Navigation & Transit Data
        "a424" | "arinc424" | "arinc" => "ARINC 424 aeronautical navigation database",
        "aixm" => "Aeronautical Information Exchange Model (AIXM)",
        "fixm" => "Flight Information Exchange Model (FIXM)",
        "wxxm" => "Weather Information Exchange Model (WXXM)",
        "000" | "s57" => "IHO S-57 Electronic Navigational Chart (ENC)",
        "s63" => "IHO S-63 encrypted nautical chart",
        "s100" => "IHO S-100 hydrographic framework data",
        "s101" => "IHO S-101 Electronic Navigational Chart",
        "ais" => "AIS marine vessel navigation telemetry",
        "gtfs" => "GTFS public transit feed package",
        "gtfsrt" => "GTFS Realtime transit update stream",

        // SUPPLEMENTARY SPECIALIZED PART 8: Color Management, Prepress & Printer Protocols
        "icc" | "icm" => "ICC color profile",
        "ppd" => "PostScript Printer Description (PPD)",
        "jdf" => "CIP4 Job Definition Format (JDF) prepress",
        "jmf" => "CIP4 Job Messaging Format (JMF)",
        "afp" | "afpds" => "IBM Advanced Function Presentation (AFP)",
        "pcl" | "pclx" | "pxl" => "HP Printer Command Language (PCL/PCL-XL)",
        "escp" | "epson" => "Epson ESC/P printer command stream",
        "spl" => "Windows Print Spooler payload",
        "shd" => "Windows Print Spooler Shadow header",
        "cups" => "CUPS device-ready print raster",

        // SUPPLEMENTARY SPECIALIZED PART 9: Low-Level Hardware, Embedded Device Trees & BIOS/UEFI
        "fd" => "BIOS / SPI Flash Descriptor image",
        "me" | "txe" => "Intel ME / TXE firmware image",
        "dtbo" => "Device Tree Overlay Blob (DTBO)",
        "dtsi" => "Device Tree Source include (DTSI)",
        "dsdt" | "ssdt" => "ACPI System Description Table",
        "aml" => "ACPI Machine Language (AML) bytecode",
        "dsl" | "asl" => "ACPI Source Language (ASL/DSL)",
        "orom" => "PCI Option ROM firmware",
        "pdr" => "Intel Platform Data Region (PDR)",

        // SUPPLEMENTARY SPECIALIZED PART 10: Compiler Internals, Debug Symbols & Profiling Runtimes
        "dwarf" => "DWARF debugging information",
        "bc" => "LLVM intermediate representation bitcode",
        "ll" => "LLVM intermediate representation assembly",
        "gimple" => "GCC GIMPLE intermediate representation",
        "perf" => "Linux perf event execution recording",
        "trace" => "Catapult / Chromium trace execution log",
        "speedscope" => "Speedscope flamegraph profile JSON",
        "hprof" => "Java JVM memory heap dump (HPROF)",
        "core" => "POSIX process crash core dump",

        // SUPPLEMENTARY SPECIALIZED PART 11: Key-Value Stores, NoSQL Internals & Log-Structured Engines
        "sst" | "sstable" => "LSM-Tree Sorted String Table (SSTable)",
        "manifest" => "LSM-Tree version manifest log",
        "rdb" => "Redis database binary snapshot (RDB)",
        "aof" => "Redis transaction append-only log (AOF)",
        "pgdump" | "dump" => "PostgreSQL database dump archive",
        "mrk" | "mrk2" | "mrk3" => "ClickHouse columnar data mark index",

        // SUPPLEMENTARY SPECIALIZED PART 12: Spatial Audio, Haptics & Acoustic Impulses
        "adm" => "ITU-R Audio Definition Model (ADM) spatial audio",
        "sofa" => "SOFA HRTF spatial acoustics dataset",
        "ahap" => "Apple Haptic Audio Pattern (AHAP) JSON",
        "sir" => "Super Impulse Response (SIR) reverb audio",
        "adv" => "Audio Design Vault DSP acoustic profile",

        // SUPPLEMENTARY SPECIALIZED PART 13: Web Archiving, E-Discovery, Legal Tech & Bibliographic Data
        "warc" => "WARC web archive (ISO 28500)",
        "wacz" => "WACZ web archive collection package",
        "edrm" => "EDRM e-discovery exchange XML",
        "opt" => "Opticon / Concordance image cross-reference",
        "marc" | "marc21" => "MARC21 machine-readable bibliographic record",
        "bib" | "bibtex" => "BibTeX bibliographic database",
        "ris" => "Research Information Systems (RIS) citation",
        "enw" => "EndNote bibliographic citation export",
        "mets" => "Library of Congress METS metadata schema",
        "premis" => "PREMIS digital preservation metadata",

        // SUPPLEMENTARY SPECIALIZED PART 14: Distributed Ledgers, Smart Contracts & Block Data
        "blk" => "Bitcoin Core raw blockchain block data",
        "rev" => "Bitcoin Core blockchain undo data",
        "sol" => "Solidity smart contract source",
        "vy" => "Vyper Pythonic smart contract source",
        "car" => "IPFS / IPLD Content Addressable Archive (CAR)",
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
        assert_eq!(category(&entry("track.opus", FileKind::File, 0o644)), Category::Audio);
        assert_eq!(category(&entry("music.flac", FileKind::File, 0o644)), Category::Audio);
        assert_eq!(category(&entry("chiptune.sid", FileKind::File, 0o644)), Category::Audio);
        assert_eq!(category(&entry("tracker.xm", FileKind::File, 0o644)), Category::Audio);
        assert_eq!(category(&entry("manual.pdf", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("sheet.xlsx", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("book.epub", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("font.woff2", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("scene.gltf", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("part.step", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("mesh.stl", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("arch.ifc", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("backup.zst", FileKind::File, 0o644)), Category::Archive);
        assert_eq!(category(&entry("disk.qcow2", FileKind::File, 0o644)), Category::Archive);
        assert_eq!(category(&entry("pkg.deb", FileKind::File, 0o644)), Category::Archive);
        assert_eq!(category(&entry("module.wasm", FileKind::File, 0o644)), Category::Executable);
        assert_eq!(category(&entry("class.class", FileKind::File, 0o644)), Category::Executable);
        assert_eq!(category(&entry("lib.so", FileKind::File, 0o644)), Category::Executable);
        assert_eq!(category(&entry("config.kdl", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("schema.proto", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("wire.msgpack", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("script.ts", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("data.parquet", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("app.sqlite", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("stream.arrow", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("model.safetensors", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("weights.gguf", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("vectors.lance", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("cert.pem", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("vault.kdbx", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("dump.pcapng", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("pqc.mlkem", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("contract.wit", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("manifest.c2pa", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("subtitles.srt", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("genome.fasta", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("chip.gds", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("notebook.ipynb", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("weather.grib2", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("evidence.e01", FileKind::File, 0o644)), Category::Archive);
        assert_eq!(category(&entry("card.vcf", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("rig.bvh", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("assets.vpk", FileKind::File, 0o644)), Category::Archive);
        assert_eq!(category(&entry("boot.hex", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("book.brf", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("design.vhdl", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("core.asm", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("rules.mk", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("voice.voc", FileKind::File, 0o644)), Category::Audio);
        assert_eq!(category(&entry("arm.urdf", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("robot.mcap", FileKind::File, 0o644)), Category::Archive);
        assert_eq!(category(&entry("aspirin.mol", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("POSCAR", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("jwst.asdf", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("survey.segy", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("capture.sigmf", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("threat.stix", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("chart.s57", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("display.icc", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("bios.fd", FileKind::File, 0o644)), Category::Executable);
        assert_eq!(category(&entry("perf.data", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("table.sst", FileKind::File, 0o644)), Category::Document);
        assert_eq!(category(&entry("spatial.adm", FileKind::File, 0o644)), Category::Audio);
        assert_eq!(category(&entry("crawl.warc", FileKind::File, 0o644)), Category::Archive);
        assert_eq!(category(&entry("contract.sol", FileKind::File, 0o644)), Category::Code);
        assert_eq!(category(&entry("data.car", FileKind::File, 0o644)), Category::Archive);

        assert_eq!(type_label(&entry("main.rs", FileKind::File, 0o644)), "Rust source");
        assert_eq!(type_label(&entry("song.flac", FileKind::File, 0o644)), "FLAC lossless audio");
        assert_eq!(type_label(&entry("voice.opus", FileKind::File, 0o644)), "Opus interactive audio");
        assert_eq!(type_label(&entry("c64.sid", FileKind::File, 0o644)), "Commodore 64 SID chiptune");
        assert_eq!(type_label(&entry("doc.pdf", FileKind::File, 0o644)), "PDF document");
        assert_eq!(type_label(&entry("sheet.xlsx", FileKind::File, 0o644)), "Excel spreadsheet");
        assert_eq!(type_label(&entry("cad.step", FileKind::File, 0o644)), "STEP 3D CAD model");
        assert_eq!(type_label(&entry("archive.zst", FileKind::File, 0o644)), "Zstandard compressed archive");
        assert_eq!(type_label(&entry("app.wasm", FileKind::File, 0o644)), "WebAssembly module");
        assert_eq!(type_label(&entry("data.json", FileKind::File, 0o644)), "JSON data document");
        assert_eq!(type_label(&entry("config.kdl", FileKind::File, 0o644)), "KDL document language");
        assert_eq!(type_label(&entry("analytics.parquet", FileKind::File, 0o644)), "Apache Parquet columnar data");
        assert_eq!(type_label(&entry("db.sqlite3", FileKind::File, 0o644)), "SQLite database");
        assert_eq!(
            type_label(&entry("weights.safetensors", FileKind::File, 0o644)),
            "Safetensors neural model weights"
        );
        assert_eq!(type_label(&entry("llm.gguf", FileKind::File, 0o644)), "GGUF quantized model weights");
        assert_eq!(type_label(&entry("pqc.mlkem", FileKind::File, 0o644)), "FIPS 203 ML-KEM post-quantum key");
        assert_eq!(type_label(&entry("provenance.c2pa", FileKind::File, 0o644)), "C2PA provenance manifest assertion");
        assert_eq!(type_label(&entry("photo.cr3", FileKind::File, 0o644)), "Canon Raw image");
        assert_eq!(type_label(&entry("render.exr", FileKind::File, 0o644)), "OpenEXR HDR image");
        assert_eq!(type_label(&entry("cinema.braw", FileKind::File, 0o644)), "Blackmagic RAW video");
        assert_eq!(type_label(&entry("subs.srt", FileKind::File, 0o644)), "SubRip subtitle");
        assert_eq!(type_label(&entry("seq.fa", FileKind::File, 0o644)), "FASTA biological sequence");
        assert_eq!(type_label(&entry("layout.gds", FileKind::File, 0o644)), "GDSII IC layout stream");
        assert_eq!(type_label(&entry("calc.ipynb", FileKind::File, 0o644)), "Jupyter Notebook");
        assert_eq!(type_label(&entry("grid.grb2", FileKind::File, 0o644)), "GRIB meteorological data");
        assert_eq!(type_label(&entry("dump.e01", FileKind::File, 0o644)), "Expert Witness forensic image (E01)");
        assert_eq!(type_label(&entry("person.vcf", FileKind::File, 0o644)), "vCard / Variant Call Format (VCF)");
        assert_eq!(type_label(&entry("anim.bvh", FileKind::File, 0o644)), "Biovision Hierarchy motion capture (BVH)");
        assert_eq!(type_label(&entry("level.bsp", FileKind::File, 0o644)), "Binary Space Partitioning level geometry");
        assert_eq!(type_label(&entry("flash.srec", FileKind::File, 0o644)), "Motorola S-Record firmware");
        assert_eq!(type_label(&entry("page.brf", FileKind::File, 0o644)), "Braille Ready Format (BRF)");
        assert_eq!(type_label(&entry("top.vhdl", FileKind::File, 0o644)), "VHDL hardware description");
        assert_eq!(type_label(&entry("start.s", FileKind::File, 0o644)), "Assembly source");
        assert_eq!(type_label(&entry("rules.mk", FileKind::File, 0o644)), "Makefile build rules");
        assert_eq!(type_label(&entry("snd.voc", FileKind::File, 0o644)), "Creative Voice audio");
        assert_eq!(type_label(&entry("arm.urdf", FileKind::File, 0o644)), "Unified Robot Description Format (URDF)");
        assert_eq!(type_label(&entry("robot.mcap", FileKind::File, 0o644)), "MCAP robotics multimodal container");
        assert_eq!(type_label(&entry("aspirin.mol", FileKind::File, 0o644)), "MDL Molfile chemical structure");
        assert_eq!(type_label(&entry("POSCAR", FileKind::File, 0o644)), "VASP POSCAR structure geometry");
        assert_eq!(type_label(&entry("jwst.asdf", FileKind::File, 0o644)), "Advanced Scientific Data Format (ASDF)");
        assert_eq!(type_label(&entry("voyager.spk", FileKind::File, 0o644)), "NASA SPICE Ephemeris Kernel");
        assert_eq!(type_label(&entry("survey.segy", FileKind::File, 0o644)), "SEG-Y seismic reflection survey data");
        assert_eq!(type_label(&entry("capture.sigmf", FileKind::File, 0o644)), "SigMF RF recording");
        assert_eq!(
            type_label(&entry("threat.stix", FileKind::File, 0o644)),
            "OASIS STIX cyber threat intelligence JSON"
        );
        assert_eq!(
            type_label(&entry("chart.s57", FileKind::File, 0o644)),
            "IHO S-57 Electronic Navigational Chart (ENC)"
        );
        assert_eq!(type_label(&entry("display.icc", FileKind::File, 0o644)), "ICC color profile");
        assert_eq!(type_label(&entry("bios.fd", FileKind::File, 0o644)), "BIOS / SPI Flash Descriptor image");
        assert_eq!(type_label(&entry("perf.data", FileKind::File, 0o644)), "Linux perf event execution recording");
        assert_eq!(type_label(&entry("table.sst", FileKind::File, 0o644)), "LSM-Tree Sorted String Table (SSTable)");
        assert_eq!(
            type_label(&entry("spatial.adm", FileKind::File, 0o644)),
            "ITU-R Audio Definition Model (ADM) spatial audio"
        );
        assert_eq!(type_label(&entry("crawl.warc", FileKind::File, 0o644)), "WARC web archive (ISO 28500)");
        assert_eq!(type_label(&entry("contract.sol", FileKind::File, 0o644)), "Solidity smart contract source");
        assert_eq!(
            type_label(&entry("data.car", FileKind::File, 0o644)),
            "IPFS / IPLD Content Addressable Archive (CAR)"
        );
        assert_eq!(type_label(&entry("a.qux", FileKind::File, 0o644)), "QUX file");
        assert_eq!(extension(".bashrc"), None);
    }
}
