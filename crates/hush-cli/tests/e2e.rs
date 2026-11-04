//! End-to-end: run the `hush` binary on files made from the golden sample.
//! Needs ffmpeg + ffprobe on PATH (or HUSH_FFMPEG); skipped otherwise.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const RAW: &str = concat!(
    env!("CARGO_WORKSPACE_DIR"),
    "/crates/hush-core/tests/fixtures/golden/sample_00006_raw.wav"
);

fn have_ffmpeg() -> bool {
    let ok = |b: &str| {
        Command::new(b)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let found = ok("ffmpeg") && ok("ffprobe");
    if !found {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
    }
    found
}

fn ffmpeg(args: &[&str]) {
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .status()
        .unwrap();
    assert!(st.success(), "ffmpeg {args:?}");
}

fn hush(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_hush"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

#[derive(Debug, PartialEq)]
struct Stream {
    kind: String,
    codec: String,
    rate: Option<u32>,
    channels: Option<u32>,
}

fn streams(p: &Path) -> (Vec<Stream>, f64) {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_streams",
            "-show_format",
        ])
        .arg(p)
        .output()
        .unwrap();
    assert!(out.status.success(), "ffprobe {}", p.display());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let s = v["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| Stream {
            kind: s["codec_type"].as_str().unwrap().into(),
            codec: s["codec_name"].as_str().unwrap().into(),
            rate: s["sample_rate"].as_str().and_then(|r| r.parse().ok()),
            channels: s["channels"].as_u64().map(|c| c as u32),
        })
        .collect();
    let d = v["format"]["duration"].as_str().unwrap().parse().unwrap();
    (s, d)
}

fn audio(s: &[Stream]) -> &Stream {
    s.iter().find(|s| s.kind == "audio").unwrap()
}

fn fixtures(dir: &Path) -> Vec<(PathBuf, &'static str, u32)> {
    let p = |n: &str| dir.join(n);
    let s = |p: &PathBuf| p.to_str().unwrap().to_string();
    let (mp3, m4a, mp4, wav, flac) = (p("a.mp3"), p("b.m4a"), p("c.mp4"), p("d.wav"), p("e.flac"));
    ffmpeg(&[
        "-i",
        RAW,
        "-ac",
        "2",
        "-ar",
        "44100",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "192k",
        &s(&mp3),
    ]);
    ffmpeg(&[
        "-i",
        RAW,
        "-ac",
        "2",
        "-ar",
        "48000",
        "-c:a",
        "aac",
        &s(&m4a),
    ]);
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=duration=5:size=160x120:rate=25",
        "-i",
        RAW,
        "-ac",
        "2",
        "-ar",
        "48000",
        "-c:v",
        "mpeg4",
        "-c:a",
        "aac",
        "-shortest",
        &s(&mp4),
    ]);
    ffmpeg(&["-i", RAW, "-ar", "48000", "-c:a", "pcm_s24le", &s(&wav)]);
    ffmpeg(&[
        "-i",
        RAW,
        "-ac",
        "2",
        "-ar",
        "44100",
        "-c:a",
        "flac",
        &s(&flac),
    ]);
    vec![
        (mp3, "mp3", 44100),
        (m4a, "aac", 48000),
        (mp4, "aac", 48000),
        (wav, "pcm_s24le", 48000),
        (flac, "flac", 44100),
    ]
}

fn denoised(p: &Path) -> PathBuf {
    let stem = p.file_stem().unwrap().to_string_lossy();
    let ext = p.extension().unwrap().to_string_lossy();
    p.with_file_name(format!("{stem}_denoised.{ext}"))
}

#[test]
fn formats_round_trip() {
    if !have_ffmpeg() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let files = fixtures(t.path());
    let out = hush(t.path(), &["denoise", ".", "-q"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    for (input, codec, rate) in files {
        let (in_s, in_d) = streams(&input);
        let (out_s, out_d) = streams(&denoised(&input));
        let a = audio(&out_s);
        assert_eq!(a.codec, codec, "{}", input.display());
        assert_eq!(a.rate, Some(rate), "{}", input.display());
        assert_eq!(a.channels, Some(1), "{}", input.display());
        assert!(
            (in_d - out_d).abs() < 0.1,
            "{}: {in_d} vs {out_d}",
            input.display()
        );
        let video = |s: &[Stream]| {
            s.iter()
                .filter(|s| s.kind == "video")
                .map(|s| s.codec.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(video(&in_s), video(&out_s), "{}", input.display());
    }
}

#[test]
fn batch_skip_fail_and_json() {
    if !have_ffmpeg() {
        return;
    }
    let t = tempfile::tempdir().unwrap();
    std::fs::copy(RAW, t.path().join("good.wav")).unwrap();
    std::fs::write(t.path().join("bad.mp3"), b"not audio").unwrap();

    let out = hush(
        t.path(),
        &["denoise", "good.wav", "bad.mp3", "--json", "-q"],
    );
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let status: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["status"].as_str().unwrap())
        .collect();
    assert_eq!(status, ["done", "failed"]);
    assert!(t.path().join("good_denoised.wav").exists());
    assert!(!t.path().join("bad_denoised.mp3").exists());

    // Second run skips; -y replaces.
    let out = hush(t.path(), &["denoise", "good.wav", "--json", "-q"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"skipped\""));
    let out = hush(t.path(), &["denoise", "good.wav", "-y", "--json", "-q"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"done\""));

    // No temp files left behind.
    let leftovers = std::fs::read_dir(t.path())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".hush-")
        })
        .count();
    assert_eq!(leftovers, 0);
}

#[test]
fn raw_matches_core_within_one_lsb() {
    if !have_ffmpeg() {
        return;
    }
    use hush_core::{DelayMode, ModelBundle, Session, Settings, StreamProcessor};

    let t = tempfile::tempdir().unwrap();
    std::fs::copy(RAW, t.path().join("in.wav")).unwrap();
    let out = hush(t.path(), &["denoise", "in.wav", "--raw", "-q"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let read = |p: &Path| -> Vec<i16> {
        hound::WavReader::open(p)
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect()
    };
    let cli = read(&t.path().join("in_denoised.wav"));
    let input: Vec<f32> = read(Path::new(RAW))
        .iter()
        .map(|&s| s as f32 / 32768.0)
        .collect();

    let dir = Path::new(env!("CARGO_WORKSPACE_DIR")).join("models/advanced_dfnet16k");
    let bundle = ModelBundle::from_directory(&dir).unwrap();
    let mut p = StreamProcessor::new(
        Session::new(&bundle, Settings::default()).unwrap(),
        DelayMode::Aligned,
    );
    let mut core = Vec::new();
    for c in input.chunks(8000) {
        p.push(c, &mut core).unwrap();
    }
    p.finish(&mut core).unwrap();

    assert_eq!(cli.len(), core.len());
    let max = cli
        .iter()
        .zip(&core)
        .map(|(&a, &b)| (a as f32 - (b * 32768.0).clamp(-32768.0, 32767.0)).abs())
        .fold(0.0, f32::max);
    assert!(max <= 1.0, "max diff {max} LSB");
}
