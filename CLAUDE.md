# CLAUDE.md

Notes for contributors and coding agents. Keep it short and factual.

## Workspace

- `crates/hush-core`: library. `ModelBundle` (load + SHA-256 check, `from_path`,
  `from_directory`, `from_static_bytes`), `Session` (one stream, one hop at a time),
  `StreamProcessor` (any chunk size; `DelayMode::Live` / `Aligned`),
  `StreamWorker` (a `StreamProcessor` on its own thread, `Send`).
- `crates/hush-cli`: the `hush` binary. See `crates/hush-cli/README.md`.
- `crates/gst-plugin-hush` (package `gst-plugin-hush`, lib `gsthush`): GStreamer plugin. 
- `crates/hush-mic-example`: mic example.
- `third_party/deep_filter`: vendored libDF. Don't change it without saying so
  in the commit message. Its warnings are not ours (clippy runs with `--no-deps`).
- `models/advanced_dfnet16k`: model tar.gz + `manifest.json` (holds its SHA-256).
  Embedded into `hush` with `include_bytes!`.
- `install/`: `install.sh` (POSIX sh) and `install.ps1` (PowerShell 5.1+).

## Commands

```sh
cargo build -p hush-cli --release
cargo test -p hush-core -p hush-cli --release      # e2e tests skip without ffmpeg on PATH
cargo test -p hush-core --release -- --ignored --nocapture   # golden diagnostics
cargo clippy -p hush-core -p hush-cli --all-targets --no-deps -- -D warnings
UPDATE_README=1 cargo test -p hush-cli readme      # after changing CLI flags or help text
shellcheck install/install.sh

# GStreamer plugin (needs GStreamer + pkg-config installed)
cargo clippy -p gst-plugin-hush --all-targets --no-deps -- -D warnings
cargo test -p gst-plugin-hush --release
cargo build -p gst-plugin-hush --release
GST_PLUGIN_PATH=$PWD/target/release gst-launch-1.0 filesrc location=in.wav ! wavparse \
  ! audioconvert ! audioresample ! audio/x-raw,format=F32LE,rate=16000,channels=1 \
  ! hush ! audioconvert ! wavenc ! filesink location=out.wav
```

Debug builds of the model are slow; use `--release` for anything that runs audio.

## Invariants

- A `Session` is not `Send`: create it on the thread that uses it. Share the
  `ModelBundle` (cheap `Clone`, `Arc` inside).
- Files use `DelayMode::Aligned` (output length = input length). Live audio uses `Live`.
- `hush-core` never prints; it returns `hush_core::Error`.
- Fixed runtime values (thresholds, channel count) live in
  `crates/hush-core/src/constants.rs`; they are not CLI options.
- `CARGO_WORKSPACE_DIR` comes from `.cargo/config.toml`; tests and `include_bytes!` use it.
- The `hush` element is mono 16 kHz F32, uses `DelayMode::Live` through a
  `StreamWorker`, and reports 320 samples (20 ms) of latency; EOS pushes that tail.
  Its `include_bytes!` uses `CARGO_MANIFEST_DIR` so git-dependency builds work.

## CLI rules

- All ffmpeg/ffprobe calls go through `src/ffmpeg/` (lookup, install) and
  `src/media/` + `src/pipeline.rs` (probe, encode rules, streaming).
- Never write an output in place: write a `.hush-*` temp file in the output
  directory, check it, then rename. Ctrl-C must leave no temp files.
- Output format rules live only in `src/media/codec.rs`. A new codec needs a row
  there, a unit test, and a line in the CLI README table.
- FFmpeg download pins live only in `src/ffmpeg/sources.toml`. Fixed-version URLs
  only; update `url` and `sha256` together.
- Exit codes: 0 ok, 1 some files failed, 2 usage (clap), 3 setup, 130 interrupted.
- Commit messages: one short, descriptive line.

## Installers

- Must stay POSIX sh / PowerShell 5.1 compatible.
- The PATH edit is one line ending in `# added by the hush installer`; uninstall
  removes exactly those lines. Uninstall deletes the install root only if it
  contains `.hush-install`.
- When testing `install.sh` locally, set a throwaway `HOME`, **unset `ZDOTDIR`**
  (otherwise zsh users' real `~/.zshrc` is edited), and point
  `HUSH_DOWNLOAD_BASE=file://<dir>` at locally built archives.

## Releases

All crates share one version (`[workspace.package]` in the root `Cargo.toml`) and
are not published to crates.io. Bump it, commit, then push a matching tag `vX.Y.Z`
(the release fails if they differ). `.github/workflows/release.yml` builds five
targets, uploads `hush-<target>.tar.gz|zip` + `.sha256` + both installers to the
GitHub Release. Installers fetch from `releases/latest/download/`.
