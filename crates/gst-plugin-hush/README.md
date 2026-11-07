# gst-plugin-hush

A GStreamer plugin with one element, `hush`, that removes background noise from
speech. It runs the same Hush model (DeepFilterNet, `advanced_dfnet16k`) as the
`hush` CLI, through `hush-core`. The model is built into the plugin.

| | |
|---|---|
| Element | `hush` (class `Filter/Effect/Audio`) |
| Caps | `audio/x-raw, format=F32LE, rate=16000, channels=1, layout=interleaved` |
| Latency | 320 samples (20 ms), reported in the latency query |
| Library | `libgsthush.so` / `libgsthush.dylib` / `gsthush.dll` |

## Requirements

- Rust 1.88+
- GStreamer 1.18+ development files and `pkg-config`
  - macOS: `brew install gstreamer`
  - Debian/Ubuntu: `sudo apt-get install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good`
  - Windows: the GStreamer **MSVC x86_64** runtime and development installers, and
    `pkg-config` (e.g. `choco install pkgconfiglite`). See [Windows](#windows).

## Build

From the repository root:

```sh
cargo build -p gst-plugin-hush --release
export GST_PLUGIN_PATH=$PWD/target/release
gst-inspect-1.0 hush
```

Use `--release`: debug builds of the model are slow.

To install it where GStreamer looks by default, use [cargo-c](https://github.com/lu-zero/cargo-c):

```sh
cargo install cargo-c
cargo cinstall -p gst-plugin-hush --release --prefix ~/.local
export GST_PLUGIN_PATH=~/.local/lib/gstreamer-1.0
```

### Windows

In PowerShell, from the repository root:

```powershell
$gst = $env:GSTREAMER_1_0_ROOT_MSVC_X86_64
$env:PKG_CONFIG_PATH = "$gst\lib\pkgconfig"
$env:PATH = "$gst\bin;$env:PATH"
cargo build -p gst-plugin-hush --release
$env:GST_PLUGIN_PATH = "$PWD\target\release"
gst-inspect-1.0 hush
```

## Use

The element only accepts 16 kHz mono F32, so put `audioconvert ! audioresample`
in front of it.

Denoise a file:

```sh
gst-launch-1.0 filesrc location=in.wav ! wavparse \
  ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=16000,channels=1 \
  ! hush \
  ! audioconvert dithering=none ! audio/x-raw,format=S16LE ! wavenc ! filesink location=out.wav
```

Live microphone to speakers (macOS; use `pulsesrc`/`pulsesink` on Linux,
`wasapisrc`/`wasapisink` on Windows):

```sh
gst-launch-1.0 osxaudiosrc ! audioconvert ! audioresample \
  ! audio/x-raw,format=F32LE,rate=16000,channels=1 \
  ! hush ! audioconvert ! osxaudiosink
```

From Rust, without installing the plugin:

```rust
gst::init()?;
gsthush::plugin_register_static()?;
let hush = gst::ElementFactory::make("hush").property("atten-lim-db", 20.0f32).build()?;
```

### Property

| Name | Type | Default | |
|---|---|---|---|
| `atten-lim-db` | float, 0–100 | 100 | Maximum noise reduction in dB. 100 means unlimited. 0 turns reduction off but still runs the model (it is not a bypass). Can be changed while playing. |

### Timing

- One output buffer per input buffer, same size and timestamps. Any buffer size works.
- The audio inside is 320 samples (20 ms) late. Live pipelines compensate through
  the latency query. File pipelines do not, so the output is 20 ms longer and
  shifted by 20 ms. The `hush` CLI removes this shift for files; the element does not.
- At EOS the element pushes the last 320 samples as one extra buffer.
- A flush (seek) resets the model.

## Debugging

```sh
GST_DEBUG=hush:5 gst-launch-1.0 ...               # element log: start, latency, EOS tail
gst-inspect-1.0 target/release/libgsthush.dylib   # prints the load error if the plugin will not load
rm -rf ~/.cache/gstreamer-1.0                     # stale plugin registry (Linux/macOS)
```

## Tests

```sh
cargo test -p gst-plugin-hush --release
cargo clippy -p gst-plugin-hush --all-targets --no-deps -- -D warnings
```

The tests use `gst_check::Harness`. They check buffer sizes and timestamps, the
EOS tail, the reported latency, the property, and that the output is
bit-identical to `hush_core::StreamProcessor` in `DelayMode::Live`.

## License

MPL-2.0. The vendored `deep_filter` crate (`third_party/deep_filter`) is
MIT OR Apache-2.0.
