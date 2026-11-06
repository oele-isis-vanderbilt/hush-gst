//! Command-line surface. Parsing and validation only; no work happens here.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};

/// Extensions picked up when an input is a directory.
pub const DEFAULT_EXTS: &[&str] = &[
    "wav", "flac", "mp3", "m4a", "aac", "mp4", "mov", "mkv", "ogg", "opus", "webm",
];

#[derive(Debug, Parser)]
#[command(
    name = "hush",
    version,
    about = "Remove background noise from audio and video files",
    long_about = "Remove background noise from audio and video files.\n\n\
        Each file is decoded with FFmpeg, mixed to mono, resampled to 16 kHz, \
        denoised, then written back in its original format and sample rate \
        (mono). With --per-channel each channel is denoised on its own and the \
        channels are kept. Video streams are copied unchanged.",
    propagate_version = true
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Args)]
#[command(next_help_heading = "Global options")]
pub struct GlobalArgs {
    /// More output (repeat for more detail)
    #[arg(short, long, global = true, action = ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Only print errors
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Path to the ffmpeg binary; ffprobe is looked up in the same directory
    #[arg(long, global = true, env = "HUSH_FFMPEG", value_name = "PATH")]
    pub ffmpeg: Option<PathBuf>,

    /// When to use colors
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    pub color: ColorChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Denoise files, directories or glob patterns
    Denoise(DenoiseArgs),

    /// Manage the FFmpeg sidecar
    #[command(subcommand)]
    Ffmpeg(FfmpegCommand),

    /// Check that ffmpeg, ffprobe and the model are usable
    Doctor,

    /// Print a shell completion script
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Args)]
pub struct DenoiseArgs {
    /// Files, directories, or glob patterns (e.g. "talks/*.m4a")
    #[arg(required = true, value_name = "INPUTS")]
    pub inputs: Vec<String>,

    /// Write outputs here instead of next to each input
    #[arg(short, long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// Added to each output file name: talk.m4a -> talk_denoised.m4a
    #[arg(long, default_value = "_denoised", value_parser = parse_suffix)]
    pub suffix: String,

    /// Descend into subdirectories
    #[arg(short, long)]
    pub recursive: bool,

    /// Extensions taken from directories (comma separated)
    #[arg(long, value_delimiter = ',', default_values_t = DEFAULT_EXTS.iter().map(|s| s.to_string()))]
    pub ext: Vec<String>,

    /// Ignore files whose name starts with PREFIX when walking directories or
    /// patterns (repeatable or comma separated, e.g. "screen-,av-mix")
    #[arg(long, value_delimiter = ',', value_name = "PREFIX", value_parser = parse_prefix)]
    pub exclude_prefix: Vec<String>,

    /// Skip files without an audio stream instead of reporting them as failed
    #[arg(long)]
    pub skip_no_audio: bool,

    /// Replace existing outputs (default: skip them)
    #[arg(short = 'y', long)]
    pub overwrite: bool,

    /// Number of files processed in parallel [default: min(4, CPUs)]
    #[arg(short, long, value_parser = clap::value_parser!(u16).range(1..))]
    pub jobs: Option<u16>,

    /// Maximum noise reduction in dB (0-100, 100 = unlimited)
    #[arg(long, default_value_t = 100.0, value_parser = parse_atten, value_name = "DB")]
    pub atten_lim_db: f32,

    /// Write the 16 kHz mono model output as WAV instead of the original format
    #[arg(long)]
    pub raw: bool,

    /// Denoise each channel separately and keep them (e.g. separate L/R mics) instead of mixing to mono
    #[arg(long)]
    pub per_channel: bool,

    /// Model directory with manifest.json [default: built-in model]
    #[arg(long, value_name = "DIR")]
    pub model: Option<PathBuf>,

    /// List input -> output pairs without processing
    #[arg(long)]
    pub dry_run: bool,

