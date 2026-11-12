//! Recent files (`recently-used.xbel` in the user's data directory).
//! Provides parsing, atomic removal and clearing of GTK/freedesktop recent bookmarks.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{fs::OpenOptionsExt, io::AsRawFd},
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub const XBEL_MAX: u64 = 32 << 20;
const LOCK_WAIT: Duration = Duration::from_secs(1);

/// One bookmark entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub uri: Vec<u8>,
    pub range: (usize, usize),
}

fn attr<'a>(tag: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut rest = tag;
    loop {
        rest = rest.trim_ascii_start();
        let eq = rest.iter().position(|&b| b == b'=')?;
        let key = rest[..eq].trim_ascii_end();
        rest = rest[eq + 1..].trim_ascii_start();
        let (&quote, body) = rest.split_first()?;
        if quote != b'"' && quote != b'\'' {
            return None;
        }
        let end = body.iter().position(|&b| b == quote)?;
        if key == name {
            return Some(&body[..end]);
        }
        rest = &body[end + 1..];
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn xml_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    'outer: while i < s.len() {
        for (ent, ch) in
            [(&b"&amp;"[..], b'&'), (b"&lt;", b'<'), (b"&gt;", b'>'), (b"&quot;", b'"'), (b"&apos;", b'\'')]
        {
            if s[i..].starts_with(ent) {
                out.push(ch);
                i += ent.len();
                continue 'outer;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

fn tag_end(text: &[u8]) -> Option<usize> {
    let mut quote = None;
    for (i, &b) in text.iter().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i),
            None => {}
        }
    }
    None
}

fn next_tag(text: &[u8], mut from: usize) -> Option<(usize, usize)> {
    loop {
        let at = from + find(text.get(from..)?, b"<")?;
        let rest = &text[at..];
        let close: Option<&[u8]> = if rest.starts_with(b"<!--") {
            Some(b"-->")
        } else if rest.starts_with(b"<![CDATA[") {
            Some(b"]]>")
        } else if rest.starts_with(b"<?") {
            Some(b"?>")
        } else {
            None
        };
        match close {
            Some(c) => from = at + find(rest, c)? + c.len(),
            None => return Some((at, at + tag_end(rest)? + 1)),
        }
    }
}

fn tag_is(tag: &[u8], name: &[u8]) -> bool {
    tag.strip_prefix(name).and_then(|r| r.first()).is_some_and(|&b| b.is_ascii_whitespace() || b == b'>' || b == b'/')
}

#[must_use]
pub fn parse(text: &[u8]) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some((start, close)) = next_tag(text, i) {
        i = close;
        if !tag_is(&text[start..close], b"<bookmark") {
            continue;
        }
        let tag = &text[start + 9..close - 1];
        let end = if tag.last() == Some(&b'/') {
            close
        } else {
            let mut from = close;
            loop {
                let Some((a, b)) = next_tag(text, from) else {
                    return out;
                };
                if tag_is(&text[a..b], b"</bookmark") {
                    break b;
                }
                from = b;
            }
        };
        if let Some(href) = attr(tag, b"href") {
            out.push(Entry { uri: xml_decode(href), range: (start, end) });
        }
        i = end;
    }
    out
}

#[must_use]
pub fn file_path(uri: &[u8]) -> Option<Vec<u8>> {
    let rest = uri.strip_prefix(b"file://")?;
    if rest.first() != Some(&b'/') {
        return None;
    }
    let mut out = Vec::with_capacity(rest.len());
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == b'%' {
            let h = rest.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(rest[i]);
            i += 1;
        }
    }
    (!out.contains(&0)).then_some(out)
}

pub fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().as_chunks::<2>().0 {
        let hex_str = std::str::from_utf8(chunk).ok()?;
        let byte = u8::from_str_radix(hex_str, 16).ok()?;
        out.push(byte);
    }
    Some(out)
}

pub fn load(path: &Path) -> io::Result<Vec<u8>> {
    let f = match fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY).open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        r => r?,
    };
    if !f.metadata()?.is_file() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    let mut out = Vec::new();
    f.take(XBEL_MAX + 1).read_to_end(&mut out)?;
    if out.len() as u64 > XBEL_MAX {
        return Err(io::Error::from_raw_os_error(libc::EFBIG));
    }
    Ok(out)
}

