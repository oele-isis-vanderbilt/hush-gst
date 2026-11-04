//! One file: ffmpeg decoder -> StreamProcessor -> ffmpeg encoder, all streaming.
//!
//! ```text
//! input ──ffmpeg──▶ f32le 16 kHz mono ──hush──▶ f32le 16 kHz mono ──ffmpeg──▶ temp file ──rename──▶ output
//!                                                    (+ video / metadata copied from input)
//! ```

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow, bail};
use hush_core::{DelayMode, ModelBundle, Session, Settings, StreamProcessor};

use crate::ffmpeg::Tools;
use crate::inputs::Job;
use crate::media::codec::{self, EncodeSpec};
use crate::media::probe::{self, MediaInfo};

/// Samples read from the decoder per step (0.5 s at 16 kHz).
const BLOCK: usize = 8000;

pub static CANCEL: AtomicBool = AtomicBool::new(false);

pub struct Ctx<'a> {
    pub tools: &'a Tools,
    pub bundle: &'a ModelBundle,
    pub settings: Settings,
    pub raw: bool,
    pub overwrite: bool,
}

pub struct Done {
    /// Seconds of audio processed.
    pub seconds: f64,
    pub warnings: Vec<String>,
}

/// Probe first, so the caller can size the progress bar.
pub fn prepare(ctx: &Ctx, job: &Job) -> Result<(MediaInfo, EncodeSpec)> {
    let info = probe::probe(ctx.tools, &job.input)?;
    let spec = if ctx.raw {
        codec::raw_spec(ctx.bundle.info().sample_rate as u32)
    } else {
        codec::spec_for(&job.input, &info)?
    };
    Ok((info, spec))
}

/// `progress` gets the seconds of audio processed so far.
pub fn run(
    ctx: &Ctx,
    job: &Job,
    info: &MediaInfo,
    spec: &EncodeSpec,
    progress: &dyn Fn(f64),
) -> Result<Done> {
    let mut warnings = Vec::new();
    if info.audio_streams > 1 {
        warnings.push(format!(
            "{} audio streams; only the first is denoised and kept",
            info.audio_streams
        ));
    }

    let model_rate = ctx.bundle.info().sample_rate;
    let out_dir = match job.output.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let ext = job
        .output
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("tmp");
    let tmp = tempfile::Builder::new()
        .prefix(".hush-")
        .suffix(&format!(".{ext}"))
        .tempfile_in(out_dir)
        .with_context(|| format!("creating a temp file in {}", out_dir.display()))?
        .into_temp_path();

    // Decoder: ffmpeg does the downmix and the resampling.
    let mut dec_cmd = ctx.tools.ffmpeg();
    dec_cmd
        .arg("-i")
        .arg(&job.input)
        .args(["-map", "0:a:0", "-vn", "-sn", "-dn", "-ac", "1"])
        .args(["-ar", &model_rate.to_string(), "-f", "f32le", "pipe:1"]);
    let mut dec = Proc::spawn(dec_cmd, false, true, "decoder")?;

    let enc_cmd = encoder_command(ctx.tools, &job.input, spec, model_rate, &tmp);
    let mut enc = match Proc::spawn(enc_cmd, true, false, "encoder") {
        Ok(p) => p,
        Err(e) => {
            dec.kill();
            return Err(e);
        }
    };

    let result = stream(ctx, &mut dec, &mut enc, model_rate, progress);
    let samples = match result {
        Ok(n) => n,
        Err(e) => {
            dec.kill();
            let _ = dec.wait();
            if CANCEL.load(Ordering::SeqCst) {
                enc.kill();
                bail!("cancelled");
            }
            // The encoder's stdin is closed now, so it exits. If it failed, its
            // log explains more than our broken pipe does.
            return Err(enc.wait().err().unwrap_or(e));
        }
    };
    dec.wait()?;
    enc.wait()?;
    if samples == 0 {
        bail!("decoder produced no audio");
    }

    // Output length must match: Aligned mode keeps sample counts equal, so any
    // larger gap means the encoder dropped audio.
    let seconds = samples as f64 / model_rate as f64;
    let out_info = probe::probe(ctx.tools, &tmp).context("checking the written file")?;
    if out_info.audio.channels != 1 {
        bail!(
            "output has {} channels, expected 1",
            out_info.audio.channels
        );
    }
    if let Some(d) = out_info.audio.duration.or(out_info.duration) {
        let tolerance = (seconds * 0.01).max(0.5);
        if (d - seconds).abs() > tolerance {
            bail!("output is {d:.2}s long, expected {seconds:.2}s");
        }
    }

    if ctx.overwrite {
        tmp.persist(&job.output)
    } else {
        tmp.persist_noclobber(&job.output)
    }
    .map_err(|e| anyhow!("writing {}: {}", job.output.display(), e.error))?;

    Ok(Done { seconds, warnings })
}

