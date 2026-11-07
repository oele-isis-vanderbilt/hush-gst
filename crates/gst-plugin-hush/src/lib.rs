// Copyright (C) 2026 Umesh Timalsina <umesh.timalsina@vanderbilt.edu>
//
// This Source Code Form is subject to the terms of the Mozilla Public License, v2.0.
// If a copy of the MPL was not distributed with this file, You can obtain one at
// <https://mozilla.org/MPL/2.0/>.

//! GStreamer plugin `hush`: speech denoising with the Hush model.

use gst::glib;
use gst::prelude::*;

mod imp;

// Public type for the element. Its name must match `ObjectSubclass::Type` in imp.rs.
glib::wrapper! {
    pub struct HushNoiseFilter(ObjectSubclass<imp::HushNoiseFilter>)
        @extends gst_audio::AudioFilter, gst_base::BaseTransform, gst::Element, gst::Object;
}

fn plugin_init(plugin: &gst::Plugin) -> Result<(), glib::BoolError> {
    gst::Element::register(
        Some(plugin),
        "hush",
        gst::Rank::NONE,
        HushNoiseFilter::static_type(),
    )
}

// The plugin name must match the library name: `hush` -> libgsthush.
gst::plugin_define!(
    hush,
    env!("CARGO_PKG_DESCRIPTION"),
    plugin_init,
    concat!(env!("CARGO_PKG_VERSION"), "-", env!("COMMIT_ID")),
    // GStreamer only knows short license names; "MPL" is what gst-plugins-rs uses.
    "MPL",
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_NAME"),
    env!("CARGO_PKG_REPOSITORY"),
    env!("BUILD_REL_DATE")
);
