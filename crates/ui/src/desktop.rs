//! Desktop integration for "Open With": the freedesktop.org specs, implemented directly.
//!
//! - MIME type of a file name: shared-mime-info `mime/globs2` (weighted globs) plus
//!   `mime/subclasses` for parent types (`text/x-rust` falls back to `text/plain`).
//! - Applications: `applications/*.desktop` in XDG data dirs, highest priority first
//!   (`$XDG_DATA_HOME`, then `$XDG_DATA_DIRS`); a desktop ID found earlier masks later ones,
//!   `Hidden=true` deletes it, `TryExec` must exist.
//! - Defaults and associations: `mimeapps.list` in XDG config dirs, then data dirs
//!   (`[Default Applications]`, `[Added Associations]`, `[Removed Associations]`), and the
//!   legacy `defaults.list`.
//! - Launching: Desktop Entry `Exec` quoting and field codes (`%f %F %u %U %i %c %k %%`).
//!
//! Everything here does blocking file I/O; the controller calls it on blocking threads.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Where to look, highest priority first.
#[derive(Debug, Clone)]
pub struct Dirs {
    pub data: Vec<PathBuf>,
    pub config: Vec<PathBuf>,
}

fn env_dirs(var: &str, default: &str) -> Vec<PathBuf> {
    let v = std::env::var(var).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_owned());
    v.split(':').map(PathBuf::from).filter(|p| p.is_absolute()).collect()
}

impl Dirs {
    /// From the XDG environment variables (with the spec's defaults).
    #[must_use]
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let abs = |var: &str, fallback: PathBuf| {
            std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or(fallback)
        };
        let mut data = vec![abs("XDG_DATA_HOME", home.join(".local/share"))];
        data.extend(env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share:/usr/pkg/share"));
        let mut config = vec![abs("XDG_CONFIG_HOME", home.join(".config"))];
        config.extend(env_dirs("XDG_CONFIG_DIRS", "/etc/xdg"));
        Self { data, config }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// Desktop file ID, e.g. `org.gnome.TextEditor.desktop`.
    pub id: String,
    pub name: String,
    pub exec: String,
    pub icon: String,
    pub path: PathBuf,
    pub mimes: Vec<String>,
    pub no_display: bool,
}

#[derive(Debug, Default)]
struct Globs {
    /// Lowercased extension (may contain dots, e.g. `tar.gz`) -> (weight, mime).
    ext: HashMap<String, (u32, String)>,
    /// Exact file names -> (weight, mime).
    literal: HashMap<String, (u32, String)>,
}

#[derive(Debug, Default)]
pub struct DesktopIndex {
    apps: HashMap<String, App>,
    by_mime: HashMap<String, Vec<String>>,
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, HashSet<String>>,
    parents: HashMap<String, Vec<String>>,
    globs: Globs,
}

/// `[Group]` -> key -> value of an ini-style file (comments and blank lines skipped).
fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut group = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(g) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            group = g.to_owned();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.entry(group.clone()).or_default().entry(k.trim().to_owned()).or_insert_with(|| v.trim().to_owned());
        }
    }
    out
}

fn list(v: &str) -> Vec<String> {
    v.split(';').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// `TryExec`: an absolute path that exists, or a name found on `$PATH`.
fn try_exec_ok(prog: &str) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let executable = |path: &Path| {
        if !path.is_file() {
            return false;
        }
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: path is a valid NUL-terminated string, and X_OK only checks access.
        unsafe { libc::access(path.as_ptr(), libc::X_OK) == 0 }
    };
    if prog.contains('/') {
        return Path::new(prog).is_absolute() && executable(Path::new(prog));
    }
    !prog.is_empty()
        && std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| executable(&d.join(prog))))
}

/// All `*.desktop` files below `dir`, with their desktop IDs (`sub/x.desktop` -> `sub-x.desktop`).
fn desktop_files(dir: &Path, prefix: &str, out: &mut Vec<(String, PathBuf)>) {
    let mut pending = vec![(dir.to_owned(), prefix.to_owned(), Vec::<PathBuf>::new())];
    while let Some((dir, prefix, mut ancestors)) = pending.pop() {
        let Ok(canonical) = std::fs::canonicalize(&dir) else { continue };
        if ancestors.len() >= 64 || ancestors.contains(&canonical) {
            continue;
        }
        ancestors.push(canonical);
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let path = entry.path();
            if path.is_dir() {
                pending.push((path, format!("{prefix}{name}-"), ancestors.clone()));
            } else if name.ends_with(".desktop") {
                out.push((format!("{prefix}{name}"), path));
            }
        }
    }
}

