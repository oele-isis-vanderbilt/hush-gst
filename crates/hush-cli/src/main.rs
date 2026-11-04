mod cli;
mod ffmpeg;
mod inputs;
mod media;
mod model;
mod pipeline;
mod report;
mod run;

use std::process::ExitCode;
use std::sync::atomic::Ordering;

use anyhow::Result;
use clap::{CommandFactory, Parser};
use console::style;

use cli::{Cli, ColorChoice, Command, FfmpegCommand, GlobalArgs};

/// Exit codes. clap itself exits with 2 on usage errors.
pub mod exit {
    pub const OK: u8 = 0;
    /// One or more files failed; the others were processed.
    pub const FILES_FAILED: u8 = 1;
    /// Setup problem (ffmpeg missing, model invalid, ...).
    pub const SETUP: u8 = 3;
    /// Interrupted with Ctrl-C.
    pub const INTERRUPTED: u8 = 130;
}

/// Encoders the output rules in `media::codec` may ask for.
const ENCODERS: &[&str] = &["aac", "alac", "flac", "libmp3lame", "libopus", "libvorbis"];

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.global.color {
        ColorChoice::Always => console::set_colors_enabled_stderr(true),
        ColorChoice::Never => console::set_colors_enabled_stderr(false),
        ColorChoice::Auto => {}
    }
    // First Ctrl-C: stop after cleaning up temp files. Second: exit now.
    let _ = ctrlc::set_handler(|| {
        if pipeline::CANCEL.swap(true, Ordering::SeqCst) {
            std::process::exit(exit::INTERRUPTED as i32);
        }
    });

    let code = match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e:#}", style("error:").red().bold());
            exit::SETUP
        }
    };
    if pipeline::CANCEL.load(Ordering::SeqCst) {
        return ExitCode::from(exit::INTERRUPTED);
    }
    ExitCode::from(code)
}

fn run(cli: &Cli) -> Result<u8> {
    let g = &cli.global;
    match &cli.command {
        Command::Denoise(args) => run::denoise(args, g),
        Command::Ffmpeg(FfmpegCommand::Install { force, dir }) => {
            let dir = match dir {
                Some(d) => d.clone(),
                None => ffmpeg::locate::default_install_dir()?,
            };
            let quiet = g.quiet;
            let r = ffmpeg::install::install(&dir, *force, &|m| {
                if !quiet {
                    eprintln!("{m}");
                }
            })?;
            if !g.quiet {
                let what = if r.fresh {
                    "installed"
                } else {
                    "already installed"
                };
                eprintln!("ffmpeg {what} in {}\n{}", r.dir.display(), r.version);
                if !r.fresh {
                    eprintln!("use --force to reinstall");
                }
            }
            Ok(exit::OK)
        }
        Command::Ffmpeg(FfmpegCommand::Path) => {
            let t = ffmpeg::locate(g.ffmpeg.as_deref())?;
            println!("ffmpeg:  {}", t.ffmpeg.display());
            println!("ffprobe: {}", t.ffprobe.display());
            println!("source:  {}", t.source);
            println!("version: {}", t.version()?);
            Ok(exit::OK)
        }
        Command::Doctor => Ok(doctor(g)),
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(*shell, &mut cmd, name, &mut std::io::stdout());
            Ok(exit::OK)
        }
    }
}

fn doctor(g: &GlobalArgs) -> u8 {
    let ok = style("ok").green().to_string();
    let bad = style("fail").red().to_string();
    let warn = style("warn").yellow().to_string();
    let mut code = exit::OK;
    let row =
        |name: &str, status: &str, detail: String| println!("{name:<9} {status:<4}  {detail}");

    match model::load(None) {
        Ok(m) => {
            let i = m.bundle.info();
            row(
                "model",
                &ok,
                format!(
                    "{} ({} Hz, sha256 {})",
                    m.manifest.name,
                    i.sample_rate,
                    &m.manifest.sha256[..12]
                ),
            );
        }
        Err(e) => {
            row("model", &bad, format!("{e:#}"));
            code = exit::SETUP;
        }
    }

    match ffmpeg::locate(g.ffmpeg.as_deref()) {
        Ok(t) => {
            match t.version() {
                Ok(v) => row(
                    "ffmpeg",
                    &ok,
                    format!("{} [{}]\n{:16}{v}", t.ffmpeg.display(), t.source, ""),
                ),
                Err(e) => {
                    row("ffmpeg", &bad, format!("{e:#}"));
                    code = exit::SETUP;
                }
            }
            match t.probe_version() {
                Ok(_) => row("ffprobe", &ok, t.ffprobe.display().to_string()),
                Err(e) => {
                    row("ffprobe", &bad, format!("{e:#}"));
                    code = exit::SETUP;
                }
            }
            let available = encoders(&t);
            let missing: Vec<&str> = ENCODERS
                .iter()
                .copied()
                .filter(|e| !available.iter().any(|a| a == e))
                .collect();
            if missing.is_empty() {
                row("encoders", &ok, ENCODERS.join(" "));
            } else {
                row(
                    "encoders",
                    &warn,
                    format!(
                        "missing {} (files needing them will fail; --raw still works)",
                        missing.join(" ")
                    ),
                );
            }
        }
        Err(e) => {
            row("ffmpeg", &bad, format!("{e:#}"));
            code = exit::SETUP;
        }
    }

    match std::env::current_exe() {
        Ok(exe) => {
            let dir = exe.parent().map(|d| d.to_path_buf()).unwrap_or_default();
            let on_path = std::env::var_os("PATH")
                .map(|p| std::env::split_paths(&p).any(|d| d == dir))
                .unwrap_or(false);
            let status = if on_path { &ok } else { &warn };
            let note = if on_path {
                ""
            } else {
                "  (not on PATH; restart your shell or add it)"
            };
            row("install", status, format!("{}{note}", exe.display()));
        }
        Err(e) => row("install", &warn, e.to_string()),
    }
    code
}

/// Names from `ffmpeg -encoders`.
fn encoders(t: &ffmpeg::Tools) -> Vec<String> {
    let Ok(out) = t.ffmpeg().arg("-encoders").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip_while(|l| !l.trim_start().starts_with("------"))
        .skip(1)
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
        .collect()
}
