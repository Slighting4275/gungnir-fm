//! `file://` and `gungnir://` URIs.
//!
//! - `file:///home/user/x`, `file://localhost/x` or a bare absolute path `/home/user/x`.
//! - `gungnir://<node>/<share>/...`: a Gungnir location. The live gungnird (protocol v2)
//!   serves this machine only, so the live backend accepts an empty node or `localhost`
//!   (`gungnir:///home/user/x`); the mock backend invents nodes such as `odin`.
//!
//! The path is kept absolute and normalized: no empty, `.` or `..` segments and no
//! trailing slash except for the root. Percent escapes are decoded when parsing a
//! `scheme://` URI (not a bare path) and re-encoded by [`VfsUri::to_uri_string`].

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{VfsError, VfsResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scheme {
    File,
    Gungnir,
    Sftp,
    Smb,
    Trash,
    Recent,
}

impl Scheme {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::File => "file",
            Scheme::Gungnir => "gungnir",
            Scheme::Sftp => "sftp",
            Scheme::Smb => "smb",
            Scheme::Trash => "trash",
            Scheme::Recent => "recent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VfsUri {
    scheme: Scheme,
    authority: String,
    path: String,
}

impl VfsUri {
    /// Parse user input or a stored URI.
    ///
    /// # Errors
    /// [`VfsError::InvalidUri`] for relative paths, unknown schemes, bad escapes or a
    /// `file://` host other than `localhost`.
    pub fn parse(input: &str) -> VfsResult<Self> {
        let s = input.trim();
        if s.starts_with('/') {
            return Ok(Self { scheme: Scheme::File, authority: String::new(), path: normalize(s)? });
        }
        let Some((scheme, rest)) = s.split_once("://") else {
            return Err(VfsError::InvalidUri(format!("{s}: not an absolute path or URI")));
        };
        let scheme = match scheme.to_ascii_lowercase().as_str() {
            "file" => Scheme::File,
            "gungnir" => Scheme::Gungnir,
            "sftp" | "ssh" => Scheme::Sftp,
            "smb" | "cifs" => Scheme::Smb,
            "trash" => Scheme::Trash,
            "recent" => Scheme::Recent,
            other => return Err(VfsError::InvalidUri(format!("unsupported scheme {other}://"))),
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let authority = percent_decode(authority)?;
        if authority.contains(['/', '@', ':']) && scheme == Scheme::Gungnir {
            return Err(VfsError::InvalidUri(format!("bad gungnir node {authority:?}")));
        }
        let authority = match scheme {
            Scheme::File if authority.is_empty() || authority.eq_ignore_ascii_case("localhost") => String::new(),
            Scheme::File => {
                return Err(VfsError::InvalidUri(format!("file:// host {authority} is not this machine")));
            }
            Scheme::Gungnir => authority.to_ascii_lowercase(),
            Scheme::Sftp | Scheme::Smb => {
                if authority.is_empty() {
                    return Err(VfsError::InvalidUri(format!("{}:// missing host or share", scheme.as_str())));
                }
                authority
            }
            Scheme::Trash | Scheme::Recent => {
                if !authority.is_empty() && !authority.eq_ignore_ascii_case("localhost") {
                    return Err(VfsError::InvalidUri(format!("{}:// does not take a host", scheme.as_str())));
                }
                String::new()
            }
        };
        Ok(Self { scheme, authority, path: normalize(&percent_decode(path)?)? })
    }

    /// A `file://` URI for an absolute local path.
    ///
    /// # Errors
    /// [`VfsError::InvalidUri`] if `p` is relative or not UTF-8.
    pub fn from_local_path(p: &Path) -> VfsResult<Self> {
        let s = p.to_str().ok_or_else(|| VfsError::InvalidUri(format!("{} is not UTF-8", p.display())))?;
        if !s.starts_with('/') {
            return Err(VfsError::InvalidUri(format!("{s} is not absolute")));
        }
        Ok(Self { scheme: Scheme::File, authority: String::new(), path: normalize(s)? })
    }

    /// `gungnir://<node><path>`.
    ///
    /// # Errors
    /// [`VfsError::InvalidUri`] for a relative path.
    pub fn gungnir(node: &str, path: &str) -> VfsResult<Self> {
        Self::parse(&format!(
            "gungnir://{node}{}",
            if path.starts_with('/') { path.to_owned() } else { format!("/{path}") }
        ))
    }

    /// `trash://<path>`.
    ///
    /// # Errors
    /// [`VfsError::InvalidUri`].
    pub fn trash(path: &str) -> VfsResult<Self> {
        let p = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
        Ok(Self { scheme: Scheme::Trash, authority: String::new(), path: normalize(&p)? })
    }

    /// `recent://<path>`.
    ///
    /// # Errors
    /// [`VfsError::InvalidUri`].
    pub fn recent(path: &str) -> VfsResult<Self> {
        let p = if path.starts_with('/') { path.to_owned() } else { format!("/{path}") };
        Ok(Self { scheme: Scheme::Recent, authority: String::new(), path: normalize(&p)? })
    }

    #[must_use]
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// The node of a `gungnir://` URI (empty for `file://` and the local gungnird).
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// The absolute, normalized path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.path == "/"
    }

    /// The local filesystem path of a `file://` URI.
    #[must_use]
    pub fn to_local_path(&self) -> Option<PathBuf> {
        (self.scheme == Scheme::File).then(|| PathBuf::from(&self.path))
    }

    /// The last path segment (`None` for the root).
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() { None } else { self.path.rsplit('/').next() }
    }

    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        let cut = self.path.rfind('/').unwrap_or(0);
        let path = if cut == 0 { "/".to_owned() } else { self.path[..cut].to_owned() };
        Some(Self { path, ..self.clone() })
    }

    /// A child of this directory.
    ///
    /// # Errors
    /// [`VfsError::InvalidUri`] if `name` is empty, `.`, `..`, or contains `/` or NUL.
    pub fn join(&self, name: &str) -> VfsResult<Self> {
        check_name(name)?;
        let path = if self.is_root() { format!("/{name}") } else { format!("{}/{name}", self.path) };
        Ok(Self { path, ..self.clone() })
    }

    /// True if `self` is `other` or below it (same scheme and node).
    #[must_use]
    pub fn starts_with(&self, other: &Self) -> bool {
        self.scheme == other.scheme
            && self.authority == other.authority
            && (other.is_root()
                || self.path == other.path
                || self.path.strip_prefix(&other.path).is_some_and(|r| r.starts_with('/')))
    }

    /// Breadcrumbs from the root down: `(label, uri)`.
    #[must_use]
    pub fn crumbs(&self) -> Vec<(String, Self)> {
        let root_label = match self.scheme {
            Scheme::File => "/".to_owned(),
            Scheme::Gungnir if self.authority.is_empty() => "gungnir://".to_owned(),
            Scheme::Gungnir => format!("gungnir://{}", self.authority),
            Scheme::Sftp => format!("sftp://{}", self.authority),
            Scheme::Smb => format!("smb://{}", self.authority),
            Scheme::Trash => "trash://".to_owned(),
            Scheme::Recent => "recent://".to_owned(),
        };
        let mut cur = Self { path: "/".into(), ..self.clone() };
        let mut out = vec![(root_label, cur.clone())];
        for seg in self.path.split('/').filter(|s| !s.is_empty()) {
            cur = Self { path: if cur.is_root() { format!("/{seg}") } else { format!("{}/{seg}", cur.path) }, ..cur };
            out.push((seg.to_owned(), cur.clone()));
        }
        out
    }

    /// What the path bar shows: the plain path for local files, the URI otherwise.
    #[must_use]
    pub fn display(&self) -> String {
        match self.scheme {
            Scheme::File => self.path.clone(),
            Scheme::Gungnir => format!("gungnir://{}{}", self.authority, self.path),
            Scheme::Sftp => format!("sftp://{}{}", self.authority, self.path),
            Scheme::Smb => format!("smb://{}{}", self.authority, self.path),
            Scheme::Trash => format!("trash://{}", self.path),
            Scheme::Recent => format!("recent://{}", self.path),
        }
    }

    /// The canonical, percent-encoded URI.
    #[must_use]
    pub fn to_uri_string(&self) -> String {
        format!("{}://{}{}", self.scheme.as_str(), self.authority, percent_encode(&self.path))
    }
}

