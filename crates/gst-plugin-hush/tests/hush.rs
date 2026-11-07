// Copyright (C) 2026 Umesh Timalsina <umesh.timalsina@vanderbilt.edu>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.

use std::path::Path;

use byte_slice_cast::*;
use gst::prelude::*;
use gst_check::Harness;
use hush_core::{DelayMode, ModelBundle, Session, Settings, StreamProcessor};

const RATE: u64 = 16000;
/// hop (160) + fft − hop (160) + lookahead (0) for advanced_dfnet16k.
const LATENCY: usize = 320;
const CAPS: &str = "audio/x-raw,format=F32LE,rate=16000,channels=1,layout=interleaved";
/// Uneven on purpose: the element must accept any buffer size.
const CHUNKS: [usize; 6] = [100, 333, 1024, 160, 7, 2000];

fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        gst::init().unwrap();
        gsthush::plugin_register_static().expect("register hush plugin");
    });
}

/// Speech-band tone plus deterministic noise.
fn signal(n: usize) -> Vec<f32> {
    let mut seed: u32 = 12345;
    (0..n)
        .map(|i| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            let t = i as f32 / RATE as f32;
            0.3 * (2.0 * std::f32::consts::PI * 220.0 * t).sin() + 0.2 * noise
        })
        .collect()
}

fn harness() -> Harness {
    init();
    let mut h = Harness::new("hush");
    h.set_src_caps_str(CAPS);
    h.play();
    h
}

fn buffer(samples: &[f32], pts: u64) -> gst::Buffer {
    let mut buf = gst::Buffer::from_mut_slice(samples.as_byte_slice().to_vec());
    {
        let buf = buf.get_mut().unwrap();
        buf.set_pts(gst::ClockTime::from_nseconds(pts * 1_000_000_000 / RATE));
    }
    buf
}

fn samples(buf: &gst::Buffer) -> Vec<f32> {
    let map = buf.map_readable().unwrap();
    map.as_slice_of::<f32>().unwrap().to_vec()
}

/// Run the whole signal through the element; return per-buffer outputs and the EOS tail.
fn run(h: &mut Harness, input: &[f32]) -> (Vec<gst::Buffer>, Vec<f32>) {
    let mut outs = Vec::new();
    let mut offset = 0;
    for &n in CHUNKS.iter().cycle() {
        if offset >= input.len() {
            break;
        }
        let end = (offset + n).min(input.len());
        h.push(buffer(&input[offset..end], offset as u64)).unwrap();
        outs.push(h.pull().unwrap());
        offset = end;
    }
    assert!(h.push_event(gst::event::Eos::new()));
    let tail = samples(&h.pull().expect("tail buffer after EOS"));
    (outs, tail)
}

#[test]
fn one_output_buffer_per_input_with_same_size_and_pts() {
    let mut h = harness();
    let input = signal(8000);
    let (outs, _) = run(&mut h, &input);

    let mut offset = 0u64;
    for (out, n) in outs.iter().zip(CHUNKS.iter().cycle()) {
        let n = (*n as u64).min(input.len() as u64 - offset);
        assert_eq!(out.size() as u64, n * 4);
        assert_eq!(
            out.pts(),
            Some(gst::ClockTime::from_nseconds(offset * 1_000_000_000 / RATE))
        );
        offset += n;
    }
}

#[test]
fn eos_pushes_the_latency_tail() {
    let mut h = harness();
    let input = signal(8000);
    let (outs, tail) = run(&mut h, &input);

    let total: usize = outs.iter().map(|b| b.size() / 4).sum::<usize>() + tail.len();
    assert_eq!(tail.len(), LATENCY);
    assert_eq!(total, input.len() + LATENCY);
}

#[test]
fn output_matches_hush_core_live_stream() {
    let mut h = harness();
    let input = signal(16000);
    let (outs, tail) = run(&mut h, &input);
    let mut got: Vec<f32> = outs.iter().flat_map(samples).collect();
    got.extend(tail);

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/advanced_dfnet16k");
    let bundle = ModelBundle::from_directory(&dir).unwrap();
    let mut proc = StreamProcessor::new(
        Session::new(&bundle, Settings::default()).unwrap(),
        DelayMode::Live,
    );
    let mut want = Vec::new();
    proc.push(&input, &mut want).unwrap();
    proc.finish(&mut want).unwrap();

    assert_eq!(got.len(), want.len());
    assert!(got == want, "element output differs from hush-core");
    // Sanity: the model did something.
    assert!(got[LATENCY..].iter().zip(&input).any(|(a, b)| a != b));
}

#[test]
fn reports_model_latency() {
    let h = harness();
    let latency = h.query_latency().expect("latency");
    assert_eq!(latency, gst::ClockTime::from_mseconds(20));
}

#[test]
fn atten_lim_db_round_trips_while_playing() {
    let mut h = harness();
    let element = h.element().unwrap();
    assert_eq!(element.property::<f32>("atten-lim-db"), 100.0);

    h.push(buffer(&signal(1600), 0)).unwrap();
    h.pull().unwrap();

    element.set_property("atten-lim-db", 12.0f32);
    assert_eq!(element.property::<f32>("atten-lim-db"), 12.0);

    h.push(buffer(&signal(1600), 1600)).unwrap();
    h.pull().unwrap();
}
