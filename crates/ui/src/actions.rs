//! Custom Actions engine (Nemo Actions equivalent).
//!
//! Declarative action definitions loaded from `~/.config/gungnir-fm/actions/*.toml`
//! (or internal defaults table if directory does not exist).
//! Matches actions against selected items and executes substituted commands.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ActionTarget {
    File,
    Directory,
    #[default]
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomAction {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub target: ActionTarget,
    #[serde(default)]
    pub mime_types: Vec<String>,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub multiselection: bool,
    pub command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ActionConfigFile {
    Table { action: CustomAction },
    Multi { actions: Vec<CustomAction> },
    Direct(CustomAction),
}

#[derive(Debug, Clone)]
pub struct TargetItem {
    pub name: String,
    pub path: PathBuf,
    pub uri: String,
    pub is_dir: bool,
    pub mime: Option<String>,
}

impl CustomAction {
    /// Check whether this action matches the given active directory and selected targets.
    #[must_use]
    pub fn matches(&self, _active_dir: &Path, targets: &[TargetItem]) -> bool {
        if targets.is_empty() {
            // Empty selection: context menu on active directory background.
            if self.target == ActionTarget::File {
                return false;
            }
            if !self.extensions.is_empty() || !self.mime_types.is_empty() {
                return false;
            }
            return true;
        }

        if targets.len() > 1 && !self.multiselection {
            return false;
        }

        for item in targets {
            match self.target {
                ActionTarget::File => {
                    if item.is_dir {
                        return false;
                    }
                }
                ActionTarget::Directory => {
                    if !item.is_dir {
                        return false;
                    }
                }
                ActionTarget::Any => {}
            }

            if !self.extensions.is_empty() {
                let name_lower = item.name.to_lowercase();
                let matches_ext = self.extensions.iter().any(|ext| {
                    let clean = ext.trim_start_matches('.').to_lowercase();
                    name_lower.ends_with(&format!(".{clean}")) || name_lower == clean
                });
                if !matches_ext {
                    return false;
                }
            }

            if !self.mime_types.is_empty() {
                let Some(item_mime) = item.mime.as_deref() else {
                    return false;
                };
                let matches_mime = self.mime_types.iter().any(|pattern| {
                    if pattern == "*/*" || pattern == "*" {
                        true
                    } else if let Some(prefix) = pattern.strip_suffix("/*") {
                        item_mime.split_once('/').is_some_and(|(kind, _)| kind.eq_ignore_ascii_case(prefix))
                    } else {
                        item_mime.eq_ignore_ascii_case(pattern)
                    }
                });
                if !matches_mime {
                    return false;
                }
            }
        }

        true
    }

    /// Substitute replacement tokens:
    /// - `%d`: Active directory path
    /// - `%f`: Selected file or folder path (first if multi)
    /// - `%F`: Space-separated list of all selected paths
    /// - `%u`: URI of selected target (or active directory URI if none)
    /// - `%%`: Literal `%`
    #[must_use]
    pub fn substitute(&self, active_dir: &str, targets: &[TargetItem]) -> String {
        substitute_tokens(&self.command, active_dir, targets)
    }

    /// Launch the action command asynchronously.
    pub fn execute(&self, active_dir: &Path, targets: &[TargetItem]) -> std::io::Result<tokio::process::Child> {
        let active_dir_str = active_dir.display().to_string();
        let cmd_str = self.substitute(&active_dir_str, targets);
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(cmd_str)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if active_dir.is_dir() {
            cmd.current_dir(active_dir);
        }
        cmd.spawn()
    }
}

/// Shell-escape a string for safe inclusion in POSIX `sh -c` command lines.
#[must_use]
pub fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    // Safe characters that require no quotes in POSIX sh
    if s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '@')) {
        return s.to_string();
    }
    // Single-quote wrapping with internal single-quotes replaced by '\''
    let mut out = String::with_capacity(s.len() + 8);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Substitute tokens in a command string template with shell-safe escaping.