    /// Print a machine-readable summary on stdout
    #[arg(long)]
    pub json: bool,
}

impl DenoiseArgs {
    pub fn jobs(&self) -> usize {
        match self.jobs {
            Some(n) => n as usize,
            None => std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .min(4),
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum FfmpegCommand {
    /// Download and verify the pinned FFmpeg build
    Install {
        /// Reinstall even if a working sidecar exists
        #[arg(long)]
        force: bool,

        /// Install into this directory instead of the default sidecar location
        #[arg(long, value_name = "DIR")]
        dir: Option<PathBuf>,
    },

    /// Show which ffmpeg and ffprobe would be used, and why
    Path,
}

/// Same rule as hush-core's `Settings::new`.
fn parse_atten(s: &str) -> Result<f32, String> {
    let db: f32 = s.parse().map_err(|_| format!("`{s}` is not a number"))?;
    if db.is_nan() || !(0.0..=100.0).contains(&db) {
        return Err(format!("must be between 0 and 100, got {db}"));
    }
    Ok(db)
}

fn parse_prefix(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("must not be empty (would exclude every file)".into());
    }
    if s.contains(['/', '\\']) {
        return Err("must not contain path separators".into());
    }
    Ok(s.to_string())
}

fn parse_suffix(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("must not be empty (outputs would overwrite inputs)".into());
    }
    if s.contains(['/', '\\']) {
        return Err("must not contain path separators".into());
    }
    Ok(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("hush").chain(args.iter().copied()))
    }

    fn denoise(args: &[&str]) -> DenoiseArgs {
        match parse(args).unwrap().command {
            Command::Denoise(d) => d,
            other => panic!("expected denoise, got {other:?}"),
        }
    }

    #[test]
    fn command_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn denoise_defaults() {
        let d = denoise(&["denoise", "a.wav"]);
        assert_eq!(d.inputs, ["a.wav"]);
        assert_eq!(d.suffix, "_denoised");
        assert_eq!(d.atten_lim_db, 100.0);
        assert_eq!(d.ext, DEFAULT_EXTS);
        assert!(!d.recursive && !d.overwrite && !d.raw && !d.dry_run && !d.json);
        assert!(!d.per_channel);
        assert!(d.output_dir.is_none() && d.model.is_none());
        assert!((1..=4).contains(&d.jobs()));
    }

    #[test]
    fn denoise_all_options() {
        let d = denoise(&[
            "denoise",
            "a.mp3",
            "dir",
            "*.m4a",
            "-o",
            "out",
            "--suffix",
            "_clean",
            "-r",
            "--ext",
            "wav,mp3",
            "-y",
            "-j",
            "8",
            "--atten-lim-db",
            "20",
            "--raw",
            "--per-channel",
            "--model",
            "m",
            "--dry-run",
            "--json",
        ]);
        assert_eq!(d.inputs, ["a.mp3", "dir", "*.m4a"]);
        assert_eq!(d.output_dir.as_deref(), Some(std::path::Path::new("out")));
        assert_eq!(d.suffix, "_clean");
        assert_eq!(d.ext, ["wav", "mp3"]);
        assert_eq!(d.jobs(), 8);
        assert_eq!(d.atten_lim_db, 20.0);
        assert!(d.recursive && d.overwrite && d.raw && d.dry_run && d.json);
        assert!(d.per_channel);
    }

