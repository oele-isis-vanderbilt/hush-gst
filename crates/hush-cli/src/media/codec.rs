//! How to write a file back: muxer, audio encoder and quality settings, chosen
//! from the input's extension and probed audio codec.

use std::path::Path;

use anyhow::{Result, bail};

use super::probe::MediaInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeSpec {
    /// ffmpeg `-f` value.
    pub muxer: &'static str,
    /// Audio encoder options (`-c:a ...` and quality flags).
    pub audio_args: Vec<String>,
    pub sample_rate: u32,
    /// Audio channels written: 1, or the input's count with `--per-channel`.
    pub channels: u32,
    /// Copy video streams from the input (video containers only).
    pub copy_video: bool,
    /// Copy subtitle streams from the input (Matroska only).
    pub copy_subtitles: bool,
    /// Copy global metadata and chapters from the input.
    pub copy_metadata: bool,
}

/// `--raw`: the model's own output, 16 kHz 16-bit WAV (mono unless `--per-channel`).
pub fn raw_spec(model_rate: u32, channels: u32) -> EncodeSpec {
    EncodeSpec {
        muxer: "wav",
        audio_args: args(&["-c:a", "pcm_s16le"]),
        sample_rate: model_rate,
        channels,
        copy_video: false,
        copy_subtitles: false,
        copy_metadata: false,
    }
}

/// Channels to write: the input's count with `--per-channel`, else mono.
pub fn channels_for(info: &MediaInfo, per_channel: bool) -> u32 {
    if per_channel {
        info.audio.channels.max(1)
    } else {
        1
    }
}

pub fn spec_for(input: &Path, info: &MediaInfo, per_channel: bool) -> Result<EncodeSpec> {
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let muxer = match ext.as_str() {
        "wav" => "wav",
        "flac" => "flac",
        "mp3" => "mp3",
        "m4a" => "ipod",
        "aac" => "adts",
        "mp4" => "mp4",
        "mov" => "mov",
        "mkv" => "matroska",
        "webm" => "webm",
        "ogg" | "oga" => "ogg",
        "opus" => "opus",
        "" => bail!("file has no extension, so the output format is unknown; use --raw"),
        other => bail!("unsupported file type .{other}; use --raw"),
    };
    let video_container = matches!(muxer, "mp4" | "mov" | "matroska" | "webm");

    let a = &info.audio;
    let bitrate = |fallback: u64| -> String {
        let bps = a.bit_rate.unwrap_or(fallback);
        format!("{}k", (bps / 1000).max(8))
    };
    let codec = a.codec_name.as_str();
    let audio_args = match codec {
        c if c.starts_with("pcm_") => args(&["-c:a", c]),
        "flac" => {
            // s32 input is 24-bit FLAC; ffmpeg's encoder writes 24 bits for s32.
            let fmt = match a.sample_fmt.as_deref() {
                Some("s32") | Some("s32p") => "s32",
                _ => "s16",
            };
            args(&["-c:a", "flac", "-sample_fmt", fmt])
        }
        "mp3" => vec![
            "-c:a".into(),
            "libmp3lame".into(),
            "-b:a".into(),
            bitrate(192_000),
        ],
        "aac" => vec!["-c:a".into(), "aac".into(), "-b:a".into(), bitrate(128_000)],
        "alac" => args(&["-c:a", "alac"]),
        "opus" => vec![
            "-c:a".into(),
            "libopus".into(),
            "-b:a".into(),
            bitrate(64_000),
        ],
        "vorbis" => vec![
            "-c:a".into(),
            "libvorbis".into(),
            "-b:a".into(),
            bitrate(128_000),
        ],
        other => bail!("unsupported audio codec `{other}`; use --raw"),
    };

    Ok(EncodeSpec {
        muxer,
        audio_args,
        sample_rate: a.sample_rate,
        channels: channels_for(info, per_channel),
        copy_video: video_container && info.video_streams > 0,
        copy_subtitles: muxer == "matroska" && info.subtitle_streams > 0,
        copy_metadata: true,
    })
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::probe::AudioStream;

    fn info(codec: &str, rate: u32, bit_rate: Option<u64>, video: usize) -> MediaInfo {
        MediaInfo {
            duration: None,
            audio: AudioStream {
                codec_name: codec.into(),
                sample_rate: rate,
                channels: 2,
                bit_rate,
                sample_fmt: None,
                duration: None,
            },
            audio_streams: 1,
            video_streams: video,
            subtitle_streams: 0,
        }
    }

    #[test]
    fn mp3_keeps_bitrate_and_rate() {
        let s = spec_for(
            Path::new("a.MP3"),
            &info("mp3", 44100, Some(256_000), 0),
            false,
        )
        .unwrap();
        assert_eq!(s.muxer, "mp3");
        assert_eq!(s.audio_args, ["-c:a", "libmp3lame", "-b:a", "256k"]);
        assert_eq!(s.sample_rate, 44100);
        assert!(!s.copy_video);
    }

    #[test]
    fn mp4_copies_video() {
        let s = spec_for(Path::new("a.mp4"), &info("aac", 48000, None, 1), false).unwrap();
        assert_eq!(s.muxer, "mp4");
        assert_eq!(s.audio_args, ["-c:a", "aac", "-b:a", "128k"]);
        assert!(s.copy_video);
    }

    #[test]
    fn m4a_never_copies_video() {
        let s = spec_for(Path::new("a.m4a"), &info("aac", 48000, None, 1), false).unwrap();
        assert_eq!(s.muxer, "ipod");
        assert!(!s.copy_video);
    }

    #[test]
    fn pcm_keeps_codec() {
        let s = spec_for(
            Path::new("a.wav"),
            &info("pcm_s24le", 48000, None, 0),
            false,
        )
        .unwrap();
        assert_eq!(s.audio_args, ["-c:a", "pcm_s24le"]);
    }

    #[test]
    fn per_channel_keeps_channels() {
        let i = info("aac", 48000, None, 1);
        let s = spec_for(Path::new("a.mp4"), &i, true).unwrap();
        assert_eq!(s.channels, 2);
        assert_eq!(s.sample_rate, 48000);
        assert_eq!(spec_for(Path::new("a.mp4"), &i, false).unwrap().channels, 1);
        assert_eq!(raw_spec(16000, channels_for(&i, true)).channels, 2);
    }

    #[test]
    fn unsupported_is_an_error() {
        assert!(spec_for(Path::new("a.xyz"), &info("aac", 48000, None, 0), false).is_err());
        assert!(spec_for(Path::new("a.mkv"), &info("dts", 48000, None, 0), false).is_err());
        assert!(spec_for(Path::new("a"), &info("aac", 48000, None, 0), false).is_err());
    }
}
