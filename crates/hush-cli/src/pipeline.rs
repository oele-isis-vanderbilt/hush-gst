//! One file: ffmpeg decoder -> StreamProcessor -> ffmpeg encoder, all streaming.
//!
//! ```text
//! input ──ffmpeg──▶ f32le 16 kHz mono ──hush──▶ f32le 16 kHz mono ──ffmpeg──▶ temp file ──rename──▶ output
//!                                                    (+ video / metadata copied from input)
//! ```
//!
//! With `--per-channel` the PCM carries N interleaved channels; each channel goes
//! through its own `StreamProcessor` (Aligned), so the channels stay in sync.

use std::collections::VecDeque;
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

/// Frames read from the decoder per step (0.5 s at 16 kHz).
const BLOCK: usize = 8000;

pub static CANCEL: AtomicBool = AtomicBool::new(false);

pub struct Ctx<'a> {
    pub tools: &'a Tools,
    pub bundle: &'a ModelBundle,
    pub settings: Settings,
    pub raw: bool,
    pub per_channel: bool,
    pub overwrite: bool,
    pub skip_no_audio: bool,
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
        let channels = codec::channels_for(&info, ctx.per_channel);
        codec::raw_spec(ctx.bundle.info().sample_rate as u32, channels)
    } else {
        codec::spec_for(&job.input, &info, ctx.per_channel)?
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

    // Decoder: ffmpeg does the downmix (unless per channel) and the resampling.
    let channels = spec.channels.max(1) as usize;
    let mut dec_cmd = ctx.tools.ffmpeg();
    dec_cmd
        .arg("-i")
        .arg(&job.input)
        .args(["-map", "0:a:0", "-vn", "-sn", "-dn"])
        .args(["-ac", &channels.to_string()])
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

    let result = stream(ctx, &mut dec, &mut enc, model_rate, channels, progress);
    let frames = match result {
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
    if frames == 0 {
        bail!("decoder produced no audio");
    }

    // Output length must match: Aligned mode keeps sample counts equal, so any
    // larger gap means the encoder dropped audio.
    let seconds = frames as f64 / model_rate as f64;
    let out_info = probe::probe(ctx.tools, &tmp).context("checking the written file")?;
    if out_info.audio.channels != spec.channels {
        bail!(
            "output has {} channels, expected {}",
            out_info.audio.channels,
            spec.channels
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
    let channels = spec.channels.to_string();
    let mut c = tools.ffmpeg();
    c.arg("-y").args([
        "-f",
        "f32le",
        "-ar",
        &model_rate.to_string(),
        "-ac",
        &channels,
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
        .args(["-ar", &spec.sample_rate.to_string(), "-ac", &channels])
        .args(["-f", spec.muxer])
        .arg(out);
    c
}

/// Pump decoder -> model -> encoder. `channels` interleaved channels, each
/// denoised by its own processor. Returns frames written (samples per channel).
fn stream(
    ctx: &Ctx,
    dec: &mut Proc,
    enc: &mut Proc,
    model_rate: usize,
    channels: usize,
    progress: &dyn Fn(f64),
) -> Result<u64> {
    let mut procs = (0..channels)
        .map(|_| {
            Ok(StreamProcessor::new(
                Session::new(ctx.bundle, ctx.settings)?,
                DelayMode::Aligned,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut reader = dec.child.stdout.take().context("decoder stdout")?;
    let mut writer = enc.child.stdin.take().context("encoder stdin")?;

    let frame_bytes = 4 * channels;
    let mut bytes = vec![0u8; BLOCK * frame_bytes];
    let mut have = 0; // bytes in `bytes`, may end mid-frame
    let mut input: Vec<Vec<f32>> = vec![Vec::with_capacity(BLOCK); channels];
    let mut output = Vec::with_capacity(BLOCK * 2);
    let mut ready: Vec<VecDeque<f32>> = vec![VecDeque::new(); channels];
    let mut out_bytes = Vec::with_capacity(BLOCK * 8 * channels);
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
        let whole = have / frame_bytes * frame_bytes;
        for ch in input.iter_mut() {
            ch.clear();
        }
        for (i, b) in bytes[..whole].as_chunks::<4>().0.iter().enumerate() {
            input[i % channels].push(f32::from_le_bytes(*b));
        }
        bytes.copy_within(whole..have, 0);
        have -= whole;

        for ((p, inp), q) in procs.iter_mut().zip(&input).zip(&mut ready) {
            output.clear();
            p.push(inp, &mut output)?;
            q.extend(&output);
        }
        total_out += write_frames(&mut writer, &mut ready, &mut out_bytes)?;
        total_in += input[0].len() as u64;
        progress(total_in as f64 / model_rate as f64);
    }

    for (p, q) in procs.iter_mut().zip(&mut ready) {
        output.clear();
        p.finish(&mut output)?;
        q.extend(&output);
    }
    total_out += write_frames(&mut writer, &mut ready, &mut out_bytes)?;
    if ready.iter().any(|q| !q.is_empty()) {
        bail!("channels ended with different lengths");
    }
    writer.flush().context("writing to encoder")?;
    drop(writer); // EOF for the encoder
    Ok(total_out)
}

/// Write the frames every channel has ready, interleaved. Returns frames written.
fn write_frames(w: &mut impl Write, ready: &mut [VecDeque<f32>], buf: &mut Vec<u8>) -> Result<u64> {
    let n = ready.iter().map(VecDeque::len).min().unwrap_or(0);
    buf.clear();
    for _ in 0..n {
        for q in ready.iter_mut() {
            let s = q.pop_front().unwrap_or_default();
            buf.extend_from_slice(&s.to_le_bytes());
        }
    }
    w.write_all(buf).context("writing to encoder")?;
    Ok(n as u64)
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