impl DesktopIndex {
    /// Read everything from `dirs`. Missing files are simply skipped.
    #[must_use]
    pub fn load(dirs: &Dirs) -> Self {
        let mut idx = Self::default();
        let mut seen: HashSet<String> = HashSet::new();
        for d in &dirs.data {
            let mut files = Vec::new();
            desktop_files(&d.join("applications"), "", &mut files);
            files.sort();
            for (id, path) in files {
                if !seen.insert(id.clone()) {
                    continue; // a higher-priority dir already defined (or deleted) this ID
                }
                let Ok(text) = std::fs::read_to_string(&path) else { continue };
                let ini = parse_ini(&text);
                let Some(e) = ini.get("Desktop Entry") else { continue };
                let truthy = |k: &str| e.get(k).is_some_and(|v| v == "true");
                if e.get("Type").map(String::as_str) != Some("Application") || truthy("Hidden") {
                    continue;
                }
                if e.get("TryExec").is_some_and(|t| !try_exec_ok(t)) {
                    continue;
                }
                let (Some(name), Some(exec)) = (e.get("Name"), e.get("Exec")) else { continue };
                let app = App {
                    id: id.clone(),
                    name: name.clone(),
                    exec: exec.clone(),
                    icon: e.get("Icon").cloned().unwrap_or_default(),
                    path,
                    mimes: e.get("MimeType").map(|m| list(m)).unwrap_or_default(),
                    no_display: truthy("NoDisplay"),
                };
                for m in &app.mimes {
                    idx.by_mime.entry(m.clone()).or_default().push(id.clone());
                }
                idx.apps.insert(id, app);
            }
        }
        // mimeapps.list: config dirs, then data dirs' applications/, then legacy defaults.list.
        let mut lists: Vec<PathBuf> = dirs.config.iter().map(|c| c.join("mimeapps.list")).collect();
        lists.extend(dirs.data.iter().map(|d| d.join("applications/mimeapps.list")));
        lists.extend(dirs.data.iter().map(|d| d.join("applications/defaults.list")));
        for path in lists {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let ini = parse_ini(&text);
            if let Some(g) = ini.get("Default Applications") {
                for (mime, ids) in g {
                    idx.defaults.entry(mime.clone()).or_insert_with(|| list(ids)); // first file wins
                }
            }
            if let Some(g) = ini.get("Added Associations") {
                for (mime, ids) in g {
                    idx.added.entry(mime.clone()).or_default().extend(list(ids));
                }
            }
            if let Some(g) = ini.get("Removed Associations") {
                for (mime, ids) in g {
                    idx.removed.entry(mime.clone()).or_default().extend(list(ids));
                }
            }
        }
        // shared-mime-info: later (lower-priority) dirs first, so higher ones override.
        for d in dirs.data.iter().rev() {
            if let Ok(text) = std::fs::read_to_string(d.join("mime/globs2")) {
                idx.globs.add_globs2(&text);
            }
            if let Ok(text) = std::fs::read_to_string(d.join("mime/subclasses")) {
                for line in text.lines() {
                    if let Some((child, parent)) = line.split_once(' ') {
                        idx.parents.entry(child.to_owned()).or_default().push(parent.trim().to_owned());
                    }
                }
            }
        }
        idx
    }

    /// The MIME type of a file name (`inode/directory` for folders).
    #[must_use]
    pub fn mime_for(&self, name: &str, is_dir: bool) -> String {
        if is_dir {
            return "inode/directory".into();
        }
        self.globs.lookup(name).unwrap_or_else(|| builtin_mime(name).into())
    }

    /// Applications for `mime`, best first: the default, added associations, then every
    /// app declaring the type, then the same for parent types. No duplicates, no removed
    /// associations.
    #[must_use]
    pub fn apps_for(&self, mime: &str) -> Vec<&App> {
        let mut out: Vec<&App> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut queue = vec![mime.to_owned()];
        let mut visited: HashSet<String> = HashSet::new();
        while let Some(m) = (!queue.is_empty()).then(|| queue.remove(0)) {
            if !visited.insert(m.clone()) {
                continue;
            }
            let removed = self.removed.get(&m);
            let ids = self
                .defaults
                .get(&m)
                .into_iter()
                .flatten()
                .chain(self.added.get(&m).into_iter().flatten())
                .chain(self.by_mime.get(&m).into_iter().flatten());
            for id in ids {
                if removed.is_some_and(|r| r.contains(id)) {
                    continue;
                }
                if let Some(app) = self.apps.get(id)
                    && seen.insert(app.id.as_str())
                {
                    out.push(app);
                }
            }
            if let Some(p) = self.parents.get(&m) {
                queue.extend(p.iter().cloned());
            } else if m.starts_with("text/") && m != "text/plain" {
                queue.push("text/plain".into());
            }
        }
        out
    }

    /// Returns the active default application for `mime`, or the first associated application.
    #[must_use]
    pub fn default_app_for(&self, mime: &str) -> Option<&App> {
        if let Some(ids) = self.defaults.get(mime) {
            for id in ids {
                if let Some(app) = self.apps.get(id) {
                    return Some(app);
                }
            }
        }
        self.apps_for(mime).first().copied()
    }

    /// Applications specifically registered/recommended for `mime` (excluding the default).
    #[must_use]
    pub fn recommended_apps_for(&self, mime: &str) -> Vec<&App> {
        let def_id = self.default_app_for(mime).map(|a| a.id.as_str());
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        if let Some(d) = def_id {
            seen.insert(d);
        }
        for app in self.apps_for(mime) {
            if seen.insert(app.id.as_str()) {
                out.push(app);
            }
        }
        out
    }

    /// Other installed system applications not specifically associated with `mime`.
    #[must_use]
    pub fn other_apps_for(&self, mime: &str) -> Vec<&App> {
        let def_id = self.default_app_for(mime).map(|a| a.id.as_str());
        let mut seen: HashSet<&str> = HashSet::new();
        if let Some(d) = def_id {
            seen.insert(d);
        }
        for app in self.apps_for(mime) {
            seen.insert(app.id.as_str());
        }
        let mut other: Vec<&App> =
            self.apps.values().filter(|a| !a.no_display && !seen.contains(a.id.as_str())).collect();
        other.sort_by_key(|a| a.name.to_lowercase());
        other
    }

