//! Fixed runtime values. These match the existing Hush wrapper (libweya_nc) and
//! define the behavior that the baseline evaluation measures. They are not
//! user-configurable; log them in RunInfo.

use df::tract::{ReduceMask, RuntimeParams};

/// Mono only. The pipeline downmixes before the model.
pub const N_CH: usize = 1;

pub const POST_FILTER: bool = false;

/// Local SNR (dB) below which a zero mask is applied.
pub const MIN_DB_THRESH: f32 = -15.0;
/// Local SNR (dB) above which all processing is skipped.
pub const MAX_DB_ERB_THRESH: f32 = 35.0;
/// Local SNR (dB) above which the deep-filter stage is skipped.
pub const MAX_DB_DF_THRESH: f32 = 35.0;

/// Has no effect for one channel. Kept equal to the wrapper.
pub const REDUCE_MASK: ReduceMask = ReduceMask::MAX;

/// Runtime parameters for a new session. `atten_lim_db` must already be
/// validated (0.0..=100.0); 100.0 means unlimited.
pub(crate) fn runtime_params(atten_lim_db: f32) -> RuntimeParams {
    RuntimeParams::new(
        N_CH,
        POST_FILTER,
        atten_lim_db,
        MIN_DB_THRESH,
        MAX_DB_ERB_THRESH,
        MAX_DB_DF_THRESH,
        REDUCE_MASK,
    )
}