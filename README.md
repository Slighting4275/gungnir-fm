# Gungnir FM

A fast dual-pane Linux file manager written in Rust with [Slint](https://slint.dev).
It combines Nemo's layout and keyboard ergonomics with sub-millisecond responsiveness, a modern dark theme, and an architecture where the UI thread never touches disk or blocking I/O. It serves as the companion GUI for [Gungnir VFS](https://gitlab.com/exalted6165/gungnir-vfs).

Source repository & issue tracker: [https://gitlab.com/exalted6165/gungnir-fm](https://gitlab.com/exalted6165/gungnir-fm).

---

## Key Features

### 🗂️ Panes, Tabs & Navigation
- **Dual-Pane Layout (`F3`)**: Single-pane default with instantaneous split-pane toggling.
- **Notebook Tabs per Pane (`Ctrl+T` / `Ctrl+W`)**: Independent multi-tab strips for primary and secondary panes, each maintaining its own path, navigation history stack (back/forward buffers), scroll position, and display titles.
- **Hybrid Path Bar**: Interactive breadcrumbs with Nemo-style home routing and remote badges, or direct editable path entry (`Ctrl+L`) for POSIX paths, `gungnir://`, `sftp://`, and `smb://` URIs.
- **Virtualized Views**: Compact list, detailed list, and thumbnail grid views virtualized with Slint `ListView`. 100,000-entry folders scroll smoothly without UI stutters.
- **Sortable Columns**: Natural, folders-first ordering by name, size, type, modified time, POSIX permissions, and Gungnir VFS sync status.

### 🖼️ Visual Image & Video Previews
- **Master Encyclopedia Media Support**:
  - **Universal & Web Raster**: JPEG (JFIF/EXIF), PNG, GIF, BMP/DIB, TIFF/BigTIFF, WebP, ICO, CUR, ICNS.
  - **High-Efficiency & Modern Encodings**: AVIF, AVIS, HEIC/HEIF, HEIFS, JPEG XL (`.jxl`), JPEG 2000 (`.jp2`, `.j2k`, `.jpx`), JPEG XR (`.jxr`), JPEG XS, FLIF, FUIF, BPG, QOI, Farbfeld.
  - **Vector & Procedural Graphics**: SVG, SVGZ, EPS, PS, AI, CDR, WMF, EMF, CGM, DXF, DWG, Gerber (`.gbr`), XAR, SK1, Flash (`.swf`, `.fla`).
  - **RAW Camera Sensors**: DNG, Canon CRW/CR2/CR3, Nikon NEF/NRW, Sony ARW/SRF/SR2, Fujifilm RAF, Olympus/OM ORF/OM, Panasonic RW2, Pentax PEF, Kodak KDC/DCR/DCS, Leaf/Mamiya MOS/MEF, Hasselblad 3FR/FFF, Phase One IIQ/CAP, Sigma X3F, Leica RWL, Samsung SRW, GoPro GPR, Minolta MRW.
  - **Professional HDR, VFX & 3D Textures**: OpenEXR (`.exr`), Radiance HDR (`.hdr`, `.pic`), DPX, Cineon (`.cin`), DirectDraw Surface (`.dds`), Khronos KTX/KTX2, ASTC, PowerVR PVR/PVRTC, PKM, Valve VTF, TGA/TARGA, Adobe PSD/PSB, GIMP XCF, Corel CPT, Affinity AFPHOTO, Paint.NET PDN, Krita KRA, OpenRaster ORA, Amiga IFF/ILBM, SGI RGB/BW.
  - **Scientific, Medical & Geospatial**: FITS, DICOM (`.dcm`), GeoTIFF, NITF, MrSID, ECW, HDF5/NetCDF (`.h5`, `.nc`), Aperio SVS, Hamamatsu NDPI, OME-TIFF, Gatan DM3/DM4, MRC, NIfTI (`.nii`), VTK.
  - **Legacy, Retro & Specialized Bitmaps**: PCX, Netpbm (`.pbm`, `.pgm`, `.ppm`, `.pam`, `.pnm`), XBM, XPM, XWD, Sun Raster, MacPaint, GEM IMG, WBMP, QuickDraw PICT, Dr. Halo CUT, BLP, PlayStation TIM/TIM2, PSP GXT.
  - **Video Containers & Cinema Mezzanine**: MP4, MKV, WebM, QuickTime MOV, AVI, WMV/ASF, FLV/F4V, OGV, 3GP/3G2, DivX/Xvid, MPEG Transport Stream (`.m2ts`, `.mts`), MPEG Program Stream (`.mpg`, `.vob`), HD-DVD EVO, MXF, GXF, DVR-MS, WTV, MOD/TOD, Apple ProRes, Avid DNxHD/DNxHR, Blackmagic RAW (`.braw`), REDCODE RAW (`.r3d`), ARRIRAW (`.ari`), Canon CRM, XAVC, DVCPRO, HDV, DV, RealMedia RM/RMVB, Bink BIK/BIK2, Smacker SMK, RoQ, AMV/MTV, IVF, Y4M, raw YUV.
  - **2026+ Active Specs**: AV2 (AOMedia Video 2), JPEG AI, 3D Gaussian Splatting (`.splat`, `.spz`), PLY point clouds.
- **Top Toolbar Zoom Slider**: Interactive zoom slider located directly in the top toolbar alongside view modes. Features stepping buttons (`[-]` / `[+]`), interactive track drag, and a percentage pill with click-to-reset.
- **Large-Scale Preview Scaling**: Smoothly scales previews from 50% to 350% (enlarging cards up to 840px in grid view and 80px in list view) with 512×512 thumbnail generation.
- **Decoupled Text Typography**: Text label font sizes remain locked to crisp 11–13px system typography while thumbnail preview cards enlarge prominently.
- **Disk Caching**: Asynchronous background generation cached under `~/.cache/gungnir-fm/thumbnails/` using source URI and modification timestamp keys.

### 📚 Master Encyclopedia: Universal Non-Image & Non-Video Format Engine
Gungnir FM provides native format detection, authoritative human-readable classification, Freedesktop MIME resolution, and Spacebar Quick Preview inspection across all 10 core technical domains:
- **Part 1: Audio Formats, Codecs & Sound Containers**:
  - *Lossless*: FLAC, ALAC, WAV, AIFF/AIF/AIFC, APE (Monkey's Audio), WavPack (`.wv`), TTA, Shorten (`.shn`), TAK, OptimFROG (`.ofr`, `.ofs`), DSD/DSF/DFF, DXD, MLP, WMA Lossless, BWF, RF64, Apple CAF, Sun/NeXT AU/SND, Wave64.
  - *Lossy*: MP3, AAC, Opus, Ogg Vorbis, AC-3, E-AC-3, Dolby TrueHD, DTS, DTS-HD MA, DTS:X, WMA, Sony ATRAC (`.oma`, `.omg`), Musepack (`.mpc`), Speex, AMR/AMR-WB, GSM 06.10, G.711/G.722/G.726/G.729, iLBC, SILK, QDMC, VSELP.
  - *Trackers & Chiptunes*: MIDI (`.mid`, `.midi`, `.smf`, `.rmi`, `.kar`), Soundtracker/ProTracker MOD, FastTracker 2 XM, Scream Tracker S3M, Impulse Tracker IT, MultiTracker MTM, Unreal UMX, Composer 669, Farandole FAR, Oktalyzer OKT, Commodore 64 SID, NES NSF/NSFE, SNES SPC700, VGM/VGZ, Genesis GYM, PC Engine HES, Game Boy GBS, MSX KSS, ZX Spectrum AY, Atari SAP.
  - *DAW Sessions & Soundfonts*: Ableton ALS, Cubase CPR, FL Studio FLP, REAPER RPP, Pro Tools PTX/PTF, Studio One SONG, Logic Pro LOGICX, Reason REASON, Sonar CWB, Audition SESX, SoundFont SF2/SF3, SFZ, Kontakt NKI, GigaStudio GIG.
- **Part 2: Documents, Office, Text, Layout & Typography**:
  - *Text & Markup*: TXT, Markdown (`.md`), AsciiDoc (`.adoc`), Emacs Org-mode (`.org`), reStructuredText (`.rst`), TeX/LaTeX (`.tex`), RTF, NFO, FILE_ID.DIZ, LOG, Perl POD.
  - *Paginated & Publishing*: PDF, PDF/A, PDF/X, PDF/E, PostScript (`.ps`), XPS, OpenXPS, DjVu (`.djvu`, `.djv`), EPUB, Mobipocket MOBI, Amazon Kindle AZW/AZW3/KF8, FictionBook FB2, Comic Book Archives (CBR, CBZ, CB7, CBT), Compiled HTML Help CHM, Microsoft Reader LIT, DVI.
  - *Spreadsheets*: Excel (XLSX, XLS, XLSM, XLSB, XLTX, XLTM), OpenDocument ODS/OTS, Apple Numbers, CSV, TSV/TAB, DIF, SYLK, Lotus 1-2-3 (WK1/WK3/WK4), Quattro Pro (QPW/WB1/WB2).
  - *Word Processing & Presentations*: Word (DOCX, DOC, DOCM, DOT, DOTX), OpenDocument ODT/OTT, Apple Pages, WordPerfect WPD, AbiWord ABW, PowerPoint (PPTX, PPT, PPTM, PPS, PPSX), OpenDocument ODP/OTP, Apple Keynote, OpenOffice SXI.
  - *Typography*: TrueType (TTF, TTC), OpenType (OTF, OTC), WOFF, WOFF2, EOT, dfont, PostScript Type 1 PFB, PFM, AFM, X11 BDF/PCF, SFNT, Windows FNT/FON, BMF.
- **Part 3: 3D Models, CAD, CAM, BIM & Spatial Data**:
  - *Realtime 3D & Interchange*: GLTF, Binary GLB, Pixar USD/USDA/USDC, USDZ, OBJ, MTL, FBX, COLLADA DAE, 3DS, Blender BLEND, Cinema 4D C4D, 3ds Max MAX, Maya MA/MB, LightWave LWO/LWS, Modo LXO, DirectX X, Source Engine SMD/DMX, Quake MD2/MD3/MD5.
  - *CAD, CAM & Solid Modeling*: STEP (`.step`, `.stp`), IGES (`.iges`, `.igs`), STL, 3MF, AMF, ACIS SAT/SAB, Parasolid X_T/X_B, Jupiter Tessellation JT, SolidWorks SLDPRT/SLDASM, CATIA CATPART/CATPRODUCT, Inventor IPT/IAM, Creo PRT/ASM, FreeCAD FCSTD, OpenSCAD SCAD, CNC G-code (`.gcode`, `.nc`).
  - *BIM & Architecture*: IFC (ISO 16739), Revit RVT/RFA, Navisworks NWD/NWC/NWF, ArchiCAD PLN, BIM Collaboration Format BCF.
  - *Point Clouds & Volumetrics*: PLY, Point Cloud Library PCD, ASPRS LAS, Lossless LASzip LAZ, ASTM E57, XYZ/PTS, OpenVDB (`.vdb`), Gaussian Splatting (`.splat`, `.spz`).
- **Part 4: Archives, Compression, Packages & Virtual Disks**:
  - *Archives*: ZIP/ZIPX, TAR, GZ, BZ2, XZ, Zstandard (`.zst`, `.zstd`), 7Z, RAR, CAB, ARJ, LHA/LZH, ALZ/EGG, Unix Z, LZ4, Brotli BR, CPIO, Unix AR, WinACE ACE, PAK, Doom WAD, SQX, ZOO, SEA ARC.
  - *Disk Images & ROMs*: Optical ISO, Raw IMG, Apple DMG, Microsoft VHD/VHDX, VMware VMDK, VirtualBox VDI, QEMU QCOW/QCOW2, CUE, Alcohol MDF/MDS, Nero NRG, DiscJuggler CDI, Floppy DSK, Amiga ADF, Nintendo ROMs (NDS, GBA, NES, SFC/SMC, Z64/N64), MAME CHD, PSP CSO, Wii WBFS.
  - *Packages*: DEB, RPM, Android APK/AAB, iOS IPA, Windows MSI/MSIX/APPX, macOS PKG, Flatpak (`.flatpak`, `.flatpakref`), AppImage, Ubuntu Snap, Gentoo ebuild.
- **Part 5: Executables, Compiled Objects, Binaries & Runtimes**:
  - *Native Binaries*: Linux/BSD ELF, Windows PE (`.exe`, `.dll`, `.sys`), Mach-O, COFF, a.out, DOS COM, compiled object files (`.o`), Shared Objects (`.so`), macOS dylib, static archives (`.a`, `.lib`).
  - *Bytecode & Shaders*: WebAssembly WASM/WAT, Java CLASS/JAR/WAR/EAR, Python PYC/PYO, Android Dalvik DEX/ODEX/VDEX, Erlang BEAM, Lua LUAC, .NET CIL/MSIL, SPIR-V SPV, DirectX DXBC/DXIL, SourcePawn SMX, Flash ABC.
- **Part 6: Data Serialization, Configuration & Protocols**:
  - *Structured Configs*: JSON, JSONC, JSON5, JSON-LD, YAML, TOML, XML, INI, Java properties, CONF/CFG, shell ENV, Apple PLIST, HashiCorp HCL, KDL, Rusty Object Notation RON.
  - *Binary Serialization*: Protocol Buffers (`.proto`, `.pb`), FlatBuffers FBS, MessagePack, BSON, Apache Avro, CBOR, ASN.1, Apache Thrift, Cap'n Proto, Jackson Smile, Amazon Ion.
  - *Web & APIs*: HTML, XHTML, CSS, SCSS/SASS, LESS, JavaScript (JS, MJS, CJS), TypeScript (TS, MTS, CTS), React JSX/TSX, GraphQL (`.graphql`, `.gql`), WSDL, RSS, Atom.
- **Part 7: Databases, Big Data, Storage Engines & GIS**:
  - *Relational & Embedded*: SQL dumps, SQLite (`.sqlite`, `.sqlite3`, `.db`), WAL/SHM, Access MDB/ACCDB, dBase DBF, SQL Server MDF/LDF, MySQL InnoDB IBD/FRM, Firebird FDB, InterBase GDB, raw DAT.
  - *Columnar & Scientific*: Apache Parquet, Apache ORC, Arrow Feather, Arrow IPC, HDF5, NetCDF, Zarr, CERN ROOT, Stata DTA, SPSS SAV/POR, R RDATA/RDS, NumPy NPY/NPZ.
  - *GIS Spatial*: GeoJSON, KML/KMZ, ESRI Shapefiles (`.shp`, `.shx`), GPX, OGC GeoPackage GPKG, GML, OpenStreetMap OSM/PBF, DEM elevation rasters.
- **Part 8: Machine Learning, AI Weights & Computational Tensors**:
  - *Weights & Checkpoints*: Safetensors, GGUF, GGML, ONNX, PyTorch PT/PTH, training CKPT, TensorFlow Lite TFLITE, Keras H5/KERAS, Apple Core ML MLMODEL/MLPACKAGE, TensorRT ENGINE/PLAN, MyriadX BLOB.
  - *Vector Search & Embeddings*: Lance columnar data (`.lance`), FAISS index, Spotify Annoy, USearch, HNSW graphs, WebDataset training shards.
- **Part 9: Cryptography, Security, Identity & Telemetry**:
  - *PKI & Vaults*: PEM, DER, X.509 CRT/CER, private keys (`.key`), PKCS #12 P12/PFX, Java KeyStore JKS/BKS, CSR, CRL, SSH/PGP PUB, PGP/GPG ASC/GPG, age encryption, KeePass KDBX.
  - *Observability & Forensics*: libpcap PCAP, PCAPNG, CAP, HTTP Archive HAR, Windows Event Log EVTX, Syslog streams, Windows ETL, Google pprof, OpenTelemetry OTLP.
- **Part 10: 2026 Active Standards & Future Formats**:
  - *Post-Quantum Cryptography*: FIPS 203 ML-KEM key containers (`.mlkem`, `.kem`), FIPS 204 ML-DSA and FIPS 205 SLH-DSA signature/key blobs (`.mldsa`, `.slhdsa`).
  - *Advanced AI & Runtimes*: GGUF v3/v4 MoE/FP4/MXFP8 containers, Safetensors 2.0 streaming sharded formats, LanceDB v2 columnar engine, WebAssembly Interface Types (`.wit`), C2PA provenance manifest assertions (`.c2pa`).

### 🖥️ Universal Display Server & Window Manager Compatibility
Gungnir FM runs natively without crashes, panics, or missing window mappings across 48 Linux and UNIX display targets:
- **Display Servers & Protocols**: Wayland, XLibre, X.Org Server, XFree86, Phoenix X Server, Arcan, Mir (legacy standalone), Y Window System, TinyX / KDrive, XWayland, Xephyr, Xvfb (Virtual Framebuffer), Xvnc / TigerVNC, nxagent (X2Go / NX), Xdummy, DirectFB / Linux Framebuffer (`/dev/fb*` & direct DRM/KMS `/dev/dri/card*`), kmscon, Twin.
- **Tiling & Dynamic Window Managers**: i3wm, dwm, bspwm, AwesomeWM, xmonad, Qtile (X11 backend), LeftWM, Herbstluftwm, Spectrwm, Ratpoison, StumpWM, cwm (Calm Window Manager). Window min-size is relaxed to 480×320 for split-column tiling without clipping.
- **Floating, Stacking & Desktop Environments**: Openbox, Fluxbox, Blackbox, IceWM, JWM, Window Maker, AfterStep, FVWM, PekWM, twm, mwm, KWin (KDE Plasma), Mutter (GNOME), Compiz, Metacity, Xfwm4 (Xfce), Marco (MATE), Enlightenment (E16 / E26 X11 engine).
- **ICCCM & EWMH Compliance**: Automatic `RESOURCE_NAME` and `XCLIENT_NAME` setup guarantees standardized `WM_CLASS = "gungnir-fm"` and `_NET_WM_ICON` across all WMs.
- **Automated Software Rendering Fallback**: On headless or virtual X servers lacking hardware GLX (e.g. Xvfb, Xdummy, TinyX, nxagent), Gungnir FM catches initialization errors and automatically switches to `SLINT_BACKEND=winit-software`.

### ⚙️ Native Cross-Init System & Supervisor Specification
Gungnir FM is built to run natively across all 24 Linux and BSD init systems and process supervisors without requiring or assuming `systemd` or `libsystemd`:
1. **systemd** (`init/systemd/gungnir-fm.service`)
2. **OpenRC** (`init/openrc/gungnir-fm`)
3. **runit** (`init/runit/gungnir-fm/run`)
4. **s6 & s6-rc** (`init/s6/gungnir-fm/`)
5. **dinit** (`init/dinit/gungnir-fm`)
6. **SysVinit / inittab** (`init/sysvinit/gungnir-fm`)
7. **BSD Init / rc.subr** (`init/bsd/gungnir_fm`)
8. **GNU Shepherd** (`init/shepherd/gungnir-fm.scm`)
9. **BusyBox init** (`init/busybox/rc.gungnir-fm`)
10. **Toybox init** (`init/busybox/rc.gungnir-fm`)
11. **Finit** (`init/finit/gungnir-fm.conf`)
12. **procd (OpenWrt)** (`init/procd/gungnir-fm.init`)
13. **sinit (suckless init)**
14. **nosh** (`init/daemontools/gungnir-fm/run`)
15. **daemontools** (`init/daemontools/gungnir-fm/run`)
16. **daemontools-encore** (`init/daemontools/gungnir-fm/run`)
17. **perp** (`init/daemontools/gungnir-fm/run`)
18. **anopa** (`init/s6/gungnir-fm/`)
19. **cinit**
20. **initng**
21. **Upstart** (`init/upstart/gungnir-fm.conf`)
22. **OpenLaunchD** (`init/launchd/org.gungnir.fm.plist`)
23. **Epoch**
24. **minit** & **simpleinit-msb**

- **Foreground by Default**: Complies with modern process supervisors; writes stream logs to stdout/stderr.
- **Agnostic Readiness Notification**: Built-in support for pipe file descriptors (`--ready-fd <FD>` or `$S6_NOTIFICATION_FD`) and raw datagram sockets (`$NOTIFY_SOCKET`) without linking against `libsystemd`.
- **Legacy Daemonization**: Clean `-d / --daemon` and `--pidfile <PATH>` options for traditional SysVinit and BSD `rc.subr` scripts.
- **POSIX /bin/sh Shell Scripts**: All deployment, installation, and build scripts strictly adhere to POSIX `/bin/sh` syntax.

### 🔒 File Hashing & Interactive Checksum Verification
- **Streaming Multi-Hash Engine**: Computes MD5, SHA-1, SHA-256, and SHA-512 concurrently in a single streaming pass over 64 KB buffers without loading entire files into memory.
- **Interactive Checksum Dialog**: Right-click any file → **"Check File Hash…"** displays all computed digests with an interactive input field. Pasting an expected hash provides immediate visual verification (green on match, red on mismatch).
- **Checksum File Verification**: Automatically parses and verifies `.sha256`, `.md5`, `.sha512`, `.sha1`, and `SHA256SUMS` files against referenced targets in the folder.

### 🌐 Built-in Remote Filesystems (SFTP / SSH & SMB / CIFS)
- **Native Remote VFS**: Built-in `RemoteVfsBackend` handling `sftp://`, `ssh://`, `smb://`, and `cifs://` schemes.
- **"Connect to Server…" Dialog**: Accessible from the File menu or command palette, enabling direct connections to remote network shares.
- **Full VFS Parity**: Remote directory streaming, stat inspection, file operations, remote breadcrumb parsing, and status badges.

### ⚡ Custom Actions Engine (Nemo Actions Equivalent)
- **Declarative TOML Actions**: Loads user-defined actions from `~/.config/gungnir-fm/actions/*.toml` (with built-in fallbacks).
- **Target & MIME Filtering**: Rules match on target kind (`file`, `directory`, `any`), file extensions, or MIME patterns.
- **Token Substitution**: Standard tokens `%d` (active directory), `%f` (single selection), `%F` (multi-selection), and `%u` (URI).
- **Asynchronous Execution**: Spawns processes in the background via Tokio without blocking the UI event loop.
- **Default Actions Included**:
  - *Open Terminal Here* (`alacritty`, `ptyxis`, `gnome-terminal`, etc.)
  - *Calculate SHA-256 to Clipboard*
  - *Verify Checksum File*

### 💾 Crash-Resilient Session Persistence & Recovery
- **Automatic State Saving**: Debounced auto-save (500 ms) to `~/.config/gungnir-fm/session.json`.
- **Atomic Writes**: Writes to temporary files and atomically renames to prevent corruption during unexpected shutdowns or crashes.
- **Persisted State**: Window size and maximized state, active pane, split view toggle, open tabs per pane, preview zoom level, hidden file visibility (`Ctrl+H`), and sidebar state (`F9`).
- **Safe Recovery**: Automatically restores open tabs and paths on restart, safely falling back to the user's home directory if any saved path is unmounted or inaccessible.

### 🔌 Removable Drives & Desktop Integration
- **Removable Media**: D-Bus and `udisks2` integration displaying removable storage drives, partitions, and mount states in the sidebar.
- **"Open With…" Chooser**: Freedesktop `.desktop` parser ranking default handlers, explicit associations, and parent MIME handlers.
- **Drag-and-Drop & Clipboard**: Seamless drag-and-drop between panes and tabs, plus standard cut/copy/paste (`Ctrl+X`, `Ctrl+C`, `Ctrl+V`).

### 🔱 Gungnir VFS Companion
- **Native `gungnir://` Protocol**: Connects to the local `gungnird` daemon via a zero-copy shared-memory ring buffer (protocol v2).
- **Live Metadata**: Real-time `metadata::*` xattr tags, cluster node routing, and mount-health fail-fast monitoring.
- **Mock Mode**: Built-in mock provider for headless development and testing without a live daemon.

---

## Keyboard Shortcuts

| Shortcut | Action |
| --- | --- |
| `F3` | Toggle dual split view |
| `F9` | Toggle sidebar (Places, Gungnir, Tree, Devices) |
| `Ctrl+T` | Open new tab in current directory |
| `Ctrl+W` | Close active tab (collapses secondary pane if last tab) |
| `Ctrl+Tab` / `Ctrl+PageDown` | Switch to next tab |
| `Ctrl+Shift+Tab` / `Ctrl+PageUp` | Switch to previous tab |
| `Alt+1` .. `Alt+9` | Jump directly to tab index 1–9 |
| `Ctrl+L` | Focus location bar (type path or URI) |
| `Alt+Up` | Navigate up to parent directory |
| `Alt+Left` / `Alt+Right` | Navigate back / forward in tab history |
| `Ctrl+H` | Toggle hidden files |
| `Ctrl+1` / `Ctrl+2` / `Ctrl+3` | Switch view: Detailed list / Compact list / Grid |
| `Ctrl++` / `Ctrl+=` | Zoom in previews / icons (up to 350%) |
| `Ctrl+-` / `Ctrl+_` | Zoom out previews / icons (down to 50%) |
| `Ctrl+0` | Reset preview zoom to 100% |
| `Space` | Quick Preview overlay (image/video/text) |
| `F2` | Rename selected file or directory |
| `F5` / `F6` | Copy / Move selection to other pane |
| `F7` | Create new directory |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | Copy / Cut / Paste selection |
| `Delete` | Move selection to Trash |
| `Shift+Delete` | Permanently delete selection |
| `F10` | Open context menu |

---

## Building and Installation

### Build Requirements
- Linux (x86_64, AArch64)
- Rust 1.92+ (edition 2024)
- Slint runtime dependencies (fontconfig, xkbcommon, X11 or Wayland libraries)
- Optional tools for preview enhancements: `ffmpegthumbnailer`, `ffmpeg`, `imagemagick`

### Compile from Source
```sh
cargo build --release
target/release/gungnir-fm [LEFT_PATH] [RIGHT_PATH]
```

### User Installation
To install into `~/.local/bin` with the desktop icon and application launcher entry:
```sh
scripts/install-local.sh
```
To uninstall:
```sh
scripts/install-local.sh --uninstall
```

### Running Tests
The workspace includes unit and integration tests across all crates (sandboxed and isolated, with zero personal data access):
```sh
cargo test --workspace
cargo clippy --workspace --all-targets
```

---

## License

GPL-3.0-or-later (see [LICENSE](LICENSE)). Slint is used under its GPLv3 option.
