// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The Grimoire Authors

//! OCI content-addressed digests.
//!
//! Adapted from OCX `oci/digest.rs`. The hashing helpers here are
//! synchronous: Phase 1 has no async hash call sites, and the streamed
//! 64 KiB loop is cheap enough to run inline. A future phase that hashes
//! inside an async task should wrap [`Algorithm::hash_file`] in
//! `tokio::task::spawn_blocking`.

pub mod error;

use std::path::Path;

use serde::{Deserialize, Serialize};

use error::DigestError;

const DIGEST_SHORT_LEN: usize = 12;

/// Supported digest hash algorithms.
///
/// The single source of truth for the algorithm concept: every site that
/// hashes bytes or a file flows through [`Algorithm::hash`] or
/// [`Algorithm::hash_file`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Algorithm {
    Sha256,
    Sha384,
    Sha512,
}

impl Algorithm {
    /// Every supported algorithm, in parse/Display-prefix order.
    pub const ALL: &'static [Self] = &[Self::Sha256, Self::Sha384, Self::Sha512];

    /// OCI-spec algorithm prefix, e.g. `"sha256"`.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }

    /// Expected hex-string length for this algorithm's digest output.
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Sha256 => 64,
            Self::Sha384 => 96,
            Self::Sha512 => 128,
        }
    }

    /// Hashes `bytes` in memory with this algorithm.
    pub fn hash(self, bytes: impl AsRef<[u8]>) -> Digest {
        match self {
            Self::Sha256 => Digest::Sha256(hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes))),
            Self::Sha384 => Digest::Sha384(hex::encode(<sha2::Sha384 as sha2::Digest>::digest(bytes))),
            Self::Sha512 => Digest::Sha512(hex::encode(<sha2::Sha512 as sha2::Digest>::digest(bytes))),
        }
    }

    /// Streams the file at `path` through this algorithm in 64 KiB chunks,
    /// without loading the whole file into memory.
    ///
    /// # Errors
    ///
    /// Returns any I/O error from opening or reading the file.
    pub fn hash_file(self, path: &Path) -> std::io::Result<Digest> {
        let hex = match self {
            Self::Sha256 => hash_file_hex::<sha2::Sha256>(path)?,
            Self::Sha384 => hash_file_hex::<sha2::Sha384>(path)?,
            Self::Sha512 => hash_file_hex::<sha2::Sha512>(path)?,
        };
        Ok(match self {
            Self::Sha256 => Digest::Sha256(hex),
            Self::Sha384 => Digest::Sha384(hex),
            Self::Sha512 => Digest::Sha512(hex),
        })
    }
}

impl std::fmt::Display for Algorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.prefix())
    }
}

/// A parsed OCI content-addressed digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Digest {
    /// A SHA-256 digest, the most common type used in OCI.
    Sha256(String),
    /// A SHA-384 digest, less common but supported in OCI.
    Sha384(String),
    /// A SHA-512 digest, the least common but supported in OCI.
    Sha512(String),
}

impl Digest {
    /// Returns the algorithm prefix and hex string without allocating.
    pub fn parts(&self) -> (&str, &str) {
        (self.algorithm().prefix(), self.hex())
    }

    /// The algorithm that produced this digest.
    pub fn algorithm(&self) -> Algorithm {
        match self {
            Self::Sha256(_) => Algorithm::Sha256,
            Self::Sha384(_) => Algorithm::Sha384,
            Self::Sha512(_) => Algorithm::Sha512,
        }
    }

    /// Returns the hex string of the digest (without the algorithm prefix).
    pub fn hex(&self) -> &str {
        match self {
            Self::Sha256(hex) | Self::Sha384(hex) | Self::Sha512(hex) => hex,
        }
    }

    /// Returns the first [`DIGEST_SHORT_LEN`] hex characters for display.
    pub fn short_hex(&self) -> &str {
        &self.hex()[..DIGEST_SHORT_LEN]
    }

    /// Returns a truncated `algorithm:short_hex` string for display.
    pub fn to_short_string(&self) -> String {
        let (alg, hex) = self.parts();
        format!("{}:{}", alg, &hex[..DIGEST_SHORT_LEN])
    }
}

/// Streams a file through any `sha2::Digest` hasher in 64 KiB chunks,
/// returning the lowercase hex output. Shared across the
/// [`Algorithm::hash_file`] variants so the read-and-hash loop is not
/// triplicated.
fn hash_file_hex<H>(path: &Path) -> std::io::Result<String>
where
    H: sha2::Digest,
    sha2::digest::Output<H>: AsRef<[u8]>,
{
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut hasher = H::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (algorithm, hex) = self.parts();
        write!(f, "{algorithm}:{hex}")
    }
}

