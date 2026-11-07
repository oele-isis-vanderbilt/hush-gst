// Copyright (C) 2026 Umesh Timalsina <umesh.timalsina@vanderbilt.edu>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.

//! `hush` element: 16 kHz mono F32 in, denoised audio out, one output buffer
//! per input buffer. Output lags input by the model latency (320 samples,
//! 20 ms), which the latency query reports. EOS pushes the remaining tail.

use std::sync::{LazyLock, Mutex, OnceLock};

use byte_slice_cast::*;
use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gst_audio::subclass::prelude::*;
use gst_base::prelude::*;
use gst_base::subclass::base_transform::BaseTransformImplExt;
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
    /// End time of the last input buffer: where the EOS tail goes.
    next_pts: Option<gst::ClockTime>,
}

#[derive(Default)]
pub struct HushNoiseFilter {
    settings: Mutex<Settings>,
    state: Mutex<Option<State>>,
}

impl HushNoiseFilter {
    /// Flush the model at EOS and push its last `latency` samples downstream.
    fn push_tail(&self) -> Result<gst::FlowSuccess, gst::FlowError> {
        let buffer = {
            let mut guard = self.state.lock().unwrap();
            let Some(state) = guard.as_mut() else {
                return Ok(gst::FlowSuccess::Ok);
            };

            state.out.clear();
            state.worker.finish(&mut state.out).map_err(|e| {
                gst::element_imp_error!(self, gst::StreamError::Failed, ["{}", e]);
                gst::FlowError::Error
            })?;
            // finish() ends the stream; get ready for the next one.
            state.worker.reset().map_err(|e| {
                gst::element_imp_error!(self, gst::StreamError::Failed, ["{}", e]);
                gst::FlowError::Error
            })?;
            let pts = state.next_pts.take();

            if state.out.is_empty() {
                return Ok(gst::FlowSuccess::Ok);
            }
            let mut buffer = gst::Buffer::from_mut_slice(state.out.as_byte_slice().to_vec());
            {
                let buffer = buffer.get_mut().unwrap();
                buffer.set_pts(pts);
                buffer.set_duration(samples_to_time(state.out.len() as u64));
            }
            buffer
        };

        gst::debug!(
            CAT,
            imp = self,
            "Pushing {} tail bytes at EOS",
            buffer.size()
        );
        self.obj().src_pad().push(buffer)
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
        let worker = StreamWorker::spawn(bundle, settings, DelayMode::Live)
            .map_err(|e| init_error(e.to_string()))?;

        gst::debug!(
            CAT,
            imp = self,
            "Started: hop {} samples, latency {} samples",
            worker.hop_size(),
            worker.latency_samples()
        );

        *self.state.lock().unwrap() = Some(State {
            worker,
            out: Vec::new(),
            next_pts: None,
        });
        Ok(())
    }

    fn stop(&self) -> Result<(), gst::ErrorMessage> {
        // Dropping the worker joins its thread.
        let _ = self.state.lock().unwrap().take();
        Ok(())
    }

    fn transform(
        &self,
        inbuf: &gst::Buffer,
        outbuf: &mut gst::BufferRef,
    ) -> Result<gst::FlowSuccess, gst::FlowError> {
        let mut guard = self.state.lock().unwrap();
        let state = guard.as_mut().ok_or_else(|| {
            gst::element_imp_error!(self, gst::CoreError::Negotiation, ["Not started"]);
            gst::FlowError::NotNegotiated
        })?;

        let in_map = inbuf.map_readable().map_err(|_| gst::FlowError::Error)?;
        let input = in_map.as_slice_of::<f32>().map_err(|_| {
            gst::element_imp_error!(self, gst::CoreError::Failed, ["Unaligned input buffer"]);
            gst::FlowError::Error
        })?;

        state.out.clear();
        state.worker.push(input, &mut state.out).map_err(|e| {
            gst::element_imp_error!(self, gst::StreamError::Failed, ["{}", e]);
            gst::FlowError::Error
        })?;

        let mut out_map = outbuf.map_writable().map_err(|_| gst::FlowError::Error)?;
        let output = out_map.as_mut_slice_of::<f32>().map_err(|_| {
            gst::element_imp_error!(self, gst::CoreError::Failed, ["Unaligned output buffer"]);
            gst::FlowError::Error
        })?;
        // Live mode returns exactly as many samples as it was given.
        output.copy_from_slice(&state.out);

        if let Some(pts) = inbuf.pts() {
            state.next_pts = Some(pts + samples_to_time(input.len() as u64));
        }

        Ok(gst::FlowSuccess::Ok)
    }

    fn sink_event(&self, event: gst::Event) -> bool {
        use gst::EventView;

        match event.view() {
            EventView::Eos(_) => {
                gst::debug!(CAT, imp = self, "Handling EOS");
                if self.push_tail().is_err() {
                    return false;
                }
            }
            EventView::FlushStop(_) => {
                // A flush (seek) starts a new stream.
                if let Some(state) = self.state.lock().unwrap().as_mut() {
                    state.next_pts = None;
                    if let Err(e) = state.worker.reset() {
                        gst::element_imp_error!(self, gst::StreamError::Failed, ["{}", e]);
                        return false;
                    }
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
            let ours = match self.state.lock().unwrap().as_ref() {
                Some(state) => samples_to_time(state.worker.latency_samples() as u64),
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
