//! Cryptographic hash computation and checksum verification for files.
//!
//! Streams MD5, SHA-1, SHA-256, and SHA-512 in a single pass over file contents,
//! supporting verification against user-provided hashes or checksum files.

use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use sha2::Digest;
use sha2::{Sha256, Sha512};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileHashes {
    pub file_name: String,
    pub file_size: u64,
    pub md5: String,
    pub sha1: String,
    pub sha256: String,
    pub sha512: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    #[must_use]
    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
            Self::Sha512 => "SHA-512",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashMatch {
    Empty,
    Matched(HashAlgorithm),
    Mismatch,
}

/// Computes MD5, SHA-1, SHA-256, and SHA-512 concurrently in a single streaming pass.
pub fn compute_hashes(path: &Path) -> Result<FileHashes, std::io::Error> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let file_size = metadata.len();
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut md5_ctx = md5::Context::new();
    let mut sha1_ctx = sha1::Sha1::new();
    let mut sha256_ctx = Sha256::new();
    let mut sha512_ctx = Sha512::new();

    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let chunk = &buffer[..n];
        md5_ctx.consume(chunk);
        sha1_ctx.update(chunk);
        sha256_ctx.update(chunk);
        sha512_ctx.update(chunk);
    }

    let md5_digest = md5_ctx.compute();
    let sha1_digest = sha1_ctx.finalize();
    let sha256_digest = sha256_ctx.finalize();
    let sha512_digest = sha512_ctx.finalize();

    Ok(FileHashes {
        file_name,
        file_size,
        md5: format!("{md5_digest:x}"),
        sha1: format!("{sha1_digest:x}"),
        sha256: format!("{sha256_digest:x}"),
        sha512: format!("{sha512_digest:x}"),
    })
}

/// Compares user-supplied expected hash against the computed hashes.
#[must_use]
pub fn verify_hash(hashes: &FileHashes, expected: &str) -> HashMatch {
    let clean = expected.trim().to_ascii_lowercase();
    if clean.is_empty() {
        return HashMatch::Empty;
    }

    if clean == hashes.sha256 {
        HashMatch::Matched(HashAlgorithm::Sha256)
    } else if clean == hashes.sha512 {
        HashMatch::Matched(HashAlgorithm::Sha512)
    } else if clean == hashes.md5 {
        HashMatch::Matched(HashAlgorithm::Md5)
    } else if clean == hashes.sha1 {
        HashMatch::Matched(HashAlgorithm::Sha1)
    } else {
        HashMatch::Mismatch
    }
}

/// Verifies a checksum file (like `SHA256SUMS` or `file.sha256`) against files in the same directory.
pub fn verify_checksum_file(path: &Path) -> Result<Vec<(String, bool)>, std::io::Error> {
    let file = File::open(path)?;
    let parent = path.parent().unwrap_or(Path::new("."));
    let reader = BufReader::new(file);
    let mut results = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Common format: "<hash>  <filename>" or "<hash> *<filename>"
        let mut parts = trimmed.split_whitespace();
        let Some(hash_str) = parts.next() else { continue };
        let rest = trimmed[hash_str.len()..].trim_start();
        let target_name = rest.trim_start_matches('*').trim();
        if target_name.is_empty() {
            continue;
        }

        let target_path = parent.join(target_name);
        if !target_path.is_file() {
            results.push((target_name.to_string(), false));
            continue;
        }

        if let Ok(hashes) = compute_hashes(&target_path) {
            let matched = matches!(verify_hash(&hashes, hash_str), HashMatch::Matched(_));
            results.push((target_name.to_string(), matched));
        } else {
            results.push((target_name.to_string(), false));
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_compute_hashes() {
        let sandbox = std::env::temp_dir().join("gungnir-test-sandbox").join("hash_test");
        let _ = std::fs::create_dir_all(&sandbox);
        let test_file = sandbox.join("hello.txt");

        let mut f = File::create(&test_file).unwrap_or_else(|e| panic!("{e}"));
        f.write_all(b"Hello Gungnir FM\n").unwrap_or_else(|e| panic!("{e}"));
        drop(f);

        let hashes = compute_hashes(&test_file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(hashes.file_name, "hello.txt");
        assert_eq!(hashes.file_size, 17);

        // Verification matching
        assert_eq!(verify_hash(&hashes, &hashes.sha256), HashMatch::Matched(HashAlgorithm::Sha256));
        assert_eq!(verify_hash(&hashes, &hashes.md5), HashMatch::Matched(HashAlgorithm::Md5));
        assert_eq!(verify_hash(&hashes, &hashes.sha1), HashMatch::Matched(HashAlgorithm::Sha1));
        assert_eq!(verify_hash(&hashes, &hashes.sha512), HashMatch::Matched(HashAlgorithm::Sha512));
        assert_eq!(verify_hash(&hashes, "wrong_hash"), HashMatch::Mismatch);
        assert_eq!(verify_hash(&hashes, "   "), HashMatch::Empty);

        // Checksum file test
        let sums_file = sandbox.join("SHA256SUMS");
        let mut sf = File::create(&sums_file).unwrap_or_else(|e| panic!("{e}"));
        writeln!(sf, "{}  hello.txt", hashes.sha256).unwrap_or_else(|e| panic!("{e}"));
        drop(sf);

        let sum_results = verify_checksum_file(&sums_file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sum_results.len(), 1);
        assert_eq!(sum_results[0], ("hello.txt".to_string(), true));

        // Cleanup
        let _ = std::fs::remove_dir_all(&sandbox);
    }
}
