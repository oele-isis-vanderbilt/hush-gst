// Copyright (C) 2026 Umesh Timalsina <umesh.timalsina@vanderbilt.edu>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.

//! `hush` element: 16 kHz mono F32 in, denoised audio out.
//!
//! The model runs in `DelayMode::Aligned`: output sample i is input sample i,
//! so output timestamps are the input's own timestamps and the total output
//! length equals the input length. The output leaves the element up to 320
//! samples (20 ms) after the matching input arrived; the latency query reports
//! that. Buffer sizes are not 1:1 with the input.

use std::sync::{LazyLock, Mutex, OnceLock};

use byte_slice_cast::*;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_audio::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::base_transform::{BaseTransformImplExt, GenerateOutputSuccess};
use hush_core::{DelayMode, ModelBundle, ModelManifest, StreamWorker, parse_sha256};

static CAT: LazyLock<gst::DebugCategory> = LazyLock::new(|| {
    gst::DebugCategory::new(
        "hush",
        gst::DebugColorFlags::empty(),
        Some("Rust Hush Audio Denoiser"),
    )
});

// CARGO_MANIFEST_DIR (not CARGO_WORKSPACE_DIR): .cargo/config.toml is not read
// when this crate is built as a git dependency.
static MODEL: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../models/advanced_dfnet16k/advanced_dfnet16k.tar.gz"
));

static MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../models/advanced_dfnet16k/manifest.json"
));

/// The model's sample rate. `start` checks it against the bundle.
const MODEL_RATE: i32 = 16000;

// Same default as the CLI's --atten-lim-db.
const DEFAULT_ATTN_LIMIT_DB: f32 = 100.0;

/// The embedded model, parsed and hash-checked once per process.
fn bundle() -> Result<ModelBundle, String> {
    static BUNDLE: OnceLock<Result<ModelBundle, String>> = OnceLock::new();
    BUNDLE
        .get_or_init(|| {
            let manifest: ModelManifest =
                serde_json::from_str(MANIFEST).map_err(|e| format!("model manifest: {e}"))?;
            let sha = parse_sha256(&manifest.sha256).map_err(|e| e.to_string())?;
            ModelBundle::from_static_bytes(MODEL, Some(sha)).map_err(|e| e.to_string())
        })
        .clone()
}

fn samples_to_time(samples: u64) -> gst::ClockTime {
    gst::ClockTime::from_nseconds(
        samples
            .mul_div_floor(*gst::ClockTime::SECOND, MODEL_RATE as u64)
            .unwrap_or(0),
    )
}

#[derive(Debug, Clone, Copy)]
struct Settings {
    attn_lim_db: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            attn_lim_db: DEFAULT_ATTN_LIMIT_DB,
        }
    }
}

struct State {
    worker: StreamWorker,
    /// Scratch output, reused across buffers.
    out: Vec<f32>,
    /// True once the current stream has seen its first buffer.
    anchored: bool,
    /// PTS of the first input sample of the current stream.
    base_pts: Option<gst::ClockTime>,
    /// Buffer offset (sample index) of the first input sample, if upstream sets offsets.
    base_offset: Option<u64>,
    /// Samples pushed into the model in the current stream.
    samples_in: u64,
    /// Samples emitted in the current stream. Output sample n has time
    /// `base_pts + n / rate`.
    samples_out: u64,
}

impl State {
    /// Forget the current stream; the next buffer starts a new one.
    fn reset(&mut self) -> Result<(), hush_core::Error> {
        self.anchored = false;
        self.base_pts = None;
        self.base_offset = None;
        self.samples_in = 0;
        self.samples_out = 0;
        self.worker.reset()
    }

    /// Wrap `self.out` into a buffer timestamped from the stream position.
    fn take_output(&mut self) -> Option<gst::Buffer> {
        if self.out.is_empty() {
            return None;
        }
        let start = self.samples_out;
        let end = start + self.out.len() as u64;
        let mut buffer = gst::Buffer::from_mut_slice(self.out.as_byte_slice().to_vec());
        {
            let buffer = buffer.get_mut().unwrap();
            if let Some(base) = self.base_pts {
                let pts = base + samples_to_time(start);
                buffer.set_pts(pts);
                // From the end time, so rounding never accumulates.
                buffer.set_duration((base + samples_to_time(end)) - pts);
            }
            if let Some(base) = self.base_offset {
                buffer.set_offset(base + start);
                buffer.set_offset_end(base + end);
            }
            if start == 0 {
                buffer.set_flags(gst::BufferFlags::DISCONT);
            }
        }
        self.samples_out = end;
        Some(buffer)
    }
}

#[derive(Default)]
pub struct HushNoiseFilter {
    settings: Mutex<Settings>,
    state: Mutex<Option<State>>,
}

