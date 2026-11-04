mod cli;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use cli::{Cli, Command, FfmpegCommand};

/// Exit codes. clap itself exits with 2 on usage errors.
pub mod exit {
    pub const OK: u8 = 0;
    /// One or more files failed; the others were processed.
    pub const FILES_FAILED: u8 = 1;
    /// Setup problem (ffmpeg missing, model invalid, ...).
    pub const SETUP: u8 = 3;
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(exit::SETUP)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<u8> {
    match cli.command {
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
            Ok(exit::OK)
        }
        Command::Denoise(_) => not_implemented("denoise"),
        Command::Ffmpeg(FfmpegCommand::Install { .. }) => not_implemented("ffmpeg install"),
        Command::Ffmpeg(FfmpegCommand::Path) => not_implemented("ffmpeg path"),
        Command::Doctor => not_implemented("doctor"),
    }
}

fn not_implemented(what: &str) -> anyhow::Result<u8> {
    anyhow::bail!("`hush {what}` is not implemented yet")
}
