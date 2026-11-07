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
const CAPS: &str = "audio/x-raw,format=F32LE,rate=16000,channels=1,layout=interleaved";
/// Uneven on purpose: the element must accept any buffer size.
const CHUNKS: [usize; 6] = [100, 333, 1024, 160, 7, 2000];
/// Streams start here, not at 0, to show output is anchored to the input PTS.
const START: u64 = RATE; // 1 s, in samples

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

fn time(samples: u64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(samples * 1_000_000_000 / RATE)
}

fn harness() -> Harness {
    init();
    let mut h = Harness::new("hush");
    h.set_src_caps_str(CAPS);
    h.play();
    h
}

fn buffer(samples: &[f32], at: u64, discont: bool) -> gst::Buffer {
    let mut buf = gst::Buffer::from_mut_slice(samples.as_byte_slice().to_vec());
    {
        let buf = buf.get_mut().unwrap();
        buf.set_pts(time(at));
        buf.set_duration(time(at + samples.len() as u64) - time(at));
        // Offsets are sample indices, as decoders and wavparse set them.
        buf.set_offset(at);
        buf.set_offset_end(at + samples.len() as u64);
        if discont {
            buf.set_flags(gst::BufferFlags::DISCONT);
        }
    }
    buf
}

fn samples(buf: &gst::Buffer) -> Vec<f32> {
    let map = buf.map_readable().unwrap();
    map.as_slice_of::<f32>().unwrap().to_vec()
}

fn pull_all(h: &mut Harness, into: &mut Vec<gst::Buffer>) {
    while let Some(b) = h.try_pull() {
        into.push(b);
    }
}

/// Push `input` starting at sample time `at`, in uneven chunks.
fn push_stream(h: &mut Harness, input: &[f32], at: u64, outs: &mut Vec<gst::Buffer>) {
    let mut offset = 0;
    for &n in CHUNKS.iter().cycle() {
        if offset >= input.len() {
            break;
        }
        let end = (offset + n).min(input.len());
        let pts = at + offset as u64;
        h.push(buffer(&input[offset..end], pts, offset == 0))
            .unwrap();
        pull_all(h, outs);
        offset = end;
    }
}

/// One whole stream through the element, ending in EOS.
fn run(h: &mut Harness, input: &[f32]) -> Vec<gst::Buffer> {
    let mut outs = Vec::new();
    push_stream(h, input, START, &mut outs);
    assert!(h.push_event(gst::event::Eos::new()));
    pull_all(h, &mut outs);
    outs
}

/// Output buffers must tile time with no gaps or overlaps, starting at `at`.
fn assert_contiguous(outs: &[gst::Buffer], at: u64) {
    let mut n = at;
    for b in outs {
        assert_eq!(b.pts(), Some(time(n)), "buffer at sample {n}");
        let len = (b.size() / 4) as u64;
        assert_eq!(b.duration(), Some(time(n + len) - time(n)));
        n += len;
    }
}

#[test]
fn output_is_timestamped_like_the_input() {
    let mut h = harness();
    let input = signal(8000);
    let outs = run(&mut h, &input);

    assert!(outs[0].flags().contains(gst::BufferFlags::DISCONT));
    assert_contiguous(&outs, START);
}

#[test]
fn output_length_equals_input_length() {
    let mut h = harness();
    let input = signal(8000);
    let outs = run(&mut h, &input);

    let total: usize = outs.iter().map(|b| b.size() / 4).sum();
    assert_eq!(total, input.len());
}

#[test]
fn output_matches_hush_core_aligned_stream() {
    let mut h = harness();
    let input = signal(16000);
    let got: Vec<f32> = run(&mut h, &input).iter().flat_map(samples).collect();

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/advanced_dfnet16k");
    let bundle = ModelBundle::from_directory(&dir).unwrap();
    let mut proc = StreamProcessor::new(
        Session::new(&bundle, Settings::default()).unwrap(),
        DelayMode::Aligned,
    );
    let mut want = Vec::new();
    proc.push(&input, &mut want).unwrap();
    proc.finish(&mut want).unwrap();

    assert_eq!(got.len(), want.len());
    assert!(got == want, "element output differs from hush-core");
    // Sanity: the model did something.
    assert!(got.iter().zip(&input).any(|(a, b)| a != b));
}

