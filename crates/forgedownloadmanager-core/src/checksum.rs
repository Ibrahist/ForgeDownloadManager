use std::fs::File;
use std::io::Read;
use std::path::Path;

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::error::Error;
use crate::model::ChecksumAlgo;
use crate::Result;

pub fn hash_file(path: &Path, algo: ChecksumAlgo) -> Result<String> {
    let mut file = File::open(path)?;
    let mut buf = [0u8; 64 * 1024];
    match algo {
        ChecksumAlgo::Sha256 => {
            let mut hasher = Sha256::new();
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        ChecksumAlgo::Sha1 => {
            let mut hasher = Sha1::new();
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
        ChecksumAlgo::Md5 => {
            let mut hasher = md5::Md5::new();
            loop {
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(hex::encode(hasher.finalize()))
        }
    }
}

pub fn hashes_match(expected: &str, actual: &str) -> bool {
    expected.trim().eq_ignore_ascii_case(actual.trim())
}

pub fn random_token() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|err| Error::Failed(format!("rng: {err}")))?;
    Ok(hex::encode(buf))
}
