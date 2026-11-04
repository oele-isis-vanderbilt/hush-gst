//! `hush ffmpeg install`: download the pinned build for this platform, check
//! its SHA-256, extract ffmpeg + ffprobe, test them, then swap them into place.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::exe;
use super::locate::version_line;

static SOURCES: &str = include_str!("sources.toml");

#[derive(Debug, Deserialize)]
struct Sources {
    target: Vec<Target>,
}

#[derive(Debug, Deserialize)]
struct Target {
    key: String,
    license: String,
    download: Vec<Download>,
}

#[derive(Debug, Deserialize)]
struct Download {
    url: String,
    sha256: String,
    archive: Archive,
    extract: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
enum Archive {
    #[serde(rename = "zip")]
    Zip,
    #[serde(rename = "tar.xz")]
    TarXz,
    #[serde(rename = "tar.gz")]
    TarGz,
}

pub fn platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub struct Installed {
    pub dir: PathBuf,
    pub version: String,
    pub fresh: bool,
}

pub fn install(dir: &Path, force: bool, log: &dyn Fn(&str)) -> Result<Installed> {
    let ffmpeg = dir.join(exe("ffmpeg"));
    let ffprobe = dir.join(exe("ffprobe"));
    if !force
        && ffmpeg.is_file()
        && ffprobe.is_file()
        && let (Ok(v), Ok(_)) = (version_line(&ffmpeg), version_line(&ffprobe))
    {
        return Ok(Installed {
            dir: dir.to_path_buf(),
            version: v,
            fresh: false,
        });
    }

    let sources: Sources = toml::from_str(SOURCES).context("parsing built-in sources.toml")?;
    let key = platform_key();
    let target = sources
        .target
        .iter()
        .find(|t| t.key == key)
        .with_context(|| format!("no FFmpeg build is listed for {key}"))?;
    if target
        .download
        .iter()
        .any(|d| d.url.is_empty() || d.sha256.is_empty())
    {
        bail!(
            "no pinned FFmpeg build for {key} in this release yet.\n\
             Install ffmpeg with your package manager (brew, apt, winget, ...) and hush \
             will find it on PATH, or point to it with --ffmpeg <path> / HUSH_FFMPEG"
        );
    }

    let parent = dir.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let work = tempfile::tempdir_in(parent).context("creating a staging directory")?;
    let staging = work.path().join("ffmpeg");
    std::fs::create_dir(&staging)?;

    let mut notice = format!("FFmpeg for {key}\nLicense: {}\n\n", target.license);
    for d in &target.download {
        log(&format!("downloading {}", d.url));
        let archive_path = work.path().join("download");
        let sha = download(&d.url, &archive_path)?;
        if !sha.eq_ignore_ascii_case(&d.sha256) {
            bail!(
                "checksum mismatch for {}: expected {}, got {sha}",
                d.url,
                d.sha256
            );
        }
        extract(&archive_path, d.archive, &d.extract, &staging)
            .with_context(|| format!("extracting {}", d.url))?;
        std::fs::remove_file(&archive_path).ok();
        notice.push_str(&format!("Source: {}\nSHA-256: {}\n", d.url, d.sha256));
    }
    notice.push_str(
        "\nFFmpeg is a trademark of Fabrice Bellard. Source code: https://ffmpeg.org/download.html\n",
    );
    std::fs::write(staging.join("NOTICE.txt"), &notice)?;

    let staged_ffmpeg = staging.join(exe("ffmpeg"));
    let staged_ffprobe = staging.join(exe("ffprobe"));
    for bin in [&staged_ffmpeg, &staged_ffprobe] {
        if !bin.is_file() {
            bail!(
                "{} missing from the download",
                bin.file_name().unwrap().to_string_lossy()
            );
        }
        make_runnable(bin);
    }
    let version = version_line(&staged_ffmpeg).context("the downloaded ffmpeg does not run")?;
    version_line(&staged_ffprobe).context("the downloaded ffprobe does not run")?;
    std::fs::write(staging.join("VERSION"), format!("{version}\n{notice}"))?;

    if dir.exists() {
        std::fs::remove_dir_all(dir).with_context(|| format!("removing old {}", dir.display()))?;
    }
    std::fs::rename(&staging, dir).with_context(|| format!("moving into {}", dir.display()))?;
    Ok(Installed {
        dir: dir.to_path_buf(),
        version,
        fresh: true,
    })
}

/// Stream to `dest`, return the hex SHA-256.
fn download(url: &str, dest: &Path) -> Result<String> {
    let resp = ureq::get(url)
        .call()
        .with_context(|| format!("downloading {url}"))?;
    let mut reader = resp.into_body().into_reader();
    let mut file = File::create(dest)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("downloading {url}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
    }
    file.flush()?;
    Ok(hex::encode(hasher.finalize()))
}

fn extract(archive: &Path, kind: Archive, wanted: &[String], dest: &Path) -> Result<()> {
    let mut found: Vec<&str> = Vec::new();
    let mut take = |name: &str, r: &mut dyn Read| -> Result<()> {
        let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
        if let Some(w) = wanted.iter().find(|w| w.as_str() == base)
            && !found.contains(&w.as_str())
        {
            let mut out = File::create(dest.join(base))?;
            io::copy(r, &mut out)?;
            found.push(w.as_str());
        }
        Ok(())
    };

    match kind {
        Archive::Zip => {
            let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                if entry.is_file() {
                    let name = entry.name()?.into_owned();
                    take(&name, &mut entry)?;
                }
            }
        }
        Archive::TarXz | Archive::TarGz => {
            let file = File::open(archive)?;
            let reader: Box<dyn Read> = match kind {
                Archive::TarXz => Box::new(xz2::read::XzDecoder::new(file)),
                _ => Box::new(flate2::read::GzDecoder::new(file)),
            };
            let mut tar = tar::Archive::new(reader);
            for entry in tar.entries()? {
                let mut entry = entry?;
                if entry.header().entry_type().is_file() {
                    let name = entry.path()?.to_string_lossy().into_owned();
                    take(&name, &mut entry)?;
                }
            }
        }
    }