    /// All installed desktop applications.
    #[must_use]
    pub fn all_apps(&self) -> Vec<&App> {
        let mut list: Vec<&App> = self.apps.values().filter(|a| !a.no_display).collect();
        list.sort_by_key(|a| a.name.to_lowercase());
        list
    }
}

impl Globs {
    fn add_globs2(&mut self, text: &str) {
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(4, ':');
            let (Some(w), Some(mime), Some(glob)) = (parts.next(), parts.next(), parts.next()) else { continue };
            let Ok(weight) = w.parse::<u32>() else { continue };
            let case_sensitive = parts.next().is_some_and(|f| f.split(',').any(|x| x == "cs"));
            let entry = (weight, mime.to_owned());
            if let Some(ext) = glob.strip_prefix("*.") {
                if !ext.contains(['*', '?', '[']) {
                    let key = if case_sensitive { ext.to_owned() } else { ext.to_lowercase() };
                    let better = self.ext.get(&key).is_none_or(|(w0, _)| weight >= *w0);
                    if better {
                        self.ext.insert(key, entry);
                    }
                }
            } else if !glob.contains(['*', '?', '[']) {
                self.literal.insert(glob.to_owned(), entry);
            }
        }
    }

    /// Exact name first, then the longest matching extension (`x.tar.gz` -> `tar.gz`).
    fn lookup(&self, name: &str) -> Option<String> {
        if let Some((_, m)) = self.literal.get(name) {
            return Some(m.clone());
        }
        let lower = name.to_lowercase();
        let mut rest = name;
        let mut rest_lower = lower.as_str();
        while let Some(i) = rest.find('.') {
            let (ext, ext_lower) = (&rest[i + 1..], &rest_lower[i + 1..]);
            if let Some((_, m)) = self.ext.get(ext).or_else(|| self.ext.get(ext_lower)) {
                return Some(m.clone());
            }
            rest = ext;
            rest_lower = ext_lower;
        }
        None
    }
}

/// Built-in fallback MIME resolver for standard encyclopedia formats when system mime-info is missing.
fn builtin_mime(name: &str) -> &'static str {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return "application/octet-stream";
    };
    match ext.to_ascii_lowercase().as_str() {
        // Audio
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "m4a" | "aac" => "audio/mp4",
        "aiff" | "aif" | "aifc" => "audio/x-aiff",
        "ape" => "audio/x-ape",
        "wv" => "audio/x-wavpack",
        "mid" | "midi" | "smf" | "kar" => "audio/midi",
        "mod" => "audio/x-mod",
        "xm" => "audio/x-xm",
        "s3m" => "audio/x-s3m",
        "it" => "audio/x-it",

        // Text & Code
        "txt" | "log" | "nfo" | "diz" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "html" | "htm" => "text/html",
        "xhtml" => "application/xhtml+xml",
        "css" => "text/css",
        "scss" | "sass" => "text/x-scss",
        "less" => "text/x-less",
        "js" | "mjs" | "cjs" => "application/javascript",
        "ts" | "mts" | "cts" => "application/typescript",
        "json" | "jsonc" | "json5" => "application/json",
        "jsonld" => "application/ld+json",
        "yaml" | "yml" => "application/yaml",
        "toml" => "application/toml",
        "xml" => "application/xml",
        "ini" | "conf" | "cfg" => "text/x-ini",
        "sh" | "bash" | "zsh" | "fish" => "application/x-shellscript",
        "rs" => "text/x-rust",
        "c" | "h" => "text/x-c",
        "cpp" | "hpp" | "cc" | "cxx" => "text/x-c++",
        "py" | "pyw" => "text/x-python",
        "go" => "text/x-go",
        "lua" => "text/x-lua",
        "sql" => "application/sql",
        "csv" => "text/csv",
        "tsv" | "tab" => "text/tab-separated-values",

        // Documents & Office
        "pdf" | "pdfa" | "pdfx" | "pdfe" => "application/pdf",
        "epub" => "application/epub+zip",
        "mobi" | "azw" | "azw3" => "application/x-mobipocket-ebook",
        "djvu" | "djv" => "image/vnd.djvu",
        "rtf" => "application/rtf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "doc" => "application/msword",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xls" => "application/vnd.ms-excel",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ppt" => "application/vnd.ms-powerpoint",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",

        // Fonts
        "ttf" | "ttc" => "font/ttf",
        "otf" | "otc" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",

        // 3D & Models
        "gltf" => "model/gltf+json",
        "glb" => "model/gltf-binary",
        "usd" | "usda" | "usdc" | "usdz" => "model/vnd.usdz+zip",
        "obj" => "model/obj",
        "stl" => "model/stl",
        "step" | "stp" => "model/step",
        "blend" => "application/x-blender",

        // Archives
        "zip" | "zipx" => "application/zip",
        "tar" => "application/x-tar",
        "gz" | "gzip" => "application/gzip",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "zst" | "zstd" => "application/zstd",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "iso" => "application/x-iso9660-image",
        "deb" => "application/vnd.debian.binary-package",
        "rpm" => "application/x-rpm",
        "apk" => "application/vnd.android.package-archive",

        // Databases & Modern AI / 2026+ Specs
        "sqlite" | "sqlite3" | "db" => "application/vnd.sqlite3",
        "parquet" => "application/vnd.apache.parquet",
        "arrow" | "feather" => "application/vnd.apache.arrow.file",
        "wasm" => "application/wasm",
        "wit" => "text/x-wit",
        "safetensors" => "application/x-safetensors",
        "gguf" => "application/x-gguf",
        "c2pa" => "application/x-c2pa-manifest",
        "pem" | "crt" | "cer" => "application/x-x509-ca-cert",
        "pcap" | "pcapng" | "cap" => "application/vnd.tcpdump.pcap",
        "urdf" | "xacro" => "application/xml",
        "mcap" => "application/x-mcap",
        "bag" => "application/x-ros-bag",
        "mol" => "chemical/x-mdl-molfile",
        "cml" => "chemical/x-cml",
        "cif" => "chemical/x-cif",
        "asdf" => "application/x-asdf",
        "segy" | "sgy" => "application/x-segy",
        "sarif" => "application/sarif+json",
        "spdx" => "text/spdx",
        "cyclonedx" => "application/vnd.cyclonedx+json",
        "yara" | "yar" => "text/x-yara",
        "dts" | "dtsi" => "text/x-devicetree-source",
        "dtb" | "dtbo" => "application/x-devicetree-binary",
        "warc" => "application/warc",
        "wacz" => "application/x-wacz",
        "sol" => "text/x-solidity",
        "icc" | "icm" => "application/vnd.iccprofile",
        "sofa" => "application/x-sofa",
        _ => "application/octet-stream",
    }
}

