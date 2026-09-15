use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

const PARTIAL_HASH_BYTES: usize = 64 * 1024;
const READ_BUF_SIZE: usize = 256 * 1024;

/// Hashes only the first `PARTIAL_HASH_BYTES` of the file — cheap pre-filter to
/// eliminate same-size files that clearly differ before paying for a full read.
pub fn partial_hash(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; PARTIAL_HASH_BYTES];
    let mut total = 0;
    while total < buf.len() {
        let n = file.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    buf.truncate(total);
    Ok(*blake3::hash(&buf).as_bytes())
}

pub fn full_hash(path: &Path) -> io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; READ_BUF_SIZE];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}
