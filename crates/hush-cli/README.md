# hush-cli

The `hush` command: batch noise suppression for audio and video files, built on
[`hush-core`](../hush-core). Installation and a quick start are in the
[top-level README](../../README.md).

## How a file is processed

```text
input ──ffmpeg──▶ f32le 16 kHz mono ──StreamProcessor──▶ f32le 16 kHz mono ──ffmpeg──▶ .hush-XXXX temp ──rename──▶ output
       (decode,                        (DelayMode::Aligned,                     (encode to the input's codec,
        mix to mono,                    output length = input length)            sample rate and bitrate; copy
        resample)                                                               video, metadata, chapters)
```

- The three stages run at the same time over pipes, so memory use stays flat
  (about 60 MB) however long the file is.
- Output is written to a hidden temp file next to the output and renamed only
  after a check (ffprobe: channel count and duration match). A failure or Ctrl-C leaves
  nothing behind.
- `--per-channel` keeps the input's channels (e.g. one mic on L, one on R): the
  decoder outputs all channels at 16 kHz, each channel goes through its own
  model session, and the channels are interleaved again before encoding. Every
  session runs with the model delay removed, so the channels stay in sync with
  each other and with the video. Costs about N times the CPU for N channels.
- `--jobs N` processes N files in parallel. Each file gets its own model
  session; the model weights are shared.

## Output format rules

| Input codec | Written with | Kept |
|---|---|---|
| `pcm_*` (WAV) | same PCM codec | bit depth, sample rate |
| FLAC | `flac` | 16 or 24 bit, sample rate |
| MP3 | `libmp3lame` | bitrate, sample rate |
| AAC (m4a, mp4, mov, mkv, aac) | `aac` | bitrate, sample rate |
| ALAC | `alac` | sample rate |
| Opus / Vorbis | `libopus` / `libvorbis` | bitrate, sample rate |

Output is mono unless `--per-channel` is given, in which case it has the input's
channel count (written with ffmpeg's default layout for that count; MP3 holds at
most 2). Video streams are copied only into video containers
(mp4, mov, mkv, webm); subtitles only into mkv. Cover art is dropped. Other
codecs or extensions are refused with a hint to use `--raw` (16 kHz mono WAV).

## Where ffmpeg comes from

1. `--ffmpeg <path>` or `HUSH_FFMPEG` (a binary, or a directory holding both tools)
2. the sidecar next to the install: `<install root>/ffmpeg/`, e.g. `~/.hush/ffmpeg/`
3. the sidecar in the user data dir (`~/Library/Application Support/hush/ffmpeg`,
   `~/.local/share/hush/ffmpeg`, `%LOCALAPPDATA%\hush\ffmpeg`)
4. `PATH`

ffprobe is always taken from the same place as ffmpeg. `hush ffmpeg path` shows
the choice; `hush doctor` also checks the encoders listed above.

`hush ffmpeg install` downloads the build pinned in
[`src/ffmpeg/sources.toml`](src/ffmpeg/sources.toml), checks its SHA-256, and
installs `ffmpeg`, `ffprobe` and a `NOTICE.txt` into the sidecar directory.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | every file done or skipped |
| 1 | at least one file failed (the others were still processed) |
| 2 | invalid command line |
| 3 | setup error: ffmpeg not found, model invalid, ... |
| 130 | interrupted (Ctrl-C) |

## Command reference

Generated from the clap definitions in `src/cli.rs`. A test fails when it is out
of date; refresh it with `UPDATE_README=1 cargo test -p hush-cli readme`.

<!-- help:start -->
### `hush`

```text
Remove background noise from audio and video files

Usage: hush [OPTIONS] <COMMAND>

Commands:
  denoise      Denoise files, directories or glob patterns
  ffmpeg       Manage the FFmpeg sidecar
  doctor       Check that ffmpeg, ffprobe and the model are usable
  completions  Print a shell completion script
  help         Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help (see more with '--help')
  -V, --version  Print version

Global options:
  -v, --verbose...     More output (repeat for more detail)
  -q, --quiet          Only print errors
      --ffmpeg <PATH>  Path to the ffmpeg binary; ffprobe is looked up in the same directory [env:
                       HUSH_FFMPEG=]
      --color <COLOR>  When to use colors [default: auto] [possible values: auto, always, never]
```