/// Split an `Exec` value into arguments (Desktop Entry spec quoting rules).
fn tokenize(exec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut has_token = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                has_token = true;
            }
            '\\' if in_quotes => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            c => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

fn file_uri(p: &Path) -> String {
    core_vfs::VfsUri::from_local_path(p).map(|u| u.to_uri_string()).unwrap_or_else(|_| p.display().to_string())
}

/// The command lines that open `files` with `app`: one per file for `%f`/`%u`, one for
/// `%F`/`%U` (or for an `Exec` without file codes, which then gets no files).
#[must_use]
pub fn command_lines(app: &App, files: &[PathBuf]) -> Vec<Vec<String>> {
    let args = tokenize(&app.exec);
    let single = args.iter().any(|a| a.contains("%f") || a.contains("%u"));
    let expand = |chunk: &[PathBuf]| -> Vec<String> {
        let mut out = Vec::new();
        for a in &args {
            match a.as_str() {
                "%F" => out.extend(chunk.iter().map(|p| p.display().to_string())),
                "%U" => out.extend(chunk.iter().map(|p| file_uri(p))),
                "%i" => {
                    if !app.icon.is_empty() {
                        out.push("--icon".into());
                        out.push(app.icon.clone());
                    }
                }
                _ => {
                    let first = chunk.first();
                    let mut s = String::new();
                    let mut it = a.chars().peekable();
                    while let Some(c) = it.next() {
                        if c != '%' {
                            s.push(c);
                            continue;
                        }
                        match it.next() {
                            Some('f') => s.push_str(&first.map(|p| p.display().to_string()).unwrap_or_default()),
                            Some('u') => s.push_str(&first.map(|p| file_uri(p)).unwrap_or_default()),
                            Some('c') => s.push_str(&app.name),
                            Some('k') => s.push_str(&app.path.display().to_string()),
                            Some('%') => s.push('%'),
                            _ => {} // %F/%U inside a word, and deprecated codes, are dropped
                        }
                    }
                    if !s.is_empty() || !a.contains('%') {
                        out.push(s);
                    }
                }
            }
        }
        out
    };
    if single && files.len() > 1 {
        files.iter().map(|f| expand(std::slice::from_ref(f))).collect()
    } else {
        vec![expand(files)]
    }
}