impl fmt::Display for VfsUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// A single file name: non-empty, not `.`/`..`, no `/` or NUL.
///
/// # Errors
/// [`VfsError::InvalidUri`].
pub fn check_name(name: &str) -> VfsResult<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(VfsError::InvalidUri(format!("{name:?} is not a valid file name")));
    }
    Ok(())
}

fn normalize(p: &str) -> VfsResult<String> {
    if p.contains('\0') {
        return Err(VfsError::InvalidUri("path contains NUL".into()));
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}

fn percent_decode(s: &str) -> VfsResult<String> {
    if !s.contains('%') {
        return Ok(s.to_owned());
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| VfsError::InvalidUri(format!("bad percent escape in {s}")))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| VfsError::InvalidUri(format!("{s} decodes to invalid UTF-8")))
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&c) {
            out.push(char::from(c));
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> VfsUri {
        VfsUri::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn local_forms_agree() {
        let a = u("/home/user/Music");
        assert_eq!(a, u("file:///home/user/Music"));
        assert_eq!(a, u("file://localhost/home/user/./Music/"));
        assert_eq!(a, u("/home//user/x/../Music"));
        assert_eq!(a.to_local_path(), Some(PathBuf::from("/home/user/Music")));
        assert_eq!(a.display(), "/home/user/Music");
    }

    #[test]
    fn gungnir_uris() {
        let g = u("gungnir://Odin/asgard/runes/");
        assert_eq!(g.scheme(), Scheme::Gungnir);
        assert_eq!(g.authority(), "odin");
        assert_eq!(g.path(), "/asgard/runes");
        assert_eq!(g.display(), "gungnir://odin/asgard/runes");
        assert_eq!(u("gungnir://odin").path(), "/");
        assert_eq!(u("gungnir:///home").authority(), "");
        assert!(g.to_local_path().is_none());
    }

    #[test]
    fn sftp_and_smb_uris() {
        let s = u("sftp://user@example.com/srv/data");
        assert_eq!(s.scheme(), Scheme::Sftp);
        assert_eq!(s.authority(), "user@example.com");
        assert_eq!(s.path(), "/srv/data");
        assert_eq!(s.display(), "sftp://user@example.com/srv/data");

        let ssh = u("ssh://server/home/remote");
        assert_eq!(ssh.scheme(), Scheme::Sftp);
        assert_eq!(ssh.authority(), "server");

        let m = u("smb://nas.local/share/docs");
        assert_eq!(m.scheme(), Scheme::Smb);
        assert_eq!(m.authority(), "nas.local");
        assert_eq!(m.path(), "/share/docs");
        assert_eq!(m.display(), "smb://nas.local/share/docs");

        let cifs = u("cifs://winserver/public");
        assert_eq!(cifs.scheme(), Scheme::Smb);
    }

    #[test]
    fn rejects_bad_input() {
        for bad in ["relative/x", "ftp://host/share", "file://evil/etc", "gungnir://a/%zz", "gungnir://u@h/x"] {
            assert!(VfsUri::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn percent_round_trip() {
        let a = u("file:///tmp/a%20b/%C3%A6");
        assert_eq!(a.path(), "/tmp/a b/æ");
        assert_eq!(a.to_uri_string(), "file:///tmp/a%20b/%C3%A6");
        assert_eq!(u(&a.to_uri_string()), a);
    }

    #[test]
    fn navigation() {
        let a = u("/home/user");
        assert_eq!(a.parent(), Some(u("/home")));
        assert_eq!(u("/home").parent(), Some(u("/")));
        assert_eq!(u("/").parent(), None);
        assert_eq!(a.join("x").map(|j| j.path().to_owned()), Ok("/home/user/x".to_owned()));
        assert_eq!(u("/").join("etc"), Ok(u("/etc")));
        assert!(a.join("..").is_err() && a.join("a/b").is_err() && a.join("").is_err());
        assert_eq!(a.file_name(), Some("user"));
        assert!(u("/home/user/x").starts_with(&a));
        assert!(!u("/home/userel").starts_with(&a));
        assert!(a.starts_with(&u("/")));
        let crumbs: Vec<String> = u("gungnir://odin/asgard/runes").crumbs().into_iter().map(|c| c.0).collect();
        assert_eq!(crumbs, ["gungnir://odin", "asgard", "runes"]);
    }
}
