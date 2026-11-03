//! Stream layer: arbitrary-length chunks in, samples out, on top of one `Session`.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

use crate::bundle::ModelBundle;
use crate::error::Error;
use crate::session::{Session, Settings};

/// The vendored `process` skips any frame below about 1e-7 mean power (returning
/// zeros) without advancing its state, so zeros cannot flush the model's tail.
/// Padding and flushing use a faint alternating signal (mean power 1e-6, about
/// -60 dBFS) instead. This is a workaround: check the tail in Phase 0.
const FLUSH_AMPLITUDE: f32 = 1e-3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelayMode {
    /// One output sample per input sample, as the audio arrives. Output lags the
    /// input by `latency_samples()`. `finish` appends that many samples, so the
    /// total output is longer than the input by the same amount.
    Live,
    /// For files. The lag is removed, so output sample i matches input sample i
    /// and the total output length equals the total input length. Output arrives
    /// later than in `Live`: `push` returns whatever is ready.
    Aligned,
}

pub struct StreamProcessor {
    session: Session,
    mode: DelayMode,
    hop: usize,
    model_delay: usize,
    pending: Vec<f32>,    // input not yet forming a full hop
    queue: VecDeque<f32>, // output ready to hand out
    scratch: Vec<f32>,    // one hop of model output
    flush: Vec<f32>,      // padding / flush frame, see FLUSH_AMPLITUDE
    skip: usize,          // leading model output still to drop (Aligned)
    total_in: u64,
    total_out: u64,
    finished: bool,
}

impl StreamProcessor {
    pub fn new(session: Session, mode: DelayMode) -> Self {
        let hop = session.hop_size();
        let model_delay = session.delay_samples();
        let flush = (0..hop)
            .map(|i| {
                if i % 2 == 0 {
                    FLUSH_AMPLITUDE
                } else {
                    -FLUSH_AMPLITUDE
                }
            })
            .collect();
        let mut p = Self {
            session,
            mode,
            hop,
            model_delay,
            pending: Vec::new(),
            queue: VecDeque::new(),
            scratch: vec![0.0; hop],
            flush,
            skip: 0,
            total_in: 0,
            total_out: 0,
            finished: false,
        };
        p.arm();
        p
    }

    pub fn hop_size(&self) -> usize {
        self.hop
    }

    pub fn sample_rate(&self) -> usize {
        self.session.sample_rate()
    }

    /// Live: how far output lags input (one hop of buffering plus the model delay).
    /// Aligned: 0.
    pub fn latency_samples(&self) -> usize {
        match self.mode {
            DelayMode::Live => self.hop + self.model_delay,
            DelayMode::Aligned => 0,
        }
    }

    /// Feed a chunk of any length. Appends ready output to `output`.
    /// Live mode appends exactly `input.len()` samples.
    pub fn push(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<(), Error> {
        if self.finished {
            return Err(Error::Process("stream is finished, call reset".into()));
        }
        self.total_in += input.len() as u64;
        self.pending.extend_from_slice(input);

        let hop = self.hop;
        let mut offset = 0;
        while self.pending.len() - offset >= hop {
            self.session
                .process_frame(&self.pending[offset..offset + hop], &mut self.scratch)?;
            offset += hop;
            emit(&mut self.queue, &mut self.skip, &self.scratch);
        }
        self.pending.drain(..offset);

        let n = match self.mode {
            DelayMode::Live => input.len(),
            DelayMode::Aligned => self.queue.len(),
        };
        self.take(n, output);
        Ok(())
    }

    /// End of stream: append the audio still inside the pipeline. Afterwards
    /// `push` is an error until `reset`.
    pub fn finish(&mut self, output: &mut Vec<f32>) -> Result<(), Error> {
        if self.finished {
            return Ok(());
        }
        let target = self.total_in + self.latency_samples() as u64;
        let needed = (target - self.total_out) as usize;

        // Complete a partial frame.
        if !self.pending.is_empty() {
            let have = self.pending.len();
            self.pending.extend_from_slice(&self.flush[have..]);
            self.session
                .process_frame(&self.pending, &mut self.scratch)?;
            emit(&mut self.queue, &mut self.skip, &self.scratch);
            self.pending.clear();
        }
        // Run the model forward until enough output exists.
        while self.queue.len() < needed {
            self.session.process_frame(&self.flush, &mut self.scratch)?;
            emit(&mut self.queue, &mut self.skip, &self.scratch);
        }

        self.take(needed, output);
        self.queue.clear();
        self.finished = true;
        Ok(())
    }

    pub fn set_atten_lim_db(&mut self, db: f32) -> Result<(), Error> {
        self.session.set_atten_lim_db(db)
    }

    /// Start a new stream. Rebuilds the session, so use it on flush or seek.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.session.reset()?;
        self.arm();
        Ok(())
    }