/// Start `app` on `files`, detached in its own process group (closing Gungnir FM does not
/// close it). Returns how many processes were started.
///
/// # Errors
/// The spawn error of the first command that failed.
pub fn launch(app: &App, files: &[PathBuf]) -> std::io::Result<usize> {
    use std::os::unix::process::CommandExt;
    let mut n = 0;
    for argv in command_lines(app, files) {
        let Some((prog, rest)) = argv.split_first() else { continue };
        let mut cmd = std::process::Command::new(prog);
        cmd.args(rest)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        if let Some(dir) = files.first().and_then(|f| f.parent()) {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn()?;
        // Reap it whenever it exits, on a tiny thread, so no zombie is left behind.
        std::thread::Builder::new().stack_size(64 * 1024).spawn(move || {
            let _ = child.wait();
        })?;
        n += 1;
    }
    Ok(n)
}

/// User-friendly display name for a MIME type matching Nemo / desktop standards.
#[must_use]
pub fn mime_description(mime: &str) -> String {
    match mime {
        "video/mp4" => "MPEG-4 video".into(),
        "video/x-matroska" => "Matroska video".into(),
        "video/webm" => "WebM video".into(),
        "video/quicktime" => "QuickTime video".into(),
        "video/x-msvideo" => "AVI video".into(),
        "video/mpeg" => "MPEG video".into(),
        "audio/mpeg" => "MP3 audio".into(),
        "audio/flac" => "FLAC audio".into(),
        "audio/ogg" => "Ogg Vorbis audio".into(),
        "audio/opus" => "Opus audio".into(),
        "audio/wav" => "WAV audio".into(),
        "audio/x-wavpack" => "WavPack audio".into(),
        "image/png" => "PNG image".into(),
        "image/jpeg" => "JPEG image".into(),
        "image/webp" => "WebP image".into(),
        "image/svg+xml" => "SVG vector image".into(),
        "image/gif" => "GIF image".into(),
        "image/avif" => "AVIF image".into(),
        "image/jxl" => "JPEG XL image".into(),
        "application/pdf" => "PDF document".into(),
        "application/zip" => "Zip archive".into(),
        "application/x-tar" => "Tar archive".into(),
        "application/x-7z-compressed" => "7-Zip archive".into(),
        "application/x-rar" | "application/vnd.rar" => "RAR archive".into(),
        "application/zstd" => "Zstandard compressed archive".into(),
        "application/gzip" => "Gzip compressed archive".into(),
        "application/x-xz" => "XZ compressed archive".into(),
        "application/x-bzip2" => "Bzip2 compressed archive".into(),
        "text/plain" => "Plain text document".into(),
        "text/markdown" => "Markdown document".into(),
        "text/html" => "HTML document".into(),
        "text/x-rust" => "Rust source code".into(),
        "text/x-python" => "Python script".into(),
        "text/x-c" => "C source code".into(),
        "text/x-c++" => "C++ source code".into(),
        "application/json" => "JSON document".into(),
        "application/toml" => "TOML document".into(),
        "inode/directory" => "Folder".into(),
        other => {
            if let Some((cat, sub)) = other.split_once('/') {
                let clean_sub = sub.strip_prefix("x-").unwrap_or(sub).replace('-', " ");
                let clean_cat = cat.replace('-', " ");
                format!("{clean_sub} ({clean_cat})")
            } else {
                other.to_string()
            }
        }
    }
}

/// Run the platform opener and surface a failed exit status rather than claiming
/// a file opened merely because the process could be spawned.
pub fn open_default_with(program: &std::ffi::OsStr, target: &std::ffi::OsStr) -> std::io::Result<()> {
    let status = std::process::Command::new(program)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("Default application opener exited with {status}")))
    }
}

/// Assign `desktop_id` as the default application for `mime` in `config_dir/mimeapps.list`.
///
/// # Errors
/// Standard I/O errors when writing `mimeapps.list`.
pub fn set_default_app_in(config_dir: &Path, mime: &str, desktop_id: &str) -> std::io::Result<()> {
    let path = config_dir.join("mimeapps.list");
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(String::from).collect();

    let id_val = if desktop_id.ends_with(';') { desktop_id.to_string() } else { format!("{desktop_id};") };

    let mut in_default = false;
    let mut default_header_idx = None;
    let mut mime_default_idx = None;

    let mut in_added = false;
    let mut added_header_idx = None;
    let mut mime_added_idx = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "[Default Applications]" {
            in_default = true;
            in_added = false;
            default_header_idx = Some(i);
        } else if trimmed == "[Added Associations]" {
            in_added = true;
            in_default = false;
            added_header_idx = Some(i);
        } else if trimmed.starts_with('[') {
            in_default = false;
            in_added = false;
        } else if let Some((k, _)) = trimmed.split_once('=')
            && k.trim() == mime
        {
            if in_default {
                mime_default_idx = Some(i);
            } else if in_added {
                mime_added_idx = Some(i);
            }
        }
    }

    // 1. Update [Default Applications]
    if let Some(idx) = mime_default_idx {
        lines[idx] = format!("{mime}={id_val}");
    } else if let Some(header_idx) = default_header_idx {
        lines.insert(header_idx + 1, format!("{mime}={id_val}"));
        if let Some(ref mut a_idx) = added_header_idx
            && *a_idx > header_idx
        {
            *a_idx += 1;
        }
    } else {
        lines.insert(0, "[Default Applications]".to_string());
        lines.insert(1, format!("{mime}={id_val}"));
        lines.insert(2, String::new());
        if let Some(ref mut a_idx) = added_header_idx {
            *a_idx += 3;
        }
    }

    // 2. Update [Added Associations] to also include desktop_id
    if let Some(idx) = mime_added_idx {
        let existing = lines[idx].split_once('=').map(|(_, v)| v.trim()).unwrap_or("");
        if !existing.split(';').any(|x| x.trim() == desktop_id.trim_end_matches(';')) {
            lines[idx] = format!("{mime}={id_val}{existing}");
        }
    } else if let Some(header_idx) = added_header_idx {
        lines.insert(header_idx + 1, format!("{mime}={id_val}"));
    } else {
        lines.push("[Added Associations]".to_string());
        lines.push(format!("{mime}={id_val}"));
        lines.push(String::new());
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = config_dir.join(format!(".mimeapps.tmp.{}", std::process::id()));
    std::fs::write(&temp, out)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// Reset any user default application override for `mime` in `config_dir/mimeapps.list`.
pub fn reset_default_app_in(config_dir: &Path, mime: &str) -> std::io::Result<()> {
    let path = config_dir.join("mimeapps.list");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut lines = Vec::new();
    let mut in_target_sec = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[Default Applications]" || trimmed == "[Added Associations]" {
            in_target_sec = true;
            lines.push(line.to_string());
            continue;
        } else if trimmed.starts_with('[') {
            in_target_sec = false;
        }

        if in_target_sec && trimmed.split_once('=').map(|(k, _)| k.trim()) == Some(mime) {
            continue;
        }
        lines.push(line.to_string());
    }

    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    let temp = config_dir.join(format!(".mimeapps.tmp.{}", std::process::id()));
    std::fs::write(&temp, out)?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// Convenience function updating standard user config (`$XDG_CONFIG_HOME/mimeapps.list`).
