# hush-gst

Noise suppression for speech, based on [Hush](https://github.com/pulp-vision/Hush)
by weya.ai. Hush runs a DeepFilterNet model (through the vendored `deep_filter`
crate and tract) on 16 kHz mono audio.

This workspace holds:

- **`hush`**, a command-line tool that denoises audio and video files in batches,
- **`hush-core`**, the Rust library behind it (model loading, sessions, streaming),
- **`hush-gst`**, a GStreamer plugin (in progress).

## Install the `hush` CLI

### macOS and Linux

```sh
curl -fsSL https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.sh | sh
```

### Windows (PowerShell)

```powershell
irm https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.ps1 | iex
```

The installer:

1. downloads the `hush` binary for your platform and checks its SHA-256,
2. installs it, then runs `hush ffmpeg install` to download a portable FFmpeg
   (ffmpeg + ffprobe) next to it,
3. adds hush to your `PATH`,
4. runs `hush doctor` to check everything.

Restart your shell afterwards, then run `hush --help`.

### Installer options

| sh (macOS / Linux) | PowerShell (Windows) | Effect |
|---|---|---|
| `--version v0.2.0` or `HUSH_VERSION` | `-Version v0.2.0` or `HUSH_VERSION` | install a specific release (default: latest) |
| `--dir <path>` or `HUSH_INSTALL_DIR` | `-Dir <path>` or `HUSH_INSTALL_DIR` | install root (default below) |
| `--no-ffmpeg` | `-NoFfmpeg` | don't download FFmpeg |
| `--ffmpeg-from-path` | `-FfmpegFromPath` | skip the FFmpeg download if ffmpeg and ffprobe are already on `PATH` |
| `--no-modify-path` | `-NoModifyPath` | don't change `PATH` |
| `--uninstall` | `-Uninstall` | remove hush, its FFmpeg, and the `PATH` entry |

Passing options to a piped install:

```sh
curl -fsSL https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.sh | sh -s -- --no-ffmpeg
```

```powershell
& ([scriptblock]::Create((irm https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.ps1))) -NoFfmpeg
```

### What goes where

| | macOS / Linux | Windows |
|---|---|---|
| binary | `~/.hush/bin/hush` | `%LOCALAPPDATA%\hush\bin\hush.exe` |
| FFmpeg | `~/.hush/ffmpeg/` (`ffmpeg`, `ffprobe`, `NOTICE.txt`, `VERSION`) | `%LOCALAPPDATA%\hush\ffmpeg\` |
| PATH | one line marked `# added by the hush installer` in `~/.zshrc` (zsh, honors `ZDOTDIR`), `~/.bashrc` (+ `~/.bash_profile` on macOS), `~/.config/fish/config.fish`, or `~/.profile` | the user `Path` variable |

The model is built into the binary; nothing else is downloaded.

### FFmpeg

hush needs `ffmpeg` and `ffprobe` to read and write media. It looks for them in
this order: `--ffmpeg <path>` / `HUSH_FFMPEG`, the installed sidecar, then your
`PATH`.

- Install or repair the sidecar at any time: `hush ffmpeg install` (`--force` to reinstall).
- Use your own FFmpeg instead: install it with your package manager
  (`brew install ffmpeg`, `sudo apt install ffmpeg`, `winget install Gyan.FFmpeg`)
  and install hush with `--no-ffmpeg`.
- See which one is used: `hush ffmpeg path`.

`hush ffmpeg install` downloads FFmpeg 9.0.2 (about 130 MB unpacked) from fixed-version
URLs and checks each file's SHA-256 before using it:

| Platform | Source | License |
|---|---|---|
| macOS (arm64, x86_64) | [martin-riedl.de](https://ffmpeg.martin-riedl.de) static builds | GPL-3.0 |
| Linux (x86_64, arm64), Windows (x64) | [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds), LGPL variant | LGPL-2.1 |

The pins live in
[`crates/hush-cli/src/ffmpeg/sources.toml`](crates/hush-cli/src/ffmpeg/sources.toml).

### Manual install

1. Download `hush-<target>.tar.gz` (or `.zip` on Windows) and its `.sha256` from
   the [releases page](https://github.com/oele-isis-vanderbilt/hush-gst/releases).
2. Check it: `shasum -a 256 -c hush-<target>.tar.gz.sha256`
   (PowerShell: `Get-FileHash -Algorithm SHA256 hush-<target>.zip`).
3. Put `hush` somewhere on your `PATH`, then run `hush ffmpeg install` and `hush doctor`.

Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-msvc`.

### Build from source

Needs Rust 1.88 or newer.

```sh
git clone https://github.com/oele-isis-vanderbilt/hush-gst
cd hush-gst
cargo install --locked --path crates/hush-cli
hush ffmpeg install    # or use an ffmpeg from your package manager
```

### Uninstall

```sh
curl -fsSL https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.sh | sh -s -- --uninstall
```

```powershell
& ([scriptblock]::Create((irm https://github.com/oele-isis-vanderbilt/hush-gst/releases/latest/download/install.ps1))) -Uninstall
```

The uninstaller only deletes a directory that the installer created.

## Quick start

```sh
hush denoise talk.m4a                      # writes talk_denoised.m4a next to it
hush denoise -r -o clean/ recordings/      # a whole folder tree, outputs into clean/
hush denoise "lectures/*.mp4" -j 8         # glob (quoted, so it also works on Windows), 8 files at once
hush denoise --atten-lim-db 20 *.mp3       # gentler: at most 20 dB of noise reduction
hush denoise --raw in.wav                  # 16 kHz mono WAV, exactly what the model produced
hush denoise --dry-run --json recordings/  # show what would happen, as JSON
```

Existing outputs are skipped unless you pass `-y`. Files that were already
denoised (`*_denoised.*`) are skipped when found through a folder or a pattern.

## What happens to your file

1. FFmpeg decodes the first audio stream, mixes all channels to mono and
   resamples to 16 kHz.
2. The model denoises it. Output stays sample-aligned with the input: same length,
   no added delay.
3. FFmpeg encodes the result back into the **same format, codec, sample rate and
   bitrate** as the input. Video streams are copied unchanged; metadata and
   chapters are kept.

The output is **mono**. Supported inputs: WAV, FLAC, MP3, M4A/AAC, MP4, MOV, MKV,
WebM, Ogg and Opus. Other formats: use `--raw`. The full rules are in
[`crates/hush-cli/README.md`](crates/hush-cli/README.md#output-format-rules).

## Troubleshooting

| Problem | Fix |
|---|---|
| `ffmpeg and ffprobe not found` | `hush ffmpeg install`, install FFmpeg with your package manager, or pass `--ffmpeg <path>` |
| `hush: command not found` right after installing | open a new terminal, or `export PATH="$HOME/.hush/bin:$PATH"` |
| macOS: "cannot be opened because the developer cannot be verified" | `xattr -d com.apple.quarantine ~/.hush/bin/hush` (the installer does this for you) |
| `unsupported audio codec` / `unsupported file type` | `--raw` |
| `doctor` warns about a missing encoder | files that need that encoder fail; use an FFmpeg build that has it, or `--raw` |

Exit codes: `0` all done or skipped, `1` some files failed, `2` bad command
line, `3` setup problem (no ffmpeg, bad model), `130` interrupted.

## Development

```sh
cargo build -p hush-cli --release            # target/release/hush
cargo test -p hush-core -p hush-cli --release
cargo test -p hush-core --release -- --ignored --nocapture   # golden-file diagnostics
```

The end-to-end tests in `crates/hush-cli/tests/` need `ffmpeg` and `ffprobe` on
`PATH`; they skip themselves otherwise. Releases are built by
`.github/workflows/release.yml` when a `v*` tag is pushed.

| Path | What |
|---|---|
| `crates/hush-core` | model bundle, `Session` (one stream), `StreamProcessor` / `StreamWorker` |
| `crates/hush-cli` | the `hush` command ([details](crates/hush-cli/README.md)) |
| `crates/hush-gst` | GStreamer plugin |
| `crates/hush-mic-example` | microphone example |
| `third_party/deep_filter` | vendored DeepFilterNet runtime (libDF) |
| `models/advanced_dfnet16k` | the model and its `manifest.json` (SHA-256), embedded into `hush` |
| `install/` | `install.sh` and `install.ps1` |

## Third-party software

- **DeepFilterNet / libDF** (`third_party/deep_filter`): MIT or Apache-2.0.
- **Model** (`models/advanced_dfnet16k`): from the
  [Hush](https://github.com/pulp-vision/Hush) project; see its license terms.
- **FFmpeg** is not part of this repository. `hush ffmpeg install` downloads a
  separate FFmpeg build (LGPL on Linux and Windows, GPL on macOS, see above);
  its source and license are recorded in `NOTICE.txt` in the FFmpeg directory.
  hush runs FFmpeg as a separate program and does not link to it.