    #[test]
    fn denoise_requires_inputs() {
        let e = parse(&["denoise"]).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn atten_range_checked() {
        for bad in ["-1", "100.5", "NaN", "loud"] {
            assert!(
                parse(&["denoise", "a.wav", "--atten-lim-db", bad]).is_err(),
                "accepted {bad}"
            );
        }
        assert_eq!(
            denoise(&["denoise", "a.wav", "--atten-lim-db", "0"]).atten_lim_db,
            0.0
        );
    }

    #[test]
    fn jobs_must_be_positive() {
        assert!(parse(&["denoise", "a.wav", "-j", "0"]).is_err());
    }

    #[test]
    fn exclude_prefix_and_skip_no_audio() {
        let d = denoise(&["denoise", "a"]);
        assert!(d.exclude_prefix.is_empty() && !d.skip_no_audio);
        let d = denoise(&[
            "denoise",
            "a",
            "--exclude-prefix",
            "screen-,av-mix",
            "--exclude-prefix",
            "x",
            "--skip-no-audio",
        ]);
        assert_eq!(d.exclude_prefix, ["screen-", "av-mix", "x"]);
        assert!(d.skip_no_audio);
        assert!(parse(&["denoise", "a", "--exclude-prefix", ""]).is_err());
        assert!(parse(&["denoise", "a", "--exclude-prefix", "a/b"]).is_err());
    }

    #[test]
    fn suffix_checked() {
        assert!(parse(&["denoise", "a.wav", "--suffix", ""]).is_err());
        assert!(parse(&["denoise", "a.wav", "--suffix", "x/y"]).is_err());
    }

    #[test]
    fn quiet_conflicts_with_verbose() {
        let e = parse(&["-q", "-v", "doctor"]).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn global_flags_after_subcommand() {
        let cli = parse(&["denoise", "a.wav", "-vv", "--ffmpeg", "/opt/ff/ffmpeg"]).unwrap();
        assert_eq!(cli.global.verbose, 2);
        assert_eq!(
            cli.global.ffmpeg.as_deref(),
            Some(std::path::Path::new("/opt/ff/ffmpeg"))
        );
    }

    #[test]
    fn ffmpeg_subcommands() {
        match parse(&["ffmpeg", "install", "--force", "--dir", "d"])
            .unwrap()
            .command
        {
            Command::Ffmpeg(FfmpegCommand::Install { force, dir }) => {
                assert!(force);
                assert_eq!(dir.as_deref(), Some(std::path::Path::new("d")));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            parse(&["ffmpeg", "path"]).unwrap().command,
            Command::Ffmpeg(FfmpegCommand::Path)
        ));
    }

    /// Help text of every command, as it appears in crates/hush-cli/README.md.
    fn help_reference() -> String {
        let commands: &[&[&str]] = &[
            &[],
            &["denoise"],
            &["ffmpeg", "install"],
            &["ffmpeg", "path"],
            &["doctor"],
            &["completions"],
        ];
        let mut out = String::new();
        for path in commands {
            let mut cmd = Cli::command().term_width(100);
            cmd.build();
            let mut c = &mut cmd;
            for p in *path {
                c = c.find_subcommand_mut(p).unwrap();
            }
            let title = std::iter::once("hush")
                .chain(path.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            out.push_str(&format!(
                "### `{title}`\n\n```text\n{}\n```\n\n",
                c.render_help().to_string().trim_end()
            ));
        }
        out
    }

    /// Fails when the README's command reference is stale.
    /// Refresh with: UPDATE_README=1 cargo test -p hush-cli readme
    #[test]
    fn readme_help_is_current() {
        const START: &str = "<!-- help:start -->\n";
        const END: &str = "<!-- help:end -->";
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/README.md");
        // Windows checkouts may use CRLF.
        let readme = std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
        let (a, rest) = readme.split_once(START).expect("help:start marker");
        let (_, b) = rest.split_once(END).expect("help:end marker");
        let fresh = format!("{a}{START}{}{END}{b}", help_reference());
        if std::env::var_os("UPDATE_README").is_some() {
            std::fs::write(path, &fresh).unwrap();
        } else {
            assert!(
                readme == fresh,
                "crates/hush-cli/README.md is out of date; run UPDATE_README=1 cargo test -p hush-cli readme"
            );
        }
    }

    #[test]
    fn completions_shell() {
        assert!(parse(&["completions", "zsh"]).is_ok());
        assert!(parse(&["completions", "tcsh"]).is_err());
    }
}
