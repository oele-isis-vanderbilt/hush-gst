//! Turn INPUTS (files, directories, globs) into input -> output pairs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::cli::DenoiseArgs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub input: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub jobs: Vec<Job>,
    /// Inputs that matched nothing or could not be read.
    pub errors: Vec<String>,
}

pub struct Options<'a> {
    pub output_dir: Option<&'a Path>,
    pub suffix: &'a str,
    pub recursive: bool,
    pub exts: &'a [String],
    pub raw: bool,
}

impl<'a> From<&'a DenoiseArgs> for Options<'a> {
    fn from(a: &'a DenoiseArgs) -> Self {
        Self {
            output_dir: a.output_dir.as_deref(),
            suffix: &a.suffix,
            recursive: a.recursive,
            exts: &a.ext,
            raw: a.raw,
        }
    }
}

pub fn plan(inputs: &[String], opts: &Options) -> Result<Plan> {
    let mut plan = Plan::default();
    let mut seen = HashSet::new();
    let mut files = Vec::new();

    for arg in inputs {
        let path = Path::new(arg);
        let mut found = Vec::new();
        if path.is_file() {
            // Named explicitly: always taken, whatever the extension.
            found.push(path.to_path_buf());
        } else if path.is_dir() {
            walk(path, opts, &mut found, &mut plan.errors);
            if found.is_empty() {
                plan.errors.push(format!("{arg}: no media files found"));
            }
        } else if arg.contains(['*', '?', '[']) {
            match glob::glob(arg) {
                Ok(paths) => {
                    for p in paths {
                        match p {
                            Ok(p) if p.is_file() && !is_own_output(&p, opts.suffix) => {
                                found.push(p)
                            }
                            Ok(_) => {}
                            Err(e) => plan.errors.push(format!("{arg}: {e}")),
                        }
                    }
                    if found.is_empty() {
                        plan.errors.push(format!("{arg}: pattern matched no files"));
                    }
                }
                Err(e) => plan.errors.push(format!("{arg}: invalid pattern: {e}")),
            }
        } else {
            plan.errors
                .push(format!("{arg}: no such file or directory"));
        }

        for f in found {
            let key = f.canonicalize().unwrap_or_else(|_| f.clone());
            if seen.insert(key) {
                files.push(f);
            }
        }
    }

    let mut by_output: HashMap<PathBuf, PathBuf> = HashMap::new();
    for input in files {
        let output = output_path(&input, opts);
        if output == input {
            bail!("{}: output would overwrite the input", input.display());
        }
        if let Some(prev) = by_output.insert(output.clone(), input.clone()) {
            bail!(
                "{} and {} would both be written to {}; use separate output directories",
                prev.display(),
                input.display(),
                output.display()
            );
        }
        plan.jobs.push(Job { input, output });
    }
    Ok(plan)
}

fn walk(dir: &Path, opts: &Options, out: &mut Vec<PathBuf>, errors: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            errors.push(format!("{}: {e}", dir.display()));
            return;
        }
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths {
        if p.is_dir() {
            if opts.recursive {
                walk(&p, opts, out, errors);
            }
        } else if has_ext(&p, opts.exts) && !is_own_output(&p, opts.suffix) && !is_hidden(&p) {
            out.push(p);
        }
    }
}

fn has_ext(p: &Path, exts: &[String]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

/// Files we wrote on an earlier run (`talk_denoised.m4a`). Skipped when found
/// through a directory or a pattern, so re-runs don't denoise them again.
fn is_own_output(p: &Path, suffix: &str) -> bool {
    p.file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.ends_with(suffix))
}

fn is_hidden(p: &Path) -> bool {
    p.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.starts_with('.'))
}

fn output_path(input: &Path, opts: &Options) -> PathBuf {
    let dir = match opts.output_dir {
        Some(d) => d.to_path_buf(),
        None => input.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let mut name = format!("{stem}{}", opts.suffix);
    if opts.raw {
        name.push_str(".wav");
    } else if let Some(ext) = input.extension() {
        name.push('.');
        name.push_str(&ext.to_string_lossy());
    }
    dir.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exts() -> Vec<String> {
        crate::cli::DEFAULT_EXTS
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn opts<'a>(
        exts: &'a [String],
        out: Option<&'a Path>,
        recursive: bool,
        raw: bool,
    ) -> Options<'a> {
        Options {
            output_dir: out,
            suffix: "_denoised",
            recursive,
            exts,
            raw,
        }
    }

    fn touch(dir: &Path, rel: &str) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"").unwrap();
        p
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn file_output_next_to_input() {
        let t = tempfile::tempdir().unwrap();
        let a = touch(t.path(), "talk.m4a");
        let e = exts();
        let p = plan(&[s(&a)], &opts(&e, None, false, false)).unwrap();
        assert_eq!(
            p.jobs,
            [Job {
                input: a,
                output: t.path().join("talk_denoised.m4a")
            }]
        );
        assert!(p.errors.is_empty());
    }

    #[test]
    fn raw_writes_wav_into_output_dir() {
        let t = tempfile::tempdir().unwrap();
        let a = touch(t.path(), "talk.mp3");
        let out = t.path().join("out");
        let e = exts();
        let p = plan(&[s(&a)], &opts(&e, Some(&out), false, true)).unwrap();
        assert_eq!(p.jobs[0].output, out.join("talk_denoised.wav"));
    }

    #[test]
    fn directory_walk_filters_and_recurses() {
        let t = tempfile::tempdir().unwrap();
        touch(t.path(), "a.wav");
        touch(t.path(), "a_denoised.wav");
        touch(t.path(), "notes.txt");
        touch(t.path(), ".hidden.wav");
        touch(t.path(), "sub/b.MP3");
        let e = exts();

        let flat = plan(&[s(t.path())], &opts(&e, None, false, false)).unwrap();
        assert_eq!(flat.jobs.len(), 1);

        let deep = plan(&[s(t.path())], &opts(&e, None, true, false)).unwrap();
        let names: Vec<_> = deep
            .jobs
            .iter()
            .map(|j| j.input.file_name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["a.wav", "b.MP3"]);
    }

    #[test]
    fn glob_and_dedup() {
        let t = tempfile::tempdir().unwrap();
        let a = touch(t.path(), "a.wav");
        touch(t.path(), "b.wav");
        let e = exts();
        let pattern = s(&t.path().join("*.wav"));
        let p = plan(&[s(&a), pattern], &opts(&e, None, false, false)).unwrap();
        assert_eq!(p.jobs.len(), 2);
    }

    #[test]
    fn missing_inputs_are_errors() {
        let t = tempfile::tempdir().unwrap();
        let e = exts();
        let p = plan(
            &[s(&t.path().join("nope.wav")), s(&t.path().join("*.flac"))],
            &opts(&e, None, false, false),
        )
        .unwrap();
        assert!(p.jobs.is_empty());
        assert_eq!(p.errors.len(), 2);
    }

    #[test]
    fn output_collision_rejected() {
        let t = tempfile::tempdir().unwrap();
        let a = touch(t.path(), "x/talk.wav");
        let b = touch(t.path(), "y/talk.wav");
        let out = t.path().join("out");
        let e = exts();
        assert!(plan(&[s(&a), s(&b)], &opts(&e, Some(&out), false, false)).is_err());
    }
}