    /// Initial state for a new stream.
    fn arm(&mut self) {
        self.pending.clear();
        self.queue.clear();
        self.total_in = 0;
        self.total_out = 0;
        self.finished = false;
        match self.mode {
            DelayMode::Live => {
                // One hop of silence up front, so every push can return exactly
                // as many samples as it received.
                self.skip = 0;
                self.queue.resize(self.hop, 0.0);
            }
            DelayMode::Aligned => {
                // Drop the model's own lag so output lines up with input.
                self.skip = self.model_delay;
            }
        }
    }

    fn take(&mut self, n: usize, output: &mut Vec<f32>) {
        let n = n.min(self.queue.len());
        output.extend(self.queue.drain(..n));
        self.total_out += n as u64;
    }
}

fn emit(queue: &mut VecDeque<f32>, skip: &mut usize, frame: &[f32]) {
    let dropped = (*skip).min(frame.len());
    *skip -= dropped;
    queue.extend(&frame[dropped..]);
}

// ---------------------------------------------------------------------------
// Worker: the session is not Send, so it lives on its own thread.
// ---------------------------------------------------------------------------

enum Command {
    Push {
        input: Vec<f32>,
        reply: Sender<Result<Vec<f32>, Error>>,
    },
    Finish {
        reply: Sender<Result<Vec<f32>, Error>>,
    },
    Reset {
        reply: Sender<Result<(), Error>>,
    },
    SetAtten {
        db: f32,
        reply: Sender<Result<(), Error>>,
    },
}

struct Info {
    hop_size: usize,
    sample_rate: usize,
    latency_samples: usize,
}

/// A `StreamProcessor` on its own thread. `Send`, so an element can hold it.
/// Each call blocks until the worker replies. If the worker thread dies, calls
/// return an error instead of hanging.
pub struct StreamWorker {
    tx: Option<Sender<Command>>,
    thread: Option<JoinHandle<()>>,
    info: Info,
}

impl StreamWorker {
    /// Starts the thread, which builds its own `Session` from the bundle.
    pub fn spawn(bundle: ModelBundle, settings: Settings, mode: DelayMode) -> Result<Self, Error> {
        let (tx, rx) = mpsc::channel::<Command>();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<Info, Error>>(1);

        let thread = std::thread::Builder::new()
            .name("hush-worker".into())
            .spawn(move || {
                let mut proc = match Session::new(&bundle, settings) {
                    Ok(session) => StreamProcessor::new(session, mode),
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                        return;
                    }
                };
                let _ = init_tx.send(Ok(Info {
                    hop_size: proc.hop_size(),
                    sample_rate: proc.sample_rate(),
                    latency_samples: proc.latency_samples(),
                }));

                for cmd in rx {
                    match cmd {
                        Command::Push { input, reply } => {
                            let mut out = Vec::with_capacity(input.len());
                            let res = proc.push(&input, &mut out).map(|()| out);
                            let _ = reply.send(res);
                        }
                        Command::Finish { reply } => {
                            let mut out = Vec::new();
                            let res = proc.finish(&mut out).map(|()| out);
                            let _ = reply.send(res);
                        }
                        Command::Reset { reply } => {
                            let _ = reply.send(proc.reset());
                        }
                        Command::SetAtten { db, reply } => {
                            let _ = reply.send(proc.set_atten_lim_db(db));
                        }
                    }
                }
            })?;