impl HushNoiseFilter {
    fn stream_error(&self, e: hush_core::Error) -> gst::FlowError {
        gst::element_imp_error!(self, gst::StreamError::Failed, ["{}", e]);
        gst::FlowError::Error
    }

    /// End the current stream: flush the model, push what it still holds, and
    /// get ready for a new stream. Used at EOS and on a DISCONT.
    fn finish_stream(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
        let tail = {
            let mut guard = self.state.lock().unwrap();
            let Some(state) = guard.as_mut() else {
                return Ok(gst::FlowSuccess::Ok);
            };
            if !state.anchored {
                return Ok(gst::FlowSuccess::Ok);
            }
            state.out.clear();
            state
                .worker
                .finish(&mut state.out)
                .map_err(|e| self.stream_error(e))?;
            let tail = state.take_output();
            gst::debug!(
                CAT,
                imp = self,
                "Stream finished: {} samples in, {} out",
                state.samples_in,
                state.samples_out
            );
            state.reset().map_err(|e| self.stream_error(e))?;
            tail
        };

        // Push without holding the state lock.
        match tail {
            Some(buffer) => self.obj().src_pad().push(buffer),
            None => Ok(gst::FlowSuccess::Ok),
        }
    }
}

#[glib::object_subclass]
impl ObjectSubclass for HushNoiseFilter {
    const NAME: &'static str = "GstHushNoiseFilter";
    type Type = super::HushNoiseFilter;
    type ParentType = gst_audio::AudioFilter;
}

impl ObjectImpl for HushNoiseFilter {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                glib::ParamSpecFloat::builder("atten-lim-db")
                    .nick("Attenuation limit (dB)")
                    .blurb(
                        "Maximum noise reduction in dB; 100 means unlimited. \
                         0 disables reduction but is not a bypass",
                    )
                    .minimum(0.0)
                    .maximum(100.0)
                    .default_value(DEFAULT_ATTN_LIMIT_DB)
                    .mutable_playing()
                    .build(),
            ]
        });

        PROPERTIES.as_ref()
    }

    fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        match pspec.name() {
            "atten-lim-db" => {
                let db: f32 = value.get().expect("type checked upstream");
                self.settings.lock().unwrap().attn_lim_db = db;
                if let Some(state) = self.state.lock().unwrap().as_ref()
                    && let Err(e) = state.worker.set_atten_lim_db(db)
                {
                    gst::warning!(CAT, imp = self, "Could not set atten-lim-db: {}", e);
                }
            }
            _ => unimplemented!(),
        }
    }

    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "atten-lim-db" => self.settings.lock().unwrap().attn_lim_db.to_value(),
            _ => unimplemented!(),
        }
    }
}

impl GstObjectImpl for HushNoiseFilter {}

impl ElementImpl for HushNoiseFilter {
    fn metadata() -> Option<&'static gst::subclass::ElementMetadata> {
        static ELEMENT_METADATA: LazyLock<gst::subclass::ElementMetadata> = LazyLock::new(|| {
            gst::subclass::ElementMetadata::new(
                "Hush noise suppression",
                "Filter/Effect/Audio",
                "Removes background noise from speech (16 kHz mono)",
                "Umesh Timalsina <umesh.timalsina@vanderbilt.edu>",
            )
        });

        Some(&*ELEMENT_METADATA)
    }
}

impl BaseTransformImpl for HushNoiseFilter {
    const MODE: gst_base::subclass::BaseTransformMode =
        gst_base::subclass::BaseTransformMode::NeverInPlace;
    const PASSTHROUGH_ON_SAME_CAPS: bool = false;
    const TRANSFORM_IP_ON_PASSTHROUGH: bool = false;

    fn start(&self) -> Result<(), gst::ErrorMessage> {
        let init_error = |msg: String| gst::error_msg!(gst::LibraryError::Init, ["{}", msg]);

        let bundle = bundle().map_err(init_error)?;
        if bundle.info().sample_rate != MODEL_RATE as usize {
            return Err(init_error(format!(
                "model runs at {} Hz, element expects {} Hz",
                bundle.info().sample_rate,
                MODEL_RATE
            )));
        }

        let db = self.settings.lock().unwrap().attn_lim_db;
        let settings = hush_core::Settings::new(db).map_err(|e| init_error(e.to_string()))?;
        let worker = StreamWorker::spawn(bundle, settings, DelayMode::Aligned)
            .map_err(|e| init_error(e.to_string()))?;

        gst::debug!(
            CAT,
            imp = self,
            "Started: hop {} samples, model delay {} samples",
            worker.hop_size(),
            worker.model_delay_samples()
        );

        *self.state.lock().unwrap() = Some(State {
            worker,
            out: Vec::new(),
            anchored: false,
            base_pts: None,
            base_offset: None,
            samples_in: 0,
            samples_out: 0,
        });
        Ok(())
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        // Dropping the worker joins its thread.
        let _ = self.state.lock().unwrap().take();
        Ok(())
    }

