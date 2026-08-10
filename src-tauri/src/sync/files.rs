use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkDescriptor {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileManifest {
    pub schema_version: u8,
    pub file_sha256: String,
    pub size: u64,
    pub chunk_size: u32,
    pub chunks: Vec<ChunkDescriptor>,
}

pub fn manifest(path: &Path, chunk_size: usize) -> io::Result<FileManifest> {
    if chunk_size == 0 || chunk_size > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid chunk size",
        ));
    }
    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; chunk_size];
    let mut file_hasher = Sha256::new();
    let mut chunks = Vec::new();
    let mut total = 0_u64;

    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let bytes = &buffer[..read];
        file_hasher.update(bytes);
        chunks.push(ChunkDescriptor {
            sha256: sha256_hex(bytes),
            size: read as u64,
        });
        total = total.saturating_add(read as u64);
    }

    Ok(FileManifest {
        schema_version: 1,
        file_sha256: hex(&file_hasher.finalize()),
        size: total,
        chunk_size: chunk_size as u32,
        chunks,
    })
}

pub fn verify_chunk(expected_hash: &str, bytes: &[u8]) -> bool {
    constant_time_eq(expected_hash.as_bytes(), sha256_hex(bytes).as_bytes())
}

pub fn verify_reconstructed(manifest: &FileManifest, bytes: &[u8]) -> bool {
    manifest.size == bytes.len() as u64 && verify_chunk(&manifest.file_sha256, bytes)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn manifest_supports_resume_and_integrity_checks() {
        let path = std::env::temp_dir().join(format!(
            "clippy-sync-file-{}-{}",
            std::process::id(),
            crate::db::now_ms()
        ));
        let mut file = File::create(&path).unwrap();
        file.write_all(b"abcdefghij").unwrap();
        drop(file);

        let result = manifest(&path, 4).unwrap();
        assert_eq!(result.size, 10);
        assert_eq!(result.chunks.len(), 3);
        assert!(verify_chunk(&result.chunks[0].sha256, b"abcd"));
        assert!(!verify_chunk(&result.chunks[0].sha256, b"abce"));
        assert!(verify_reconstructed(&result, b"abcdefghij"));

        std::fs::remove_file(path).unwrap();
    }
}