#[must_use]
pub fn substitute_tokens(template: &str, active_dir: &str, targets: &[TargetItem]) -> String {
    let escaped_dir = shell_escape(active_dir);
    let first_path = targets.first().map(|t| shell_escape(&t.path.display().to_string())).unwrap_or_default();
    let all_paths = targets.iter().map(|t| shell_escape(&t.path.display().to_string())).collect::<Vec<_>>().join(" ");
    let first_uri = targets.first().map(|t| shell_escape(&t.uri)).unwrap_or_else(|| {
        let uri = core_vfs::VfsUri::from_local_path(Path::new(active_dir))
            .map(|uri| uri.to_uri_string())
            .unwrap_or_else(|_| format!("file://{active_dir}"));
        shell_escape(&uri)
    });

    let mut out = String::with_capacity(template.len() + 32);
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('d') => out.push_str(&escaped_dir),
                Some('f') => out.push_str(&first_path),
                Some('F') => out.push_str(&all_paths),
                Some('u') => out.push_str(&first_uri),
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub struct ActionRegistry {
    actions: Vec<CustomAction>,
}

impl ActionRegistry {
    /// Load actions from a directory of `.toml` files, or use internal defaults if none found.
    #[must_use]
    pub fn load_or_default(actions_dir: Option<&Path>) -> Self {
        let mut actions = Vec::new();
        if let Some(dir) = actions_dir
            && dir.is_dir()
            && let Ok(entries) = std::fs::read_dir(dir)
        {
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
                .collect();
            paths.sort();
            for path in paths {
                if let Ok(content) = crate::session::read_text_file(&path)
                    && let Ok(cfg) = toml::from_str::<ActionConfigFile>(&content)
                {
                    match cfg {
                        ActionConfigFile::Table { action } | ActionConfigFile::Direct(action) => {
                            actions.push(action);
                        }
                        ActionConfigFile::Multi { actions: list } => {
                            actions.extend(list);
                        }
                    }
                }
            }
        }

        if actions.is_empty() {
            actions = Self::default_actions();
        }

        Self { actions }
    }

    /// Internal default actions table.
    #[must_use]
    pub fn default_actions() -> Vec<CustomAction> {
        vec![
            CustomAction {
                id: "open_terminal_here".into(),
                name: "Open Terminal Here".into(),
                icon: "utilities-terminal".into(),
                target: ActionTarget::Any,
                mime_types: Vec::new(),
                extensions: Vec::new(),
                multiselection: true,
                command: "sh -c 't=\"${1:-$2}\"; if [ -n \"$t\" ] && [ -d \"$t\" ]; then d=\"$t\"; elif [ -n \"$t\" ] && [ -f \"$t\" ]; then d=\"$(dirname \"$t\")\"; elif [ -n \"$2\" ] && [ -d \"$2\" ]; then d=\"$2\"; else d=\".\"; fi; cd \"$d\" 2>/dev/null || exit 1; if [ -n \"$TERMINAL\" ] && command -v \"$TERMINAL\" >/dev/null 2>&1; then exec \"$TERMINAL\"; elif command -v xdg-terminal-exec >/dev/null 2>&1; then exec xdg-terminal-exec; elif command -v gnome-terminal >/dev/null 2>&1; then exec gnome-terminal --working-directory=\"$d\"; elif command -v ptyxis >/dev/null 2>&1; then exec ptyxis --working-directory=\"$d\"; elif command -v konsole >/dev/null 2>&1; then exec konsole --workdir \"$d\"; elif command -v xfce4-terminal >/dev/null 2>&1; then exec xfce4-terminal --working-directory=\"$d\"; elif command -v mate-terminal >/dev/null 2>&1; then exec mate-terminal --working-directory=\"$d\"; elif command -v foot >/dev/null 2>&1; then exec foot -D \"$d\"; elif command -v kitty >/dev/null 2>&1; then exec kitty --directory \"$d\"; elif command -v alacritty >/dev/null 2>&1; then exec alacritty --working-directory \"$d\"; elif command -v wezterm >/dev/null 2>&1; then exec wezterm start --cwd \"$d\"; elif command -v ghostty >/dev/null 2>&1; then exec ghostty --working-directory=\"$d\"; elif command -v tilix >/dev/null 2>&1; then exec tilix --working-directory=\"$d\"; elif command -v terminator >/dev/null 2>&1; then exec terminator --working-directory=\"$d\"; elif command -v lxterminal >/dev/null 2>&1; then exec lxterminal --working-directory=\"$d\"; elif command -v urxvt >/dev/null 2>&1; then exec urxvt; elif command -v rxvt >/dev/null 2>&1; then exec rxvt; elif command -v xterm >/dev/null 2>&1; then exec xterm; fi' -- %f %d".into(),
            },
            CustomAction {
                id: "copy_path".into(),
                name: "Copy Path".into(),
                icon: "edit-copy".into(),
                target: ActionTarget::Any,
                mime_types: Vec::new(),
                extensions: Vec::new(),
                multiselection: false,
                command: "sh -c 'printf \"%s\" \"$1\" | (wl-copy 2>/dev/null || xclip -selection clipboard 2>/dev/null || xsel -b 2>/dev/null || true)' -- %f".into(),
            },
            CustomAction {
                id: "compute_sha256".into(),
                name: "Calculate SHA-256 Checksum".into(),
                icon: "document-properties".into(),
                target: ActionTarget::File,
                mime_types: Vec::new(),
                extensions: Vec::new(),
                multiselection: false,
                command: "sh -c 'h=$(sha256sum \"$1\" | awk \"{print \\$1}\"); printf \"%s\" \"$h\" | (wl-copy 2>/dev/null || xclip -selection clipboard 2>/dev/null || xsel -b 2>/dev/null || true); if command -v notify-send >/dev/null 2>&1; then notify-send \"SHA-256 Checksum\" \"$h\"; fi' -- %f".into(),
            },
            CustomAction {
                id: "verify_checksums".into(),
                name: "Verify Checksum File".into(),
                icon: "dialog-password".into(),
                target: ActionTarget::File,
                mime_types: Vec::new(),
                extensions: vec!["sha256".into(), "md5".into(), "sha512".into(), "sha1".into()],
                multiselection: false,
                command: "sh -c 'res=$(cd \"$(dirname \"$1\")\" && (sha256sum -c \"$1\" 2>&1 || md5sum -c \"$1\" 2>&1)); printf \"%s\\n\" \"$res\" | (wl-copy 2>/dev/null || xclip -selection clipboard 2>/dev/null || xsel -b 2>/dev/null || true); if command -v notify-send >/dev/null 2>&1; then notify-send \"Checksum Verification\" \"$res\"; fi' -- %f".into(),
            },
        ]
    }

    /// Find all actions matching the active directory and selected targets.
    #[must_use]
    pub fn matching_actions(&self, active_dir: &Path, targets: &[TargetItem]) -> Vec<&CustomAction> {
        self.actions.iter().filter(|a| a.matches(active_dir, targets)).collect()
    }

    /// Find a specific action by ID.
    #[must_use]
    pub fn get_action(&self, id: &str) -> Option<&CustomAction> {
        self.actions.iter().find(|a| a.id == id || (id == "terminal" && a.id == "open_terminal_here"))
    }

    /// Number of registered actions.
    #[must_use]
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// Whether any actions are registered.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_substitution_expands_correctly() {
        let dir = "/home/user/project_demo";
        let targets = vec![
            TargetItem {
                name: "main.rs".into(),
                path: PathBuf::from("/home/user/project_demo/main.rs"),
                uri: "file:///home/user/project_demo/main.rs".into(),
                is_dir: false,
                mime: Some("text/rust".into()),
            },
            TargetItem {
                name: "lib.rs".into(),
                path: PathBuf::from("/home/user/project_demo/lib.rs"),
                uri: "file:///home/user/project_demo/lib.rs".into(),
                is_dir: false,
                mime: Some("text/rust".into()),
            },
        ];

        let cmd1 = "alacritty --working-directory %d";
        assert_eq!(substitute_tokens(cmd1, dir, &targets), "alacritty --working-directory /home/user/project_demo");

        let cmd2 = "code %f";
        assert_eq!(substitute_tokens(cmd2, dir, &targets), "code /home/user/project_demo/main.rs");

        let cmd3 = "cat %F";
        assert_eq!(
            substitute_tokens(cmd3, dir, &targets),
            "cat /home/user/project_demo/main.rs /home/user/project_demo/lib.rs"
        );

        let cmd4 = "echo %u";
        assert_eq!(substitute_tokens(cmd4, dir, &targets), "echo file:///home/user/project_demo/main.rs");

        let cmd5 = "echo 100%% %d";
        assert_eq!(substitute_tokens(cmd5, dir, &targets), "echo 100% /home/user/project_demo");
    }

    #[test]
    fn token_substitution_shell_injection_protection() {
        let dir = "/home/user/my folder with spaces; rm -rf /;";
        let targets = vec![
            TargetItem {
                name: "evil;$(calc)'file.txt".into(),
                path: PathBuf::from("/home/user/evil;$(calc)'file.txt"),
                uri: "file:///home/user/evil;$(calc)'file.txt".into(),
                is_dir: false,
                mime: Some("text/plain".into()),
            },
            TargetItem {
                name: "normal file.txt".into(),
                path: PathBuf::from("/home/user/normal file.txt"),
                uri: "file:///home/user/normal file.txt".into(),
                is_dir: false,
                mime: Some("text/plain".into()),
            },
        ];

        let cmd = "cat %f";
        assert_eq!(substitute_tokens(cmd, dir, &targets), "cat '/home/user/evil;$(calc)'\\''file.txt'");

        let cmd_all = "ls %F";
        assert_eq!(
            substitute_tokens(cmd_all, dir, &targets),
            "ls '/home/user/evil;$(calc)'\\''file.txt' '/home/user/normal file.txt'"
        );

        let cmd_dir = "cd %d";
        assert_eq!(substitute_tokens(cmd_dir, dir, &targets), "cd '/home/user/my folder with spaces; rm -rf /;'");
    }

    #[test]
    fn action_target_and_extension_matching() {
        let dir = Path::new("/home/user/project_demo");
        let file_rs = TargetItem {
            name: "main.rs".into(),
            path: PathBuf::from("/home/user/project_demo/main.rs"),
            uri: "file:///home/user/project_demo/main.rs".into(),
            is_dir: false,
            mime: Some("text/rust".into()),
        };
        let file_tar = TargetItem {
            name: "archive.tar.gz".into(),
            path: PathBuf::from("/home/user/project_demo/archive.tar.gz"),
            uri: "file:///home/user/project_demo/archive.tar.gz".into(),
            is_dir: false,
            mime: Some("application/gzip".into()),
        };
        let subfolder = TargetItem {
            name: "src".into(),
            path: PathBuf::from("/home/user/project_demo/src"),
            uri: "file:///home/user/project_demo/src".into(),
            is_dir: true,
            mime: Some("inode/directory".into()),
        };

        let term_action = CustomAction {
            id: "term".into(),
            name: "Terminal".into(),
            icon: String::new(),
            target: ActionTarget::Directory,
            mime_types: Vec::new(),
            extensions: Vec::new(),
            multiselection: false,
            command: "xterm".into(),
        };

        // Term action matches empty selection (active dir background)
        assert!(term_action.matches(dir, &[]));
        // Term action matches folder item
        assert!(term_action.matches(dir, std::slice::from_ref(&subfolder)));
        // Term action does NOT match file
        assert!(!term_action.matches(dir, std::slice::from_ref(&file_rs)));

        let rust_action = CustomAction {
            id: "rust_fmt".into(),
            name: "Format Rust".into(),
            icon: String::new(),
            target: ActionTarget::File,
            mime_types: Vec::new(),
            extensions: vec!["rs".into()],
            multiselection: false,
            command: "rustfmt %f".into(),
        };

        assert!(rust_action.matches(dir, std::slice::from_ref(&file_rs)));
        assert!(!rust_action.matches(dir, std::slice::from_ref(&file_tar)));
        assert!(!rust_action.matches(dir, std::slice::from_ref(&subfolder)));
        assert!(!rust_action.matches(dir, &[]));

        let tar_action = CustomAction {
            id: "extract".into(),
            name: "Extract".into(),
            icon: String::new(),
            target: ActionTarget::File,
            mime_types: Vec::new(),
            extensions: vec!["tar.gz".into()],
            multiselection: false,
            command: "tar -xzf %f".into(),
        };

        assert!(tar_action.matches(dir, &[file_tar]));
        assert!(!tar_action.matches(dir, &[file_rs]));
    }

    #[test]
    fn action_toml_deserialization() {
        let toml_str = r#"
[action]
id = "open_terminal_here"
name = "Open Terminal Here"
icon = "utilities-terminal"
target = "directory"
mime_types = []
extensions = []
multiselection = false
command = "alacritty --working-directory %d"
"#;
        let parsed: ActionConfigFile = toml::from_str(toml_str).unwrap_or_else(|e| panic!("{e}"));
        match parsed {
            ActionConfigFile::Table { action } => {
                assert_eq!(action.id, "open_terminal_here");
                assert_eq!(action.target, ActionTarget::Directory);
                assert_eq!(action.command, "alacritty --working-directory %d");
            }
            _ => panic!("Expected table"),
        }
    }

    #[test]
    fn a_fifo_among_the_action_files_is_skipped_not_waited_on() {
        // Loaded in Controller::new: a FIFO named *.toml there kept the app from starting.
        let dir =
            std::env::temp_dir().join("gungnir-test-sandbox").join(format!("actions-fifo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        crate::session::tests::fifo(&dir.join("a.toml"));
        std::fs::write(dir.join("b.toml"), "id = \"real\"\nname = \"Real\"\ncommand = \"true %f\"\n")
            .unwrap_or_else(|e| panic!("{e}"));
        let d2 = dir.clone();
        let reg = crate::session::tests::bounded("actions", move || ActionRegistry::load_or_default(Some(&d2)));
        assert!(reg.get_action("real").is_some(), "the real action file still loads");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