    // LICENSE files are nice to have; the binaries are required.
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|w| !w.starts_with("LICENSE") && !found.contains(&w.as_str()))
        .collect();
    if !missing.is_empty() {
        bail!("archive does not contain {missing:?}");
    }
    Ok(())
}

#[cfg(unix)]
fn make_runnable(bin: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(bin, std::fs::Permissions::from_mode(0o755));
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(bin)
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(not(unix))]
fn make_runnable(_bin: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_parse_and_cover_release_targets() {
        let s: Sources = toml::from_str(SOURCES).unwrap();
        for key in [
            "macos-aarch64",
            "macos-x86_64",
            "linux-x86_64",
            "linux-aarch64",
            "windows-x86_64",
        ] {
            let t = s
                .target
                .iter()
                .find(|t| t.key == key)
                .unwrap_or_else(|| panic!("{key}"));
            let names: Vec<&String> = t.download.iter().flat_map(|d| &d.extract).collect();
            let suffix = if key.starts_with("windows") {
                ".exe"
            } else {
                ""
            };
            assert!(
                names.iter().any(|n| **n == format!("ffmpeg{suffix}")),
                "{key}"
            );
            assert!(
                names.iter().any(|n| **n == format!("ffprobe{suffix}")),
                "{key}"
            );
        }
    }

    #[test]
    fn extracts_from_nested_zip() {
        let t = tempfile::tempdir().unwrap();
        let zip_path = t.path().join("a.zip");
        {
            let mut w = zip::ZipWriter::new(File::create(&zip_path).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            w.start_file("ffmpeg-x/bin/ffmpeg", o).unwrap();
            w.write_all(b"ff").unwrap();
            w.start_file("ffmpeg-x/bin/ffprobe", o).unwrap();
            w.write_all(b"fp").unwrap();
            w.start_file("ffmpeg-x/doc/readme", o).unwrap();
            w.finish().unwrap();
        }
        let dest = t.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let wanted = ["ffmpeg".to_string(), "ffprobe".into(), "LICENSE.txt".into()];
        extract(&zip_path, Archive::Zip, &wanted, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("ffmpeg")).unwrap(), b"ff");
        assert_eq!(std::fs::read(dest.join("ffprobe")).unwrap(), b"fp");
        assert!(!dest.join("readme").exists());
    }

    #[test]
    fn extracts_from_tar_xz_and_reports_missing() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("a.tar.xz");
        {
            let xz = xz2::write::XzEncoder::new(File::create(&path).unwrap(), 1);
            let mut b = tar::Builder::new(xz);
            let mut h = tar::Header::new_gnu();
            h.set_size(2);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, "x/bin/ffmpeg", &b"ff"[..]).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        let dest = t.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let wanted = ["ffmpeg".to_string(), "ffprobe".into()];
        let err = extract(&path, Archive::TarXz, &wanted, &dest).unwrap_err();
        assert!(err.to_string().contains("ffprobe"));
        assert!(dest.join("ffmpeg").exists());
    }
}
