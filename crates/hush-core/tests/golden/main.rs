#[path = "../common/mod.rs"]
mod common;

#[allow(unused_imports)]
use common::*;

mod test {
    use super::*;

    use std::path::Path;

    use hush_core::{DelayMode, ModelBundle, Session, Settings, StreamProcessor};

    const RAW: &str = "sample_00006_raw.wav";
    const DENOISED: &str = "sample_00006_denoised.wav";
    /// Our output, written by the diagnostic. Not committed.
    const OURS: &str = "sample_00006_denoised_stream_processor.wav";
    const HOP: usize = 160;
    /// Lag search range, in samples (3 hops either way).
    const MAX_LAG: isize = 480;
    /// Frames whose max |diff| is above this are listed in the per-frame report.
    const FRAME_REPORT_LSB: i32 = 4;
    /// At most this many frames are printed.
    const FRAME_REPORT_MAX_LINES: usize = 40;

    fn bundle() -> ModelBundle {
        let path = Path::new(env!("CARGO_WORKSPACE_DIR"))
            .join("models")
            .join("advanced_dfnet16k");
        ModelBundle::from_directory(&path).unwrap()
    }

    fn run(bundle: &ModelBundle, mode: DelayMode, chunk: usize, signal: &[f32]) -> Vec<f32> {
        let session = Session::new(bundle, Settings::default()).unwrap();
        let mut p = StreamProcessor::new(session, mode);
        let mut out = Vec::with_capacity(signal.len());
        for c in signal.chunks(chunk) {
            p.push(c, &mut out).unwrap();
        }
        p.finish(&mut out).unwrap();
        out
    }

    /// Comparison of `golden[i + lag]` with `ours[i]` over the overlapping range.
    /// A positive lag means the golden is later than ours.
    struct Stats {
        lag: isize,
        compared: usize,
        equal: usize,
        max_diff: i32,
        max_diff_at: usize, // index into golden
        snr_db: f64,
    }

    fn compare(golden: &[i16], ours: &[i16], lag: isize) -> Stats {
        let mut s = Stats {
            lag,
            compared: 0,
            equal: 0,
            max_diff: 0,
            max_diff_at: 0,
            snr_db: f64::INFINITY,
        };
        let (mut sig, mut err) = (0.0f64, 0.0f64);
        for (i, &o) in ours.iter().enumerate() {
            let gi = i as isize + lag;
            if gi < 0 || gi as usize >= golden.len() {
                continue;
            }
            let g = golden[gi as usize];
            let d = (g as i32 - o as i32).abs();
            s.compared += 1;
            if d == 0 {
                s.equal += 1;
            }
            if d > s.max_diff {
                s.max_diff = d;
                s.max_diff_at = gi as usize;
            }
            sig += (g as f64).powi(2);
            err += (d as f64).powi(2);
        }
        if err > 0.0 {
            s.snr_db = 10.0 * (sig / err).log10();
        }
        s
    }

    fn best_lag(golden: &[i16], ours: &[i16]) -> Stats {
        (-MAX_LAG..=MAX_LAG)
            .map(|lag| compare(golden, ours, lag))
            .max_by(|a, b| a.snr_db.total_cmp(&b.snr_db))
            .unwrap()
    }

    fn print_stats(label: &str, s: &Stats) {
        println!(
            "  {label:<22} lag {:>4} | equal {:>6}/{:<6} ({:>6.2}%) | max |diff| {:>5} LSB at {:>6} (frame {:>3}) | SNR {:>7.2} dB",
            s.lag,
            s.equal,
            s.compared,
            100.0 * s.equal as f64 / s.compared as f64,
            s.max_diff,
            s.max_diff_at,
            s.max_diff_at / HOP,
            s.snr_db,
        );
    }

