# Golden test sample for hush-core

These files are copied from the upstream Hush repo. The golden tests in
`tests/golden/` feed the raw file through `StreamProcessor` and compare the
result with the denoised file. See `test-plan.md` at the repo root.

## Source

| Item | Value |
|---|---|
| Upstream repo | https://github.com/pulp-vision/Hush |
| Path | `assets/audio/` |
| Commit | `2a70e77` (2026-03-20, "docs: actually add the audio files (forced)") |
| Date copied | 2026-11-03 |

## Files

| File | Format | SHA-256 |
|---|---|---|
| `sample_00006_raw.wav` | mono, 16 kHz, s16le, 80 000 frames (5.0 s) | `0d5ad72703bbed8794c04e9e60b86eb66af7d6167931fc3bcf427fd1671e05df` |
| `sample_00006_denoised.wav` | mono, 16 kHz, s16le, 80 000 frames (5.0 s) | `8946bbce91695aee41b4c4ea1df3e9e148c6de452f43e4f9bc37e100572d8d4f` |

Check with `shasum -a 256 *.wav`.

## How the denoised file was made: unknown

The denoised file is older than both upstream scripts that could have made it:

- `scripts/infer_single.py` (added 2026-04-01): PyTorch, whole file at once,
  delay-compensated, no LSNR thresholds, soundfile int16 conversion (×32767, rounded).
- `deployment/examples/python/denoise_ctypes.py` with `weya_nc` (tract, frame by
  frame, same code path as hush-core): no delay compensation, int16 conversion
  `(y * 32768).clip().astype(int16)` (truncated).

If it came from the PyTorch path, bit-exact equality with hush-core is not
expected. The diagnostic test in `tests/golden/` measures which one it matches.