### `hush denoise`

```text
Denoise files, directories or glob patterns

Usage: hush denoise [OPTIONS] <INPUTS>...

Arguments:
  <INPUTS>...  Files, directories, or glob patterns (e.g. "talks/*.m4a")

Options:
  -o, --output-dir <DIR>   Write outputs here instead of next to each input
      --suffix <SUFFIX>    Added to each output file name: talk.m4a -> talk_denoised.m4a [default:
                           _denoised]
  -r, --recursive          Descend into subdirectories
      --ext <EXT>          Extensions taken from directories (comma separated) [default: wav flac
                           mp3 m4a aac mp4 mov mkv ogg opus webm]
  -y, --overwrite          Replace existing outputs (default: skip them)
  -j, --jobs <JOBS>        Number of files processed in parallel [default: min(4, CPUs)]
      --atten-lim-db <DB>  Maximum noise reduction in dB (0-100, 100 = unlimited) [default: 100]
      --raw                Write the 16 kHz mono model output as WAV instead of the original format
      --per-channel        Denoise each channel separately and keep them (e.g. separate L/R mics)
                           instead of mixing to mono
      --model <DIR>        Model directory with manifest.json [default: built-in model]
      --dry-run            List input -> output pairs without processing
      --json               Print a machine-readable summary on stdout
  -h, --help               Print help
  -V, --version            Print version

Global options:
  -v, --verbose...     More output (repeat for more detail)
  -q, --quiet          Only print errors
      --ffmpeg <PATH>  Path to the ffmpeg binary; ffprobe is looked up in the same directory [env:
                       HUSH_FFMPEG=]
      --color <COLOR>  When to use colors [default: auto] [possible values: auto, always, never]
```

### `hush ffmpeg install`

```text
Download and verify the pinned FFmpeg build

Usage: hush ffmpeg install [OPTIONS]

Options:
      --force      Reinstall even if a working sidecar exists
      --dir <DIR>  Install into this directory instead of the default sidecar location
  -h, --help       Print help
  -V, --version    Print version

Global options:
  -v, --verbose...     More output (repeat for more detail)
  -q, --quiet          Only print errors
      --ffmpeg <PATH>  Path to the ffmpeg binary; ffprobe is looked up in the same directory [env:
                       HUSH_FFMPEG=]
      --color <COLOR>  When to use colors [default: auto] [possible values: auto, always, never]
```

### `hush ffmpeg path`

```text
Show which ffmpeg and ffprobe would be used, and why

Usage: hush ffmpeg path [OPTIONS]

Options:
  -h, --help     Print help
  -V, --version  Print version

Global options:
  -v, --verbose...     More output (repeat for more detail)
  -q, --quiet          Only print errors
      --ffmpeg <PATH>  Path to the ffmpeg binary; ffprobe is looked up in the same directory [env:
                       HUSH_FFMPEG=]
      --color <COLOR>  When to use colors [default: auto] [possible values: auto, always, never]
```

### `hush doctor`

```text
Check that ffmpeg, ffprobe and the model are usable

Usage: hush doctor [OPTIONS]

Options:
  -h, --help     Print help
  -V, --version  Print version

Global options:
  -v, --verbose...     More output (repeat for more detail)
  -q, --quiet          Only print errors
      --ffmpeg <PATH>  Path to the ffmpeg binary; ffprobe is looked up in the same directory [env:
                       HUSH_FFMPEG=]
      --color <COLOR>  When to use colors [default: auto] [possible values: auto, always, never]
```

### `hush completions`

```text
Print a shell completion script

Usage: hush completions [OPTIONS] <SHELL>

Arguments:
  <SHELL>  [possible values: bash, elvish, fish, powershell, zsh]

Options:
  -h, --help     Print help
  -V, --version  Print version

Global options:
  -v, --verbose...     More output (repeat for more detail)
  -q, --quiet          Only print errors
      --ffmpeg <PATH>  Path to the ffmpeg binary; ffprobe is looked up in the same directory [env:
                       HUSH_FFMPEG=]
      --color <COLOR>  When to use colors [default: auto] [possible values: auto, always, never]
```

<!-- help:end -->
