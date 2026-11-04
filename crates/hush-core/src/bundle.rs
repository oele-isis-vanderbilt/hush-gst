//! Model bundle: hash a tar.gz file, then load it with the upstream loader.
//!
//! Flow: read file -> SHA-256 (compare if an expected hash was given) ->
//! `DfParams::new(path)` -> build the shared tract model.
//!
//! Note: the upstream loader opens the file itself, so the hashed bytes and the
//! parsed bytes come from two reads of the same file. Fine for a read-only app
//! resource. Revisit (hash and parse one buffer) if the file can change at runtime.
//!
//! The vendored crate returns `anyhow` errors; they are converted into our own
//! `Error` at this boundary, so `anyhow` never appears in our API.

use std::path::Path;
use std::sync::Arc;

use df::tract::{DfModelShared, DfParams, DfTract};
use sha2::{Digest, Sha256};

use crate::constants::{N_CH, REDUCE_MASK, runtime_params};
use crate::error::Error;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelManifest {
    pub id: String,
    pub name: String,
    pub description: String,
    /// SHA-256 of the tar.gz file, hex-encoded.
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub sha256: [u8; 32],
    pub sample_rate: usize,
    pub hop_size: usize,
    pub fft_size: usize,
    /// Frames of lookahead the model uses.
    pub lookahead: usize,
    pub size_bytes: u64,
}

impl ModelInfo {
    pub fn sha256_hex(&self) -> String {
        hex(&self.sha256)
    }
}

/// Immutable, thread-safe model. Share it across sessions.
#[derive(Clone)]
pub struct ModelBundle {
    shared: Arc<DfModelShared>,
    info: ModelInfo,
}

impl ModelBundle {
    /// Load a model bundle (tar.gz). If `expected_sha256` is given, the file's
    /// hash must match.
    pub fn from_path(path: &Path, expected_sha256: Option<[u8; 32]>) -> Result<Self, Error> {
        let bytes = std::fs::read(path)?;
        let sha256 = check_hash(&bytes, expected_sha256)?;
        let params =
            DfParams::new(path.to_path_buf()).map_err(|e| Error::Bundle(format!("{e:#}")))?;
        Self::build(params, sha256, bytes.len() as u64)
    }

    /// Load a model bundle (tar.gz) from memory, e.g. one embedded with
    /// `include_bytes!`. Hashes and parses the same buffer.
    pub fn from_static_bytes(
        bytes: &'static [u8],
        expected_sha256: Option<[u8; 32]>,
    ) -> Result<Self, Error> {
        let sha256 = check_hash(bytes, expected_sha256)?;
        let params = DfParams::from_bytes(bytes).map_err(|e| Error::Bundle(format!("{e:#}")))?;
        Self::build(params, sha256, bytes.len() as u64)
    }

    fn build(params: DfParams, sha256: [u8; 32], size_bytes: u64) -> Result<Self, Error> {
        let shared = DfModelShared::new(params, N_CH, REDUCE_MASK)
            .map_err(|e| Error::ModelInit(format!("{e:#}")))?;

        // A throwaway session reports the model's own parameters, so we don't
        // parse config.ini ourselves.
        let probe = DfTract::from_shared(&shared, &runtime_params(100.0))
            .map_err(|e| Error::ModelInit(format!("{e:#}")))?;
        let info = ModelInfo {
            sha256,
            sample_rate: probe.sr,
            hop_size: probe.hop_size,
            fft_size: probe.fft_size,
            lookahead: probe.lookahead,
            size_bytes,
        };

        Ok(Self {
            shared: Arc::new(shared),
            info,
        })
    }

    pub fn from_directory(path: &Path) -> Result<Self, Error> {
        let manifest_path = path.join("manifest.json");
        let manifest_bytes = std::fs::read(&manifest_path)?;
        let manifest: ModelManifest = serde_json::from_slice(&manifest_bytes)?;

        let bundle_path = path.join(format!("{}.tar.gz", manifest.id));
        Self::from_path(&bundle_path, Some(parse_sha256(&manifest.sha256)?))
    }

    pub fn info(&self) -> &ModelInfo {
        &self.info
    }

    /// For the session module.
    pub(crate) fn shared(&self) -> &Arc<DfModelShared> {
        &self.shared
    }
}

fn check_hash(bytes: &[u8], expected: Option<[u8; 32]>) -> Result<[u8; 32], Error> {
    let sha256: [u8; 32] = Sha256::digest(bytes).into();
    if let Some(expected) = expected
        && expected != sha256
    {
        return Err(Error::HashMismatch {
            expected: hex(&expected),
            actual: hex(&sha256),
        });
    }
    Ok(sha256)
}

/// Decode a hex SHA-256 string (as stored in manifest.json).
pub fn parse_sha256(s: &str) -> Result<[u8; 32], Error> {
    let bytes = hex::decode(s).map_err(|e| Error::Bundle(format!("invalid sha256: {e:#}")))?;
    bytes
        .try_into()
        .map_err(|_| Error::Bundle("invalid sha256 length".to_string()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn assert_send_sync<T: Send + Sync>() {}

    /// Writes `contents` to a unique temp file and returns its path.
    fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("hush-core-test-{}-{name}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn bundle_is_send_and_sync() {
        assert_send_sync::<ModelBundle>();
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let res = ModelBundle::from_path(Path::new("/nonexistent/model.tar.gz"), None);
        assert!(matches!(res, Err(Error::Io(_))));
    }

    #[test]
    fn hash_mismatch_is_reported() {
        let path = temp_file("mismatch.tar.gz", b"not a bundle");
        let res = ModelBundle::from_path(&path, Some([0u8; 32]));
        std::fs::remove_file(&path).ok();
        assert!(matches!(res, Err(Error::HashMismatch { .. })));
    }

    #[test]
    fn garbage_is_a_bundle_error_not_a_panic() {
        let path = temp_file("garbage.tar.gz", b"not a bundle");
        let res = ModelBundle::from_path(&path, None);
        std::fs::remove_file(&path).ok();
        assert!(matches!(res, Err(Error::Bundle(_))));
    }

    static MODEL: &[u8] = include_bytes!(concat!(
        env!("CARGO_WORKSPACE_DIR"),
        "/models/advanced_dfnet16k/advanced_dfnet16k.tar.gz"
    ));

    #[test]
    fn static_bytes_load_and_match_directory() {
        let dir = Path::new(env!("CARGO_WORKSPACE_DIR")).join("models/advanced_dfnet16k");
        let from_dir = ModelBundle::from_directory(&dir).unwrap();
        let from_mem = ModelBundle::from_static_bytes(MODEL, Some(from_dir.info().sha256)).unwrap();
        assert_eq!(from_mem.info(), from_dir.info());
    }

    #[test]
    fn static_bytes_hash_mismatch() {
        let res = ModelBundle::from_static_bytes(MODEL, Some([0u8; 32]));
        assert!(matches!(res, Err(Error::HashMismatch { .. })));
    }

    #[test]
    fn valid_bundle_loads() {
        let path = Path::new(env!("CARGO_WORKSPACE_DIR"))
            .join("models")
            .join("advanced_dfnet16k");
        let bundle = ModelBundle::from_directory(&path).unwrap();
        let info = bundle.info();
        assert_eq!(info.sample_rate, 16000);
        assert_eq!(info.hop_size, 160);
        assert_eq!(info.fft_size, 320);
        assert_eq!(info.lookahead, 0);
    }
}