#[test]
fn discont_ends_the_stream_and_reanchors() {
    let mut h = harness();
    let first = signal(1600);
    let second = signal(3200);
    let gap_at = START + 10 * RATE; // 10 s later

    let mut a = Vec::new();
    push_stream(&mut h, &first, START, &mut a);
    // The DISCONT on the next buffer flushes the first stream's tail first.
    let mut b = Vec::new();
    push_stream(&mut h, &second, gap_at, &mut b);
    assert!(h.push_event(gst::event::Eos::new()));
    pull_all(&mut h, &mut b);

    // Everything before the gap belongs to the first stream.
    let split = b
        .iter()
        .position(|x| x.pts() >= Some(time(gap_at)))
        .unwrap();
    a.extend(b.drain(..split));

    assert_eq!(a.iter().map(|x| x.size() / 4).sum::<usize>(), first.len());
    assert_eq!(b.iter().map(|x| x.size() / 4).sum::<usize>(), second.len());
    assert_contiguous(&a, START);
    assert_contiguous(&b, gap_at);
}

fn total(outs: &[gst::Buffer]) -> usize {
    outs.iter().map(|b| b.size() / 4).sum()
}

#[test]
fn offsets_continue_from_the_input() {
    let mut h = harness();
    let outs = run(&mut h, &signal(8000));

    let mut n = START;
    for b in &outs {
        let len = (b.size() / 4) as u64;
        assert_eq!((b.offset(), b.offset_end()), (n, n + len));
        n += len;
    }
}

#[test]
fn no_input_offsets_means_no_output_offsets() {
    let mut h = harness();
    let mut buf = buffer(&signal(1600), START, true);
    buf.get_mut()
        .unwrap()
        .set_offset(gst::format::Buffers::OFFSET_NONE);
    h.push(buf).unwrap();
    assert!(h.push_event(gst::event::Eos::new()));
    let mut outs = Vec::new();
    pull_all(&mut h, &mut outs);

    assert_eq!(total(&outs), 1600);
    assert!(
        outs.iter()
            .all(|b| b.offset() == gst::format::Buffers::OFFSET_NONE)
    );
}

#[test]
fn gap_pushes_held_audio_first() {
    let mut h = harness();
    let input = signal(8000);
    let mut outs = Vec::new();
    push_stream(&mut h, &input, START, &mut outs);
    assert!(
        total(&outs) < input.len(),
        "model should be holding samples"
    );

    let gap_at = START + input.len() as u64;
    let gap = gst::event::Gap::builder(time(gap_at))
        .duration(gst::ClockTime::SECOND)
        .build();
    assert!(h.push_event(gap));
    pull_all(&mut h, &mut outs);

    // Everything before the gap is out before the GAP event passes on.
    assert_eq!(total(&outs), input.len());
    assert_contiguous(&outs, START);

    // After the gap, a new stream starts at the next buffer, even without DISCONT.
    let after = gap_at + RATE;
    let mut next = Vec::new();
    h.push(buffer(&signal(3200), after, false)).unwrap();
    pull_all(&mut h, &mut next);
    assert!(!next.is_empty());
    assert_contiguous(&next, after);
}

#[test]
fn new_segment_pushes_held_audio_first() {
    let mut h = harness();
    let input = signal(8000);
    let mut outs = Vec::new();
    push_stream(&mut h, &input, START, &mut outs);
    assert!(
        total(&outs) < input.len(),
        "model should be holding samples"
    );

    let segment = gst::FormattedSegment::<gst::ClockTime>::new();
    assert!(h.push_event(gst::event::Segment::new(&segment)));
    pull_all(&mut h, &mut outs);

    assert_eq!(total(&outs), input.len());
    assert_contiguous(&outs, START);
}

#[test]
fn reports_model_latency() {
    let h = harness();
    let latency = h.query_latency().expect("latency");
    // One hop (160) of buffering plus the model delay (160) at 16 kHz.
    assert_eq!(latency, gst::ClockTime::from_mseconds(20));
}

#[test]
fn atten_lim_db_round_trips_while_playing() {
    let mut h = harness();
    let element = h.element().unwrap();
    assert_eq!(element.property::<f32>("atten-lim-db"), 100.0);

    let mut outs = Vec::new();
    h.push(buffer(&signal(1600), START, true)).unwrap();
    pull_all(&mut h, &mut outs);

    element.set_property("atten-lim-db", 12.0f32);
    assert_eq!(element.property::<f32>("atten-lim-db"), 12.0);

    h.push(buffer(&signal(1600), START + 1600, false)).unwrap();
    pull_all(&mut h, &mut outs);
    assert!(!outs.is_empty());
}