pub fn set_default_app(mime: &str, desktop_id: &str) -> std::io::Result<()> {
    let dirs = Dirs::from_env();
    if let Some(cfg) = dirs.config.first() {
        set_default_app_in(cfg, mime, desktop_id)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no XDG_CONFIG_HOME"))
    }
}

/// Convenience function resetting standard user config.
pub fn reset_default_app(mime: &str) -> std::io::Result<()> {
    let dirs = Dirs::from_env();
    if let Some(cfg) = dirs.config.first() { reset_default_app_in(cfg, mime) } else { Ok(()) }
}

pub fn set_default_app_with_dirs(dirs: Option<&Dirs>, mime: &str, id: &str) -> std::io::Result<()> {
    if let Some(dirs) = dirs {
        let dir = dirs.config.first().ok_or_else(|| std::io::Error::other("No desktop config directory"))?;
        set_default_app_in(dir, mime, id)
    } else {
        set_default_app(mime, id)
    }
}

pub fn reset_default_app_with_dirs(dirs: Option<&Dirs>, mime: &str) -> std::io::Result<()> {
    if let Some(dirs) = dirs {
        let dir = dirs.config.first().ok_or_else(|| std::io::Error::other("No desktop config directory"))?;
        reset_default_app_in(dir, mime)
    } else {
        reset_default_app(mime)
    }
}

pub fn create_custom_desktop_entry_with_dirs(dirs: Option<&Dirs>, cmd: &str, name: &str) -> std::io::Result<String> {
    if let Some(dirs) = dirs {
        let dir = dirs.data.first().ok_or_else(|| std::io::Error::other("No desktop data directory"))?;
        create_custom_desktop_entry_in(dir, cmd, name)
    } else {
        create_custom_desktop_entry(cmd, name)
    }
}

fn custom_tokens(command: &str) -> std::io::Result<Vec<String>> {
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid custom application command");
    if command.contains(['\0', '\n', '\r']) {
        return Err(invalid());
    }
    let (mut quote, mut escaped, mut started) = (None, false, false);
    let (mut args, mut token) = (Vec::new(), String::new());
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        if escaped {
            token.push(ch);
            escaped = false;
            started = true;
            continue;
        }
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => token.push(ch),
            (Some('"'), '\\') if chars.peek().is_some_and(|ch| !matches!(ch, '\\' | '"' | '$' | '`')) => {
                token.push('\\');
                started = true;
            }
            (_, '\\') => {
                escaped = true;
                started = true;
            }
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (None, ch) if ch.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut token));
                    started = false;
                }
            }
            _ => {
                token.push(ch);
                started = true;
            }
        }
    }
    if quote.is_some() || escaped {
        return Err(invalid());
    }
    if started {
        args.push(token);
    }
    if args.first().is_none_or(|arg| arg.is_empty()) {
        return Err(invalid());
    }
    Ok(args)
}

pub fn custom_command_name(command: &str) -> std::io::Result<String> {
    let args = custom_tokens(command)?;
    Ok(Path::new(&args[0])
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| args[0].clone()))
}

