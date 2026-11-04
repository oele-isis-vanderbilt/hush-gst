//! Lookup order: `--ffmpeg` / `HUSH_FFMPEG` -> sidecar next to the install ->
//! sidecar in the user data dir -> `PATH`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::exe;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `--ffmpeg` or `HUSH_FFMPEG`.
    Explicit,
    Sidecar(PathBuf),
    SystemPath,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Source::Explicit => write!(f, "--ffmpeg / HUSH_FFMPEG"),
            Source::Sidecar(dir) => write!(f, "sidecar ({})", dir.display()),
            Source::SystemPath => write!(f, "PATH"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub source: Source,
}

impl Tools {
    pub fn ffmpeg(&self) -> Command {
        let mut c = Command::new(&self.ffmpeg);
        c.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
        c
    }

    pub fn ffprobe(&self) -> Command {
        let mut c = Command::new(&self.ffprobe);
        c.args(["-hide_banner", "-loglevel", "error"]);
        c
    }

    /// First line of `ffmpeg -version`.
    pub fn version(&self) -> Result<String> {
        version_line(&self.ffmpeg)
    }

    pub fn probe_version(&self) -> Result<String> {
        version_line(&self.ffprobe)
    }
}

pub fn version_line(bin: &Path) -> Result<String> {
    let out = Command::new(bin)
        .arg("-version")
        .output()
        .with_context(|| format!("running {}", bin.display()))?;
    if !out.status.success() {
        bail!("{} -version failed", bin.display());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string())
}

/// Sidecar directories, in lookup order.
pub fn sidecar_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = install_root().map(|r| r.join("ffmpeg")) {
        dirs.push(d);
    }
    if let Some(d) = data_sidecar_dir() {
        dirs.push(d);
    }
    dirs
}

/// `~/.hush` when the binary is installed at `~/.hush/bin/hush`.
pub fn install_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    Some(bin.parent()?.to_path_buf())
}

fn data_sidecar_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("hush").join("ffmpeg"))
}

/// Where `hush ffmpeg install` writes when no `--dir` is given: next to the
/// install if this binary was installed by the installer, else the data dir.
pub fn default_install_dir() -> Result<PathBuf> {
    if let Some(root) = install_root().filter(|r| r.join(".hush-install").exists()) {
        return Ok(root.join("ffmpeg"));
    }
    data_sidecar_dir().context("no user data directory on this system; pass --dir")
}

pub fn locate(explicit: Option<&Path>) -> Result<Tools> {
    if let Some(p) = explicit {
        let ffmpeg = if p.is_dir() {
            p.join(exe("ffmpeg"))
        } else {
            p.to_path_buf()
        };
        if !ffmpeg.is_file() {
            bail!(
                "ffmpeg not found at {} (from --ffmpeg / HUSH_FFMPEG)",
                ffmpeg.display()
            );
        }
        let ffprobe = ffmpeg.with_file_name(exe("ffprobe"));
        if !ffprobe.is_file() {
            bail!("ffprobe not found next to ffmpeg ({})", ffprobe.display());
        }
        return Ok(Tools {
            ffmpeg,
            ffprobe,
            source: Source::Explicit,
        });
    }

    for dir in sidecar_dirs() {
        let ffmpeg = dir.join(exe("ffmpeg"));
        let ffprobe = dir.join(exe("ffprobe"));
        if ffmpeg.is_file() && ffprobe.is_file() {
            return Ok(Tools {
                ffmpeg,
                ffprobe,
                source: Source::Sidecar(dir),
            });
        }
    }

    match (which::which("ffmpeg"), which::which("ffprobe")) {
        (Ok(ffmpeg), Ok(ffprobe)) => Ok(Tools {
            ffmpeg,
            ffprobe,
            source: Source::SystemPath,
        }),
        _ => bail!(
            "ffmpeg and ffprobe not found. Run `hush ffmpeg install`, or pass --ffmpeg <path>"
        ),
    }
}