        let info = init_rx
            .recv()
            .map_err(|_| Error::Process("worker failed to start".into()))??;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            info,
        })
    }

    pub fn hop_size(&self) -> usize {
        self.info.hop_size
    }

    pub fn sample_rate(&self) -> usize {
        self.info.sample_rate
    }

    pub fn latency_samples(&self) -> usize {
        self.info.latency_samples
    }

    pub fn push(&self, input: &[f32], output: &mut Vec<f32>) -> Result<(), Error> {
        let out = self.call(|reply| Command::Push {
            input: input.to_vec(),
            reply,
        })?;
        output.extend_from_slice(&out);
        Ok(())
    }

    pub fn finish(&self, output: &mut Vec<f32>) -> Result<(), Error> {
        let out = self.call(|reply| Command::Finish { reply })?;
        output.extend_from_slice(&out);
        Ok(())
    }

    pub fn reset(&self) -> Result<(), Error> {
        self.call(|reply| Command::Reset { reply })
    }

    pub fn set_atten_lim_db(&self, db: f32) -> Result<(), Error> {
        self.call(|reply| Command::SetAtten { db, reply })
    }

    fn call<T>(&self, make: impl FnOnce(Sender<Result<T, Error>>) -> Command) -> Result<T, Error> {
        let stopped = || Error::Process("worker thread stopped".into());
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .as_ref()
            .ok_or_else(stopped)?
            .send(make(reply_tx))
            .map_err(|_| stopped())?;
        reply_rx.recv().map_err(|_| stopped())?
    }
}

impl Drop for StreamWorker {
    fn drop(&mut self) {
        // Closing the channel ends the worker loop.
        self.tx.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;
    use std::path::PathBuf;

    #[test]
    fn worker_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<StreamWorker>();
    }

    fn test_bundle() -> ModelBundle {
        let path = PathBuf::from(std::env::var("CARGO_WORKSPACE_DIR").unwrap())
            .join("models")
            .join("advanced_dfnet16k");
        ModelBundle::from_directory(&path).unwrap()
    }

    /// One second of a tone plus low-level noise, at 16 kHz.
    fn test_signal() -> Vec<f32> {
        (0..16000)
            .map(|i| {
                0.1 * (2.0 * PI * 300.0 * i as f32 / 16000.0).sin()
                    + 0.02 * (((i * 7919) % 1000) as f32 / 1000.0 - 0.5)
            })
            .collect()
    }

    /// Returns (output, latency_samples).
    fn run(
        bundle: &ModelBundle,
        mode: DelayMode,
        chunk: usize,
        signal: &[f32],
    ) -> (Vec<f32>, usize) {
        let session = Session::new(bundle, Settings::default()).unwrap();
        let mut p = StreamProcessor::new(session, mode);
        let mut out = Vec::new();
        for c in signal.chunks(chunk) {
            p.push(c, &mut out).unwrap();
        }
        p.finish(&mut out).unwrap();
        (out, p.latency_samples())
    }

    #[test]
    fn output_lengths() {
        let bundle = test_bundle();
        let signal = test_signal();

        let (live, latency) = run(&bundle, DelayMode::Live, 160, &signal);
        assert_eq!(live.len(), signal.len() + latency);

        let (aligned, latency) = run(&bundle, DelayMode::Aligned, 160, &signal);
        assert_eq!(latency, 0);
        assert_eq!(aligned.len(), signal.len());
    }

    #[test]
    fn chunk_size_does_not_change_the_output() {
        let bundle = test_bundle();
        let signal = test_signal();
        for mode in [DelayMode::Live, DelayMode::Aligned] {
            let (reference, _) = run(&bundle, mode, 160, &signal);
            for chunk in [1, 7, 333, 4000] {
                let (out, _) = run(&bundle, mode, chunk, &signal);
                assert_eq!(out, reference, "mode {mode:?}, chunk {chunk}");
            }
        }
    }

    #[test]
    fn reset_restores_a_fresh_stream() {
        let bundle = test_bundle();
        let signal = test_signal();
        let session = Session::new(&bundle, Settings::default()).unwrap();
        let mut p = StreamProcessor::new(session, DelayMode::Aligned);

        let mut first = Vec::new();
        p.push(&signal, &mut first).unwrap();
        p.finish(&mut first).unwrap();

        // Pushing after finish is an error until reset.
        assert!(p.push(&signal, &mut Vec::new()).is_err());

        p.reset().unwrap();
        let mut second = Vec::new();
        p.push(&signal, &mut second).unwrap();
        p.finish(&mut second).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn worker_matches_the_processor() {
        let bundle = test_bundle();
        let signal = test_signal();
        let (expected, _) = run(&bundle, DelayMode::Live, 160, &signal);

        let worker = StreamWorker::spawn(bundle, Settings::default(), DelayMode::Live).unwrap();
        let mut out = Vec::new();
        for c in signal.chunks(160) {
            worker.push(c, &mut out).unwrap();
        }
        worker.finish(&mut out).unwrap();
        assert_eq!(out, expected);
    }
}