fn custom_exec(command: &str) -> std::io::Result<String> {
    let args = custom_tokens(command)?;
    let quote = |arg: &str| {
        let mut output = String::from("\"");
        for ch in arg.chars() {
            if matches!(ch, '\\' | '"' | '$' | '`') {
                output.push('\\');
            }
            if ch == '%' {
                output.push('%');
            }
            output.push(ch);
        }
        output.push('"');
        output
    };
    Ok(format!("{} %f", args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ")))
}

pub fn launch_custom(command: &str, files: &[PathBuf]) -> std::io::Result<usize> {
    let app = App {
        id: String::new(),
        name: "Custom application".into(),
        exec: custom_exec(command)?,
        icon: String::new(),
        path: PathBuf::new(),
        mimes: Vec::new(),
        no_display: true,
    };
    launch(&app, files)
}

/// Create a distinct user desktop entry, preserving quoted arguments and never
/// replacing another custom command with the same display name.
pub fn create_custom_desktop_entry_in(data_dir: &Path, cmd: &str, name: &str) -> std::io::Result<String> {
    use std::io::Write;
    if name.contains(['\0', '\n', '\r']) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Invalid application name"));
    }
    let exec = custom_exec(cmd)?;
    let app_dir = data_dir.join("applications");
    std::fs::create_dir_all(&app_dir)?;
    let safe_name: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let safe_name = if safe_name.is_empty() { "custom-app".to_string() } else { safe_name };
    let content =
        format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nCategories=Other;\nNoDisplay=true\n");
    for attempt in 0..10_000 {
        let id = if attempt == 0 {
            format!("userapp-{safe_name}.desktop")
        } else {
            format!("userapp-{safe_name}-{attempt}.desktop")
        };
        let path = app_dir.join(&id);
        let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        if let Err(error) = file.write_all(content.as_bytes()) {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        return Ok(id);
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "No free custom application name"))
}

/// Creates a user desktop file in `$XDG_DATA_HOME/applications` for a custom command.
pub fn create_custom_desktop_entry(cmd: &str, name: &str) -> std::io::Result<String> {
    let dirs = Dirs::from_env();
    if let Some(data) = dirs.data.first() {
        create_custom_desktop_entry_in(data, cmd, name)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no XDG_DATA_HOME"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn app(exec: &str) -> App {
        App {
            id: "demo.desktop".into(),
            name: "Demo App".into(),
            exec: exec.into(),
            icon: "demo".into(),
            path: "/usr/share/applications/demo.desktop".into(),
            mimes: Vec::new(),
            no_display: false,
        }
    }

    #[test]
    fn custom_commands_preserve_arguments_percent_and_distinct_entries() {
        let command = r#""/opt/My App/viewer" --title "100% ready""#;
        assert_eq!(custom_tokens(r#"app "\n" "" a\ b"#).unwrap(), ["app", r"\n", "", "a b"]);
        assert_eq!(
            custom_tokens("'/opt/My App/viewer' --title 'hello there'").unwrap(),
            ["/opt/My App/viewer", "--title", "hello there"]
        );
        assert_eq!(custom_command_name(r#""/opt/My App/viewer" --title "test""#).unwrap(), "viewer");
        let entry = App {
            id: "custom.desktop".into(),
            name: "Custom".into(),
            exec: custom_exec(command).unwrap(),
            icon: String::new(),
            path: PathBuf::new(),
            mimes: Vec::new(),
            no_display: true,
        };
        assert_eq!(
            command_lines(&entry, &[PathBuf::from("/tmp/file name")]),
            vec![vec!["/opt/My App/viewer", "--title", "100% ready", "/tmp/file name"]]
        );
        for bad in ["", "\"unterminated", "program\nInjected=true"] {
            assert!(custom_exec(bad).is_err());
        }
        let t = Tree::new("distinct-custom");
        let first = create_custom_desktop_entry_in(&t.0, "player --one", "Player").unwrap();
        let contents = std::fs::read(t.0.join("applications").join(&first)).unwrap();
        let second = create_custom_desktop_entry_in(&t.0, "player --two", "Player").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(t.0.join("applications").join(first)).unwrap(), contents);
    }

    #[test]
    fn desktop_scan_ignores_symlink_cycles_and_nonexecutable_tryexec() {
        use std::os::unix::fs::PermissionsExt;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.bug-check/test-bin")
            .join(format!("desktop-cycle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app.desktop"), "[Desktop Entry]\nType=Application\nName=App\nExec=true\n").unwrap();
        std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();
        let mut entries = Vec::new();
        desktop_files(&dir, "", &mut entries);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "app.desktop");
        let executable = dir.join("program");
        std::fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(!try_exec_ok(executable.to_str().unwrap()));
        assert!(!try_exec_ok(dir.to_str().unwrap()));
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(try_exec_ok(executable.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn default_opener_reports_nonzero_exit_status() {
        use std::ffi::OsStr;
        assert!(open_default_with(OsStr::new("/usr/bin/false"), OsStr::new("/unused target")).is_err());
        assert!(open_default_with(OsStr::new("/usr/bin/true"), OsStr::new("/unused target")).is_ok());
    }

    #[test]
    fn exec_tokenizing_and_field_codes() {
        assert_eq!(tokenize(r#"viewer --title "My \"Viewer\"" %U"#), ["viewer", "--title", r#"My "Viewer""#, "%U"]);
        let files = [PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/c.txt")];
        assert_eq!(command_lines(&app("ed %F"), &files), [vec!["ed", "/tmp/a b.txt", "/tmp/c.txt"]]);
        assert_eq!(command_lines(&app("ed %U"), &files[..1]), [vec!["ed", "file:///tmp/a%20b.txt"]]);
        assert_eq!(
            command_lines(&app("ed --file=%f"), &files),
            [vec!["ed", "--file=/tmp/a b.txt"], vec!["ed", "--file=/tmp/c.txt"]]
        );
        assert_eq!(command_lines(&app("ed %i %c %%"), &[]), [vec!["ed", "--icon", "demo", "Demo App", "%"]]);
        assert_eq!(command_lines(&app("ed %d %m"), &files[..1]), [vec!["ed"]], "deprecated codes are dropped");
    }

    /// A synthetic XDG tree in the test sandbox (never the real system).
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join("gungnir-test-sandbox").join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap_or_else(|e| panic!("{e}"));
            Self(p)
        }

        fn write(&self, rel: &str, body: &str) {
            let p = self.0.join(rel);
            if let Some(d) = p.parent() {
                std::fs::create_dir_all(d).unwrap_or_else(|e| panic!("{e}"));
            }
            std::fs::write(p, body).unwrap_or_else(|e| panic!("{e}"));
        }

        fn dirs(&self) -> Dirs {
            Dirs { data: vec![self.0.join("home-data"), self.0.join("sys-data")], config: vec![self.0.join("config")] }
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn index_ranks_default_then_associations_then_parents() {
        let t = Tree::new("desktop");
        t.write("sys-data/mime/globs2", "50:text/plain:*.txt\n50:text/x-rust:*.rs\n50:application/x-compressed-tar:*.tar.gz\n10:application/gzip:*.gz\n60:text/x-makefile:Makefile\n");
        t.write("sys-data/mime/subclasses", "text/x-rust text/plain\n");
        let entry = |name: &str, mime: &str| {
            format!("[Desktop Entry]\nType=Application\nName={name}\nExec={name} %F\nMimeType={mime};\n")
        };
        t.write("sys-data/applications/editor_alpha.desktop", &entry("EditorAlpha", "text/plain"));
        t.write("sys-data/applications/editor_beta.desktop", &entry("EditorBeta", "text/plain;text/x-rust"));
        t.write("sys-data/applications/viewer_gamma.desktop", &entry("ViewerGamma", "text/plain"));
        t.write("sys-data/applications/sub/nested.desktop", &entry("Nested", "application/gzip"));
        t.write(
            "home-data/applications/viewer_gamma.desktop",
            "[Desktop Entry]\nType=Application\nName=ViewerGamma\nExec=x\nHidden=true\n",
        );
        t.write("config/mimeapps.list", "[Default Applications]\ntext/plain=editor_alpha.desktop;\n[Removed Associations]\ntext/x-rust=editor_beta.desktop;\n");
        let idx = DesktopIndex::load(&t.dirs());

        assert_eq!(idx.mime_for("notes.TXT", false), "text/plain");
        assert_eq!(
            idx.mime_for("archive_alpha.tar.gz", false),
            "application/x-compressed-tar",
            "longest extension wins"
        );
        assert_eq!(idx.mime_for("Makefile", false), "text/x-makefile");
        assert_eq!(idx.mime_for("blob", false), "application/octet-stream");
        assert_eq!(idx.mime_for("project_demo", true), "inode/directory");

        let names = |m: &str| idx.apps_for(m).iter().map(|a| a.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            names("text/plain"),
            ["EditorAlpha", "EditorBeta"],
            "default first; Hidden user entry masks ViewerGamma"
        );
        // text/x-rust: EditorBeta is removed for it, then the parent text/plain apps follow.
        // text/x-rust: EditorBeta's own association is removed, so the parent type
        // (text/plain) supplies the list: its default first, then EditorBeta again.
        assert_eq!(names("text/x-rust"), ["EditorAlpha", "EditorBeta"]);
        assert_eq!(idx.apps_for("application/gzip")[0].id, "sub-nested.desktop");

        // Built-in encyclopedia MIME fallbacks when globs2 is not available
        assert_eq!(builtin_mime("audio.flac"), "audio/flac");
        assert_eq!(builtin_mime("voice.opus"), "audio/opus");
        assert_eq!(builtin_mime("document.pdf"), "application/pdf");
        assert_eq!(builtin_mime("model.gltf"), "model/gltf+json");
        assert_eq!(builtin_mime("archive.zst"), "application/zstd");
        assert_eq!(builtin_mime("runtime.wasm"), "application/wasm");
        assert_eq!(builtin_mime("analytics.parquet"), "application/vnd.apache.parquet");
        assert_eq!(builtin_mime("weights.safetensors"), "application/x-safetensors");
        assert_eq!(builtin_mime("llm.gguf"), "application/x-gguf");
        assert_eq!(builtin_mime("manifest.c2pa"), "application/x-c2pa-manifest");
    }

    #[test]
    fn test_set_and_reset_default_app() {
        let t = Tree::new("mimeapps");
        let cfg_dir = t.0.join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();

        // 1. Assign default app
        assert!(set_default_app_in(&cfg_dir, "video/mp4", "mpv.desktop").is_ok());
        let content = std::fs::read_to_string(cfg_dir.join("mimeapps.list")).unwrap();
        assert!(content.contains("[Default Applications]"));
        assert!(content.contains("video/mp4=mpv.desktop;"));
        assert!(content.contains("[Added Associations]"));

        // 2. Change default app
        assert!(set_default_app_in(&cfg_dir, "video/mp4", "vlc.desktop").is_ok());
        let content2 = std::fs::read_to_string(cfg_dir.join("mimeapps.list")).unwrap();
        assert!(content2.contains("video/mp4=vlc.desktop;"));
        assert!(!content2.contains("video/mp4=mpv.desktop;"));

        // 3. Reset default app
        assert!(reset_default_app_in(&cfg_dir, "video/mp4").is_ok());
        let content3 = std::fs::read_to_string(cfg_dir.join("mimeapps.list")).unwrap();
        assert!(!content3.contains("video/mp4=vlc.desktop;"));
    }

    #[test]
    fn test_custom_desktop_entry() {
        let t = Tree::new("userapp");
        let data_dir = t.0.join("data");
        let id = create_custom_desktop_entry_in(&data_dir, "/usr/local/bin/my-player", "My Player").unwrap();
        assert_eq!(id, "userapp-MyPlayer.desktop");
        let desktop_file = data_dir.join("applications").join(&id);
        assert!(desktop_file.exists());
        let text = std::fs::read_to_string(desktop_file).unwrap();
        assert!(text.contains("Name=My Player"));
        assert!(text.contains(r#"Exec="/usr/local/bin/my-player" %f"#));
    }
}