    fn generate_output(&self) -> Result<GenerateOutputSuccess, gst::FlowError> {
        let Some(buffer) = self.take_queued_buffer() else {
            return Ok(GenerateOutputSuccess::NoOutput);
        };

        // A gap or jump: end the current stream so the next one is
        // timestamped from this buffer.
        if buffer.flags().contains(gst::BufferFlags::DISCONT) {
            self.finish_stream()?;
        }

        let mut guard = self.state.lock().unwrap();
        let state = guard.as_mut().ok_or_else(|| {
            gst::element_imp_error!(self, gst::CoreError::Negotiation, ["Not started"]);
            gst::FlowError::NotNegotiated
        })?;

        if !state.anchored {
            state.anchored = true;
            state.base_pts = buffer.pts();
            state.base_offset =
                Some(buffer.offset()).filter(|&o| o != gst::format::Buffers::OFFSET_NONE);
            gst::debug!(CAT, imp = self, "New stream at {}", buffer.pts().display());
        }

        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
        let input = map.as_slice_of::<f32>().map_err(|_| {
            gst::element_imp_error!(self, gst::CoreError::Failed, ["Unaligned input buffer"]);
            gst::FlowError::Error
        })?;

        state.out.clear();
        state
            .worker
            .push(input, &mut state.out)
            .map_err(|e| self.stream_error(e))?;
        state.samples_in += input.len() as u64;

        Ok(match state.take_output() {
            Some(out) => GenerateOutputSuccess::Buffer(out),
            None => GenerateOutputSuccess::NoOutput,
        })
    }

    fn sink_event(&self, event: gst::Event) -> bool {
        use gst::EventView;

        match event.view() {
            // The model holds up to 320 samples. Push them before any event that
            // ends the stream or changes the timeline, so they are not timestamped
            // after it: EOS, a GAP (no data for a while), or a new SEGMENT without
            // a flush (e.g. gapless playback). The first SEGMENT comes before any
            // data, so there is nothing to push then.
            EventView::Eos(_) | EventView::Gap(_) | EventView::Segment(_) => {
                gst::debug!(
                    CAT,
                    imp = self,
                    "Finishing stream before {:?}",
                    event.type_()
                );
                if self.finish_stream().is_err() {
                    return false;
                }
            }
            EventView::FlushStop(_) => {
                // A flush (seek) drops what the model holds and starts a new stream.
                if let Some(state) = self.state.lock().unwrap().as_mut()
                    && let Err(e) = state.reset()
                {
                    gst::element_imp_error!(self, gst::StreamError::Failed, ["{}", e]);
                    return false;
                }
            }
            _ => {}
        }
        self.parent_sink_event(event)
    }

    fn query(&self, direction: gst::PadDirection, query: &mut gst::QueryRef) -> bool {
        if direction == gst::PadDirection::Src
            && let gst::QueryViewMut::Latency(q) = query.view_mut()
        {
            // A sample can wait up to one hop for its frame to fill, then the
            // model's own delay before it comes out.
            let ours = match self.state.lock().unwrap().as_ref() {
                Some(state) => samples_to_time(
                    (state.worker.hop_size() + state.worker.model_delay_samples()) as u64,
                ),
                None => return BaseTransformImplExt::parent_query(self, direction, query),
            };

            let mut upstream_query = gst::query::Latency::new();
            if self.obj().sink_pad().peer_query(&mut upstream_query) {
                let (live, min, max) = upstream_query.result();
                gst::debug!(
                    CAT,
                    imp = self,
                    "Peer latency: live {} min {} max {}, adding {}",
                    live,
                    min,
                    max.display(),
                    ours,
                );
                q.set(live, min + ours, max.opt_add(ours));
                return true;
            }
            return false;
        }
        BaseTransformImplExt::parent_query(self, direction, query)
    }
}

impl AudioFilterImpl for HushNoiseFilter {
    fn allowed_caps() -> &'static gst::Caps {
        static CAPS: LazyLock<gst::Caps> = LazyLock::new(|| {
            gst_audio::AudioCapsBuilder::new_interleaved()
                .format(gst_audio::AUDIO_FORMAT_F32)
                .rate(MODEL_RATE)
                .channels(1)
                .build()
        });

        &CAPS
    }

    fn setup(&self, info: &gst_audio::AudioInfo) -> Result<(), gst::LoggableError> {
        // Caps are fixed (16 kHz mono F32), so there is nothing to rebuild.
        gst::debug!(CAT, imp = self, "Set caps to {:?}", info);
        Ok(())
    }
}