fn encoder_command(
    tools: &Tools,
    input: &Path,
    spec: &EncodeSpec,
    model_rate: usize,
    out: &Path,
) -> Command {
    let mut c = tools.ffmpeg();
    c.arg("-y").args([
        "-f",
        "f32le",
        "-ar",
        &model_rate.to_string(),
        "-ac",
        "1",
        "-i",
        "pipe:0",
    ]);
    let needs_input = spec.copy_video || spec.copy_subtitles || spec.copy_metadata;
    if needs_input {
        c.arg("-i").arg(input);
    }
    if spec.copy_video {
        // Video first, as in most files. Capital V: real video only, not cover art.
        c.args(["-map", "1:V?", "-c:v", "copy"]);
    }
    c.args(["-map", "0:a:0"]);
    if spec.copy_subtitles {
        c.args(["-map", "1:s?", "-c:s", "copy"]);
    }
    if spec.copy_metadata {
        c.args(["-map_metadata", "1", "-map_chapters", "1"]);
    } else {
        c.args(["-map_metadata", "-1"]);
    }
    c.args(&spec.audio_args)
        .args(["-ar", &spec.sample_rate.to_string(), "-ac", "1"])
        .args(["-f", spec.muxer])
        .arg(out);
    c
}

/// Pump decoder -> model -> encoder. Returns samples written.
fn stream(
    ctx: &Ctx,
    dec: &mut Proc,
    enc: &mut Proc,
    model_rate: usize,
    progress: &dyn Fn(f64),
) -> Result<u64> {
    let session = Session::new(ctx.bundle, ctx.settings)?;
    let mut proc = StreamProcessor::new(session, DelayMode::Aligned);

    let mut reader = dec.child.stdout.take().context("decoder stdout")?;
    let mut writer = enc.child.stdin.take().context("encoder stdin")?;

    let mut bytes = vec![0u8; BLOCK * 4];
    let mut have = 0; // bytes in `bytes`, may end mid-sample
    let mut input = Vec::with_capacity(BLOCK);
    let mut output = Vec::with_capacity(BLOCK * 2);
    let mut out_bytes = Vec::with_capacity(BLOCK * 8);
    let mut total_in: u64 = 0;
    let mut total_out: u64 = 0;

    loop {
        if CANCEL.load(Ordering::SeqCst) {
            bail!("cancelled");
        }
        let n = reader
            .read(&mut bytes[have..])
            .context("reading from decoder")?;
        if n == 0 {
            break;
        }
        have += n;
        let whole = have / 4 * 4;
        input.clear();
        input.extend(
            bytes[..whole]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b)),
        );
        bytes.copy_within(whole..have, 0);
        have -= whole;

        output.clear();
        proc.push(&input, &mut output)?;
        write_f32(&mut writer, &output, &mut out_bytes)?;
        total_in += input.len() as u64;
        total_out += output.len() as u64;
        progress(total_in as f64 / model_rate as f64);
    }

    output.clear();
    proc.finish(&mut output)?;
    write_f32(&mut writer, &output, &mut out_bytes)?;
    total_out += output.len() as u64;
    writer.flush().context("writing to encoder")?;
    drop(writer); // EOF for the encoder
    Ok(total_out)
}

fn write_f32(w: &mut impl Write, samples: &[f32], buf: &mut Vec<u8>) -> Result<()> {
    buf.clear();
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    w.write_all(buf).context("writing to encoder")
}

/// A child ffmpeg with its stderr drained on a thread (so it never blocks).
struct Proc {
    child: Child,
    stderr: Option<JoinHandle<String>>,
    role: &'static str,
}

impl Proc {
    fn spawn(mut cmd: Command, stdin: bool, stdout: bool, role: &'static str) -> Result<Self> {
        cmd.stdin(if stdin { Stdio::piped() } else { Stdio::null() })
            .stdout(if stdout {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("starting ffmpeg {role}"))?;
        let stderr = child.stderr.take().map(drain);
        Ok(Self {
            child,
            stderr,
            role,
        })
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
    }

    fn wait(&mut self) -> Result<()> {
        let status = self.child.wait().context("waiting for ffmpeg")?;
        let log = self
            .stderr
            .take()
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        if status.success() {
            return Ok(());
        }
        let log = log.trim();
        if log.is_empty() {
            bail!("ffmpeg {} failed ({status})", self.role)
        }
        bail!("ffmpeg {} failed: {log}", self.role)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Keep the last 4 KB of a child's stderr.
fn drain(mut err: ChildStderr) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut all = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n) = err.read(&mut buf) {
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
            if all.len() > 8192 {
                all.drain(..all.len() - 4096);
            }
        }
        String::from_utf8_lossy(&all).into_owned()
    })
}