    /// Max |diff| per 160-sample golden frame, at the given lag.
    fn frame_report(golden: &[i16], ours: &[i16], lag: isize) {
        let n_frames = golden.len().div_ceil(HOP);
        let mut max_per_frame = vec![0i32; n_frames];
        for (i, &o) in ours.iter().enumerate() {
            let gi = i as isize + lag;
            if gi < 0 || gi as usize >= golden.len() {
                continue;
            }
            let gi = gi as usize;
            let d = (golden[gi] as i32 - o as i32).abs();
            let f = &mut max_per_frame[gi / HOP];
            *f = (*f).max(d);
        }

        let bad: Vec<(usize, i32)> = max_per_frame
            .iter()
            .copied()
            .enumerate()
            .filter(|&(_, d)| d > FRAME_REPORT_LSB)
            .collect();
        let exact = max_per_frame.iter().filter(|&&d| d == 0).count();
        let one_lsb = max_per_frame.iter().filter(|&&d| d == 1).count();
        println!(
            "  frames: {n_frames} total | exact {exact} | max 1 LSB {one_lsb} | over {FRAME_REPORT_LSB} LSB {}",
            bad.len()
        );
        for &(f, d) in bad.iter().take(FRAME_REPORT_MAX_LINES) {
            println!(
                "    frame {f:>3} (samples {:>6}..{:<6}) max |diff| {d:>5} LSB",
                f * HOP,
                (f + 1) * HOP
            );
        }
        if bad.len() > FRAME_REPORT_MAX_LINES {
            println!("    ... {} more", bad.len() - FRAME_REPORT_MAX_LINES);
        }
    }

    /// Plan step 5. Prints how our output compares with the upstream golden,
    /// to decide which assertions are realistic. Run with:
    /// `cargo test -p hush-core --release --test golden -- --ignored --nocapture`
    #[test]
    #[ignore = "diagnostic: prints a report, run on demand"]
    fn diagnose_against_upstream_golden() {
        let raw = read_i16(&fixture(RAW));
        let golden = read_i16(&fixture(DENOISED));
        assert_eq!(raw.len(), golden.len(), "raw and denoised lengths differ");

        let bundle = bundle();
        let ours = run(&bundle, DelayMode::Aligned, HOP, &to_f32(&raw));
        assert_eq!(
            ours.len(),
            raw.len(),
            "Aligned output length != input length"
        );
        assert!(ours.iter().all(|x| x.is_finite()), "non-finite output");

        let peak = ours.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        println!("\n=== golden diagnostic: {RAW} -> StreamProcessor(Aligned, chunk {HOP}) ===");
        println!(
            "  samples {} | our peak {peak:.4} (clips if > 1.0)",
            ours.len()
        );

        for (name, ours_i16) in [
            ("floor x32768", to_i16_floor(&ours)),
            ("trunc x32768", to_i16_trunc(&ours)),
            ("round x32767", to_i16_round(&ours)),
        ] {
            println!("\n--- conversion: {name} ---");
            print_stats("at lag 0:", &compare(&golden, &ours_i16, 0));
            print_stats("at lag +160:", &compare(&golden, &ours_i16, HOP as isize));
            let best = best_lag(&golden, &ours_i16);
            print_stats("best lag:", &best);
            frame_report(&golden, &ours_i16, best.lag);
        }

        // Save our output (floor rule, the one the golden matches) next to the
        // golden for listening and diffing. Gitignored: see tests/fixtures/.gitignore.
        let out = fixture(OURS);
        write_i16(&out, &to_i16_floor(&ours));
        println!("\n  wrote {}", out.display());

        println!("\nHow to read this:");
        println!(
            "  best lag 0    -> golden is delay-compensated (offline / infer_single.py style)"
        );
        println!("  best lag +160 -> golden is raw frame-by-frame output (ctypes style)");
        println!("  diffs only in the last frames -> tail / flush handling");
        println!("  scattered large diffs -> LSNR gating or PyTorch vs tract");
        println!("  +-1 LSB everywhere -> conversion rule or float noise\n");
    }
}
