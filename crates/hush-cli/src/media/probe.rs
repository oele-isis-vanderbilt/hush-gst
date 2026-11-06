//! `ffprobe -show_format -show_streams` as typed data.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::ffmpeg::Tools;

/// The file has no audio stream. A distinct type so `--skip-no-audio` can tell
/// it apart from other probe failures.
#[derive(Debug)]
pub struct NoAudio;

impl std::fmt::Display for NoAudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("file has no audio stream")
    }
}

impl std::error::Error for NoAudio {}

#[derive(Debug, Clone)]
pub struct MediaInfo {
    pub duration: Option<f64>,
    /// First audio stream.
    pub audio: AudioStream,
    pub audio_streams: usize,
    /// Video streams that are not cover art.
    pub video_streams: usize,
    pub subtitle_streams: usize,
}

#[derive(Debug, Clone)]
pub struct AudioStream {
    pub codec_name: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub bit_rate: Option<u64>,
    pub sample_fmt: Option<String>,
    pub duration: Option<f64>,
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    streams: Vec<RawStream>,
    format: Option<RawFormat>,
}

#[derive(Deserialize)]
struct RawStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    sample_rate: Option<String>,
    channels: Option<u32>,
    bit_rate: Option<String>,
    sample_fmt: Option<String>,
    duration: Option<String>,
    #[serde(default)]
    disposition: Disposition,
}

#[derive(Deserialize, Default)]
struct Disposition {
    #[serde(default)]
    attached_pic: u8,
}

#[derive(Deserialize)]
struct RawFormat {
    duration: Option<String>,
}

pub fn probe(tools: &Tools, path: &Path) -> Result<MediaInfo> {
    let out = tools
        .ffprobe()
        .args(["-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .context("running ffprobe")?;
    if !out.status.success() {
        bail!(
            "not a readable media file: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse(&out.stdout)
}

fn parse(json: &[u8]) -> Result<MediaInfo> {
    let raw: Raw = serde_json::from_slice(json).context("parsing ffprobe output")?;
    let format = raw.format.context("ffprobe reported no format")?;
    let kind = |s: &RawStream, k: &str| s.codec_type.as_deref() == Some(k);

    let audio: Vec<&RawStream> = raw.streams.iter().filter(|s| kind(s, "audio")).collect();
    let Some(first) = audio.first() else {
        return Err(NoAudio.into());
    };
    let sample_rate = first
        .sample_rate
        .as_deref()
        .and_then(|s| s.parse().ok())
        .filter(|&r| r > 0)
        .context("audio stream has no sample rate")?;

    Ok(MediaInfo {
        duration: num(&format.duration),
        audio: AudioStream {
            codec_name: first.codec_name.clone().unwrap_or_default(),
            sample_rate,
            channels: first.channels.unwrap_or(0),
            bit_rate: num(&first.bit_rate),
            sample_fmt: first.sample_fmt.clone(),
            duration: num(&first.duration),
        },
        audio_streams: audio.len(),
        video_streams: raw
            .streams
            .iter()
            .filter(|s| kind(s, "video") && s.disposition.attached_pic == 0)
            .count(),
        subtitle_streams: raw.streams.iter().filter(|s| kind(s, "subtitle")).count(),
    })
}

fn num<T: std::str::FromStr>(s: &Option<String>) -> Option<T> {
    s.as_deref().and_then(|s| s.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mp4_with_cover_and_video() {
        let json = br#"{
          "streams": [
            {"codec_type":"video","codec_name":"h264","disposition":{"attached_pic":0}},
            {"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,
             "bit_rate":"128000","sample_fmt":"fltp","duration":"5.0"},
            {"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}},
            {"codec_type":"audio","codec_name":"aac","sample_rate":"44100","channels":1}
          ],
          "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"5.02","bit_rate":"500000"}
        }"#;
        let m = parse(json).unwrap();
        assert_eq!(m.audio.codec_name, "aac");
        assert_eq!(m.audio.sample_rate, 48000);
        assert_eq!(m.audio.channels, 2);
        assert_eq!(m.audio.bit_rate, Some(128000));
        assert_eq!(m.audio_streams, 2);
        assert_eq!(m.video_streams, 1);
        assert_eq!(m.duration, Some(5.02));
    }

    #[test]
    fn no_audio_is_an_error() {
        let json = br#"{"streams":[{"codec_type":"video"}],"format":{"format_name":"mp4"}}"#;
        assert!(parse(json).unwrap_err().is::<NoAudio>());
    }
}
