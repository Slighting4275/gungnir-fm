//! Bounded discovery of regular-file document templates. No template is executed.

use core_vfs::VfsUri;
use std::path::Path;

const MAX_ENTRIES: usize = 10_000;
const MAX_TEMPLATES: usize = 128;
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub source: VfsUri,
    pub title: String,
    pub name: String,
}

pub fn scan(root: &Path) -> Vec<Template> {
    let mut stack = vec![(root.to_path_buf(), 0)];
    let mut examined = 0;
    let mut templates = Vec::new();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = entries.take(MAX_ENTRIES - examined).filter_map(Result::ok).collect();
        examined += entries.len();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() && depth < MAX_DEPTH {
                stack.push((path, depth + 1));
            } else if kind.is_file() {
                let Ok(source) = VfsUri::from_local_path(&path) else { continue };
                let Some(title) = path.strip_prefix(root).ok().and_then(Path::to_str) else { continue };
                templates.push(Template { source, title: title.into(), name });
                if templates.len() == MAX_TEMPLATES {
                    break;
                }
            }
        }
        if examined >= MAX_ENTRIES || templates.len() == MAX_TEMPLATES {
            break;
        }
    }
    templates.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then(a.title.cmp(&b.title)));
    templates
}

pub fn numbered_name(name: &str, number: usize) -> String {
    if number == 1 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} {number}.{extension}"),
        _ => format!("{name} {number}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("gungnir-templates-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap_or_else(|e| panic!("{e}"));
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn discovery_skips_hidden_files_symlinks_and_cycles_and_labels_subfolders() {
        let f = Fixture::new("discovery");
        std::fs::create_dir(f.0.join("Office")).unwrap_or_else(|e| panic!("{e}"));
        for name in ["Office/Letter.odt", "Notes.txt", ".hidden"] {
            std::fs::write(f.0.join(name), b"template").unwrap_or_else(|e| panic!("{e}"));
        }
        std::os::unix::fs::symlink(&f.0, f.0.join("loop")).unwrap_or_else(|e| panic!("{e}"));
        std::os::unix::fs::symlink("Notes.txt", f.0.join("linked.txt")).unwrap_or_else(|e| panic!("{e}"));
        let found = scan(&f.0);
        assert_eq!(found.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Notes.txt", "Office/Letter.odt"]);
        assert_eq!(found[1].name, "Letter.odt");
    }
    #[test]
    fn discovery_is_bounded_and_missing_directories_are_empty() {
        let f = Fixture::new("bounded");
        for n in 0..200 {
            std::fs::write(f.0.join(format!("Template{n}.txt")), b"x").unwrap_or_else(|e| panic!("{e}"));
        }
        assert_eq!(scan(&f.0).len(), MAX_TEMPLATES);
        assert!(scan(&f.0.join("missing")).is_empty());
    }
    #[test]
    fn numbered_documents_keep_extensions_and_significant_spaces() {
        assert_eq!(numbered_name("Letter.odt", 1), "Letter.odt");
        assert_eq!(numbered_name("Letter.odt", 2), "Letter 2.odt");
        assert_eq!(numbered_name("  Letter .odt", 3), "  Letter  3.odt");
        assert_eq!(numbered_name("README", 2), "README 2");
        assert_eq!(numbered_name(".profile", 2), ".profile 2");
    }
    #[test]
    fn discovery_stops_at_the_directory_depth_limit() {
        let f = Fixture::new("depth");
        let mut path = f.0.clone();
        for depth in 1..=MAX_DEPTH + 2 {
            path.push("nested");
            std::fs::create_dir(&path).unwrap_or_else(|e| panic!("{e}"));
            if depth == MAX_DEPTH {
                std::fs::write(path.join("Visible.txt"), b"visible").unwrap_or_else(|e| panic!("{e}"));
            }
        }
        std::fs::write(path.join("Too deep.txt"), b"hidden by limit").unwrap_or_else(|e| panic!("{e}"));
        let found = scan(&f.0);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Visible.txt");
    }
}
