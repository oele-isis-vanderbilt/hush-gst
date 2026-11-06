//! Progress bars, per-file lines and the end summary. Bars and messages go to
//! stderr; `--json` output goes to stdout.

use std::path::PathBuf;
use std::time::Duration;

use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Done,
    Skipped,
    Failed,
    Planned,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub input: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
    pub elapsed_s: f64,
}

pub struct Reporter {
    mp: MultiProgress,
    total: ProgressBar,
    quiet: bool,
    verbose: u8,
    bars: bool,
}

impl Reporter {
    pub fn new(files: usize, quiet: bool, verbose: u8, json: bool) -> Self {
        let bars = !quiet && !json && console::Term::stderr().is_term();
        let mp = MultiProgress::with_draw_target(if bars {
            ProgressDrawTarget::stderr()
        } else {
            ProgressDrawTarget::hidden()
        });
        let total = mp.add(ProgressBar::new(files as u64));
        total.set_style(
            ProgressStyle::with_template("{prefix:.bold} [{bar:30}] {pos}/{len} files  {elapsed}")
                .unwrap()
                .progress_chars("=> "),
        );
        total.set_prefix("hush");
        Self {
            mp,
            total,
            quiet,
            verbose,
            bars,
        }
    }

    /// A bar for one file, `seconds` long (0 if unknown).
    pub fn file_bar(&self, name: &str, seconds: f64) -> ProgressBar {
        let bar = if seconds > 0.0 {
            let b = ProgressBar::new((seconds * 1000.0) as u64);
            b.set_style(
                ProgressStyle::with_template("  [{bar:20}] {percent:>3}% {wide_msg}")
                    .unwrap()
                    .progress_chars("=> "),
            );
            b
        } else {
            let b = ProgressBar::new_spinner();
            b.enable_steady_tick(Duration::from_millis(120));
            b
        };
        bar.set_message(name.to_string());
        self.mp.insert_before(&self.total, bar)
    }

    /// Count a file that needed no bar (skipped).
    pub fn tick(&self) {
        self.total.inc(1);
    }

    pub fn file_done(&self, bar: ProgressBar) {
        bar.finish_and_clear();
        self.mp.remove(&bar);
        self.total.inc(1);
    }

    /// Normal output (hidden with --quiet).
    pub fn info(&self, msg: impl AsRef<str>) {
        if !self.quiet {
            self.line(msg.as_ref());
        }
    }

    /// Only with -v.
    pub fn debug(&self, msg: impl AsRef<str>) {
        if self.verbose > 0 && !self.quiet {
            self.line(msg.as_ref());
        }
    }

    /// Always shown.
    pub fn error(&self, msg: impl AsRef<str>) {
        self.line(&format!(
            "{} {}",
            style("error:").red().bold(),
            msg.as_ref()
        ));
    }

    pub fn warn(&self, msg: impl AsRef<str>) {
        if !self.quiet {
            self.line(&format!(
                "{} {}",
                style("warning:").yellow().bold(),
                msg.as_ref()
            ));
        }
    }

    pub fn outcome(&self, o: &Outcome) {
        let input = o
            .input
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        match o.status {
            Status::Done => {
                let output = o
                    .output
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let audio = o
                    .duration_s
                    .map(|d| format!("{d:.1}s audio, "))
                    .unwrap_or_default();
                self.info(format!(
                    "{} {input} -> {output} ({audio}{:.1}s)",
                    style("done").green(),
                    o.elapsed_s
                ));
            }
            Status::Skipped => self.info(format!(
                "{} {input} ({})",
                style("skip").yellow(),
                o.error.as_deref().unwrap_or("")
            )),
            Status::Failed => self.error(format!(
                "{input}{}{}",
                if input.is_empty() { "" } else { ": " },
                o.error.as_deref().unwrap_or("failed")
            )),
            Status::Planned => {}
        }
        for w in &o.warnings {
            self.warn(format!("{input}: {w}"));
        }
    }

    pub fn summary(&self, outcomes: &[Outcome]) {
        self.total.finish_and_clear();
        let count = |s| outcomes.iter().filter(|o| o.status == s).count();
        let (done, skipped, failed) = (
            count(Status::Done),
            count(Status::Skipped),
            count(Status::Failed),
        );
        if self.quiet {
            return;
        }
        let exists = outcomes
            .iter()
            .filter(|o| o.status == Status::Skipped && o.error.as_deref() == Some("output exists"))
            .count();
        let mut parts = vec![format!("{done} done")];
        if exists > 0 {
            parts.push(format!("{exists} skipped (exists, use -y to replace)"));
        }
        if skipped > exists {
            parts.push(format!("{} skipped (no audio)", skipped - exists));
        }
        if failed > 0 {
            parts.push(style(format!("{failed} failed")).red().to_string());
        }
        self.line(&parts.join(", "));
    }

    fn line(&self, msg: &str) {
        if self.bars {
            let _ = self.mp.println(msg);
        } else {
            eprintln!("{msg}");
        }
    }
}
