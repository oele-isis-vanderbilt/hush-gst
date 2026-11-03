//! One session per audio stream. Owns the recurrent state (inside `DfTract`) and
//! the user setting. The model weights are shared through an `Arc` clone.
//!
//! A session is probably not `Send` (the tract execution state holds `Rc`).
//! Phase 0 confirms this; keep one session on one thread until then.

use std::sync::Arc;

use df::tract::{DfModelShared, DfTract};
use ndarray::{ArrayView2, ArrayViewMut2};

use crate::bundle::ModelBundle;
use crate::constants::runtime_params;
use crate::error::Error;

/// User-facing setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// Maximum noise reduction in dB, 0.0..=100.0. 100.0 means unlimited.
    /// 0.0 returns the input unprocessed and undelayed, so do not use it as a bypass.
    pub atten_lim_db: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { atten_lim_db: 100.0 }
    }
}

impl Settings {
    pub fn new(atten_lim_db: f32) -> Result<Self, Error> {
        check_atten(atten_lim_db)?;
        Ok(Self { atten_lim_db })
    }
}

fn check_atten(db: f32) -> Result<(), Error> {
    if db.is_nan() || !(0.0..=100.0).contains(&db) {
        return Err(Error::InvalidSetting(format!(
            "atten_lim_db must be between 0 and 100, got {db}"
        )));
    }
    Ok(())
}

pub struct Session {
    df: DfTract,
    shared: Arc<DfModelShared>,
    atten_lim_db: f32,
}

impl Session {
    pub fn new(bundle: &ModelBundle, settings: Settings) -> Result<Self, Error> {
        check_atten(settings.atten_lim_db)?;
        let shared = Arc::clone(bundle.shared());
        let df = build(&shared, settings.atten_lim_db)?;
        Ok(Self {
            df,
            shared,
            atten_lim_db: settings.atten_lim_db,
        })
    }

    /// Samples per call, at `sample_rate()`.
    pub fn hop_size(&self) -> usize {
        self.df.hop_size
    }

    pub fn sample_rate(&self) -> usize {
        self.df.sr
    }

    /// Process exactly one hop. Returns the local SNR estimate in dB.
    /// Both slices must be `hop_size()` long.
    pub fn process_frame(&mut self, input: &[f32], output: &mut [f32]) -> Result<f32, Error> {
        let hop = self.df.hop_size;
        if input.len() != hop {
            return Err(Error::FrameSize {
                expected: hop,
                got: input.len(),
            });
        }
        if output.len() != hop {
            return Err(Error::FrameSize {
                expected: hop,
                got: output.len(),
            });
        }
        let noisy = ArrayView2::from_shape((1, hop), input)
            .map_err(|e| Error::Process(e.to_string()))?;
        let enhanced = ArrayViewMut2::from_shape((1, hop), output)
            .map_err(|e| Error::Process(e.to_string()))?;
        self.df
            .process(noisy, enhanced)
            .map_err(|e| Error::Process(format!("{e:#}")))
    }

    pub fn set_atten_lim_db(&mut self, db: f32) -> Result<(), Error> {
        check_atten(db)?;
        self.df
            .set_atten_lim(db)
            .map_err(|e| Error::Process(format!("{e:#}")))?;
        self.atten_lim_db = db;
        Ok(())
    }

    /// Clear all stream state. Rebuilds the session from the shared model, so it
    /// is expensive: use it on flush or seek, not per buffer.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.df = build(&self.shared, self.atten_lim_db)?;
        Ok(())
    }
}

fn build(shared: &DfModelShared, atten_lim_db: f32) -> Result<DfTract, Error> {
    DfTract::from_shared(shared, &runtime_params(atten_lim_db))
        .map_err(|e| Error::ModelInit(format!("{e:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn settings_accept_the_valid_range() {
        assert!(Settings::new(0.0).is_ok());
        assert!(Settings::new(40.0).is_ok());
        assert!(Settings::new(100.0).is_ok());
    }

    #[test]
    fn settings_reject_bad_values() {
        assert!(Settings::new(-1.0).is_err());
        assert!(Settings::new(100.5).is_err());
        assert!(Settings::new(f32::NAN).is_err());
    }

    #[test]
    fn default_is_unlimited() {
        assert_eq!(Settings::default().atten_lim_db, 100.0);
    }

    #[test]
    fn smoke_test_with_real_model() {
        let path = PathBuf::from(std::env::var("CARGO_WORKSPACE_DIR").unwrap()).join("models").join("advanced_dfnet16k");
        let bundle = ModelBundle::from_directory(&path).unwrap();
        let mut session = Session::new(&bundle, Settings::default()).unwrap();
        let hop = session.hop_size();

        // Wrong sizes are errors, not panics.
        let mut out = vec![0.0f32; hop];
        assert!(matches!(
            session.process_frame(&vec![0.0; hop - 1], &mut out),
            Err(Error::FrameSize { .. })
        ));

        // A frame of low-level noise gives finite output.
        let noise: Vec<f32> = (0..hop)
            .map(|i| ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.1)
            .collect();
        for _ in 0..50 {
            session.process_frame(&noise, &mut out).unwrap();
            assert!(out.iter().all(|x| x.is_finite()));
        }

        session.set_atten_lim_db(20.0).unwrap();
        session.reset().unwrap();
        session.process_frame(&noise, &mut out).unwrap();
    }
}