impl std::str::FromStr for Digest {
    type Err = DigestError;

    fn from_str(value: &str) -> Result<Self, DigestError> {
        Self::try_from(value)
    }
}

impl TryFrom<&str> for Digest {
    type Error = DigestError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        for algorithm in Algorithm::ALL {
            if let Some(hex) = value.strip_prefix(algorithm.prefix()).and_then(|s| s.strip_prefix(':')) {
                if hex.len() != algorithm.hex_len() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(DigestError::Invalid(value.to_owned()));
                }
                return Ok(match algorithm {
                    Algorithm::Sha256 => Self::Sha256(hex.to_string()),
                    Algorithm::Sha384 => Self::Sha384(hex.to_string()),
                    Algorithm::Sha512 => Self::Sha512(hex.to_string()),
                });
            }
        }
        Err(DigestError::Invalid(value.to_owned()))
    }
}

impl TryFrom<String> for Digest {
    type Error = DigestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl TryFrom<&String> for Digest {
    type Error = DigestError;

    fn try_from(value: &String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Self::try_from(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_per_algorithm() {
        let s256 = format!("sha256:{}", "a".repeat(64));
        assert!(matches!(Digest::try_from(s256.as_str()).unwrap(), Digest::Sha256(_)));

        let s384 = format!("sha384:{}", "b".repeat(96));
        assert!(matches!(Digest::try_from(s384.as_str()).unwrap(), Digest::Sha384(_)));

        let s512 = format!("sha512:{}", "c".repeat(128));
        assert!(matches!(Digest::try_from(s512.as_str()).unwrap(), Digest::Sha512(_)));
    }

    #[test]
    fn reject_unknown_algorithm() {
        let err = Digest::try_from("md5:abcdef").unwrap_err();
        assert!(matches!(err, DigestError::Invalid(_)));
    }

    #[test]
    fn reject_wrong_hex_length() {
        let err = Digest::try_from("sha256:abc").unwrap_err();
        assert!(matches!(err, DigestError::Invalid(_)));
    }

    #[test]
    fn reject_non_hex_charset() {
        let bad = format!("sha256:{}", "g".repeat(64));
        let err = Digest::try_from(bad.as_str()).unwrap_err();
        assert!(matches!(err, DigestError::Invalid(_)));
    }

    #[test]
    fn algorithm_all_round_trips_via_display() {
        let all = [
            Digest::Sha256("a".repeat(64)),
            Digest::Sha384("b".repeat(96)),
            Digest::Sha512("c".repeat(128)),
        ];
        for d in &all {
            assert!(Algorithm::ALL.contains(&d.algorithm()));
            let displayed = d.to_string();
            let parsed = Digest::try_from(displayed.as_str()).unwrap();
            assert_eq!(&parsed, d);
        }
        assert_eq!(Algorithm::ALL.len(), all.len());
    }

    #[test]
    fn hex_and_short_hex_accessors() {
        let hex = "43567c07f1a6b07b5e8dc052108c9d4c4a32130e18bcbd8a78c53af3e90325d9";
        let digest = Digest::Sha256(hex.to_string());
        assert_eq!(digest.hex(), hex);
        assert_eq!(digest.short_hex(), &hex[..DIGEST_SHORT_LEN]);
        assert_eq!(digest.to_short_string(), format!("sha256:{}", &hex[..DIGEST_SHORT_LEN]));
    }

    #[test]
    fn hash_file_agrees_with_hash_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, b"").unwrap();
        let from_file = Algorithm::Sha256.hash_file(&path).unwrap();
        assert_eq!(from_file, Algorithm::Sha256.hash([] as [u8; 0]));
    }

    #[test]
    fn hash_file_agrees_with_hash_small() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small");
        let data = b"hello world".to_vec();
        std::fs::write(&path, &data).unwrap();
        for alg in Algorithm::ALL {
            assert_eq!(alg.hash_file(&path).unwrap(), alg.hash(&data));
        }
    }

    #[test]
    fn hash_file_agrees_with_hash_multi_chunk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big");
        let data: Vec<u8> = (0..(64 * 1024 * 3 + 7)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        assert_eq!(
            Algorithm::Sha512.hash_file(&path).unwrap(),
            Algorithm::Sha512.hash(&data)
        );
    }

    #[test]
    fn serde_round_trip() {
        let digest = Digest::Sha256("a".repeat(64));
        let json = serde_json::to_string(&digest).unwrap();
        assert_eq!(json, format!("\"sha256:{}\"", "a".repeat(64)));
        let back: Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, digest);
    }

    #[test]
    fn from_str_parses() {
        let s = format!("sha256:{}", "f".repeat(64));
        let digest: Digest = s.parse().unwrap();
        assert!(matches!(digest, Digest::Sha256(_)));
    }
}
