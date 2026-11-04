//! The model: built in by default, or loaded from a directory with `--model`.

use std::path::Path;

use anyhow::{Context, Result};
use hush_core::{ModelBundle, ModelManifest, parse_sha256};

static MODEL: &[u8] = include_bytes!(concat!(
    env!("CARGO_WORKSPACE_DIR"),
    "/models/advanced_dfnet16k/advanced_dfnet16k.tar.gz"
));
static MANIFEST: &str = include_str!(concat!(
    env!("CARGO_WORKSPACE_DIR"),
    "/models/advanced_dfnet16k/manifest.json"
));

pub struct Loaded {
    pub bundle: ModelBundle,
    pub manifest: ModelManifest,
}

pub fn load(dir: Option<&Path>) -> Result<Loaded> {
    match dir {
        Some(dir) => {
            let manifest_path = dir.join("manifest.json");
            let manifest: ModelManifest = serde_json::from_slice(
                &std::fs::read(&manifest_path)
                    .with_context(|| format!("reading {}", manifest_path.display()))?,
            )
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
            let bundle = ModelBundle::from_directory(dir)
                .with_context(|| format!("loading model from {}", dir.display()))?;
            Ok(Loaded { bundle, manifest })
        }
        None => {
            let manifest: ModelManifest =
                serde_json::from_str(MANIFEST).context("parsing built-in model manifest")?;
            let bundle =
                ModelBundle::from_static_bytes(MODEL, Some(parse_sha256(&manifest.sha256)?))
                    .context("loading built-in model")?;
            Ok(Loaded { bundle, manifest })
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn builtin_model_loads() {
        let m = super::load(None).unwrap();
        assert_eq!(m.bundle.info().sample_rate, 16000);
        assert_eq!(m.bundle.info().sha256_hex(), m.manifest.sha256);
    }
}