#[derive(Debug)]
pub enum ForgetError {
    Busy,
    NotFound,
    Io(io::Error),
}

impl From<io::Error> for ForgetError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

fn with_lock<R>(path: &Path, f: impl FnOnce() -> Result<R, ForgetError>) -> Result<R, ForgetError> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(&lock_path)?;
    let t0 = Instant::now();
    loop {
        // SAFETY: valid fd; LOCK_NB never blocks.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        if t0.elapsed() >= LOCK_WAIT {
            return Err(ForgetError::Busy);
        }
        thread::sleep(Duration::from_millis(10));
    }
    f()
}

fn write_atomic(path: &Path, out: &[u8]) -> io::Result<()> {
    // A link (a dotfiles manager's) is followed and its target replaced, beside it: the
    // rename used to replace the link with a plain file, and the real list stopped
    // changing. A dangling link is an error, never turned into a file.
    let target = match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let path = target.as_path();
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".gungnir-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let r = (|| -> io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(out)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

pub fn forget(path: &Path, uri: &[u8]) -> Result<(), ForgetError> {
    with_lock(path, || {
        let text = load(path)?;
        let hits: Vec<_> = parse(&text).into_iter().filter(|e| e.uri == uri).collect();
        if hits.is_empty() {
            return Err(ForgetError::NotFound);
        }
        let mut out = Vec::with_capacity(text.len());
        let mut at = 0;
        for e in &hits {
            out.extend_from_slice(&text[at..e.range.0]);
            at = e.range.1;
        }
        out.extend_from_slice(&text[at..]);
        Ok(write_atomic(path, &out)?)
    })
}

pub fn clear(path: &Path) -> Result<(), ForgetError> {
    with_lock(path, || {
        let text = load(path)?;
        if text.is_empty() {
            return Ok(());
        }
        let hits = parse(&text);
        if hits.is_empty() {
            return Ok(());
        }
        let mut out = Vec::with_capacity(text.len());
        let mut at = 0;
        for e in &hits {
            out.extend_from_slice(&text[at..e.range.0]);
            at = e.range.1;
        }
        out.extend_from_slice(&text[at..]);
        Ok(write_atomic(path, &out)?)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const XBEL: &str = "<?xml version=\"1.0\"?>\n<xbel version=\"1.0\">\n<bookmark href=\"file:///home/u/a.txt\" added=\"x\"/>\n<bookmark href=\"file:///home/u/b.txt\" added=\"y\"></bookmark>\n</xbel>\n";

    fn sandbox(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join("gungnir-test-sandbox").join(format!("recent-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("real")).unwrap();
        p
    }

    #[test]
    fn forgetting_and_clearing_edit_only_the_bookmarks() {
        let d = sandbox("plain");
        let p = d.join("recently-used.xbel");
        fs::write(&p, XBEL).unwrap();
        forget(&p, b"file:///home/u/a.txt").unwrap();
        let left = fs::read(&p).unwrap();
        assert_eq!(parse(&left).iter().map(|e| e.uri.clone()).collect::<Vec<_>>(), [b"file:///home/u/b.txt".to_vec()]);
        clear(&p).unwrap();
        let left = fs::read_to_string(&p).unwrap();
        assert!(parse(left.as_bytes()).is_empty() && left.contains("<xbel version") && left.contains("</xbel>"));
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_symlinked_list_stays_a_link_and_its_target_is_rewritten() {
        // The temp was renamed over the path: a linked recently-used.xbel (a dotfiles
        // manager's) became a plain file, and the real list never changed again.
        let d = sandbox("link");
        let (link, real) = (d.join("recently-used.xbel"), d.join("real/recently-used.xbel"));
        fs::write(&real, XBEL).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        forget(&link, b"file:///home/u/a.txt").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link was replaced");
        assert_eq!(parse(&fs::read(&real).unwrap()).len(), 1, "the real list did not change");
        clear(&link).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(parse(&fs::read(&real).unwrap()).is_empty());
        let _ = fs::remove_dir_all(d);
    }
}
