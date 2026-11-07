//! Shared helpers for integration tests. Use with `mod common;`.

// Each test file uses only some of these.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

pub const SAMPLE_RATE: u32 = 16_000;

/// Path to a file in `tests/fixtures/golden/`.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
        .join(name)
}

/// Read a mono, 16 kHz, 16-bit PCM WAV file. Panics on any other format.
pub fn read_i16(path: &Path) -> Vec<i16> {
    let mut reader =
        WavReader::open(path).unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
    let spec = reader.spec();
    assert_eq!(spec.channels, 1, "{}: expected mono", path.display());
    assert_eq!(
        spec.sample_rate,
        SAMPLE_RATE,
        "{}: expected 16 kHz",
        path.display()
    );
    assert_eq!(
        spec.bits_per_sample,
        16,
        "{}: expected 16-bit",
        path.display()
    );
    assert_eq!(
        spec.sample_format,
        SampleFormat::Int,
        "{}: expected PCM int",
        path.display()
    );
    reader
        .samples::<i16>()
        .map(|s| s.expect("failed to read sample"))
        .collect()
}

/// Write samples as a mono, 16 kHz, 16-bit PCM WAV file.
pub fn write_i16(path: &Path, samples: &[i16]) {
    let spec = WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)
        .unwrap_or_else(|e| panic!("cannot create {}: {e}", path.display()));
    for &s in samples {
        writer.write_sample(s).expect("failed to write sample");
    }
    writer.finalize().expect("failed to finalize wav");
}

/// int16 -> f32 in [-1, 1). Same as `denoise_ctypes.py`: x / 32768.
pub fn to_f32(samples: &[i16]) -> Vec<f32> {
    samples.iter().map(|&s| s as f32 / 32768.0).collect()
}

/// f32 -> int16 the way `denoise_ctypes.py` does it:
/// `(y * 32768).clip(-32768, 32767).astype(int16)`, which truncates toward zero.
pub fn to_i16_trunc(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&y| (y * 32768.0).clamp(-32768.0, 32767.0) as i16)
        .collect()
}

/// f32 -> int16, scale by 32768 and round down. This is the rule the upstream
/// golden (`sample_00006_denoised.wav`) matches: 88% of samples identical, no
/// mean offset. trunc and round leave a +0.5 LSB offset.
pub fn to_i16_floor(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&y| (y * 32768.0).floor().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

/// f32 -> int16 the way libsndfile (soundfile / torchaudio, used by
/// `infer_single.py`) does it: scale by 32767 and round.
pub fn to_i16_round(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|&y| (y * 32767.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}
