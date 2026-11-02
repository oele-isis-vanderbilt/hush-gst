# Vendored crate: deep_filter (libDF), trimmed

This directory is a vendored, trimmed copy of the `deep_filter` crate. The library target is named `df` (`use df::tract::...`).

## Origin

| Item | Value |
|---|---|
| Upstream project | DeepFilterNet, https://github.com/Rikorose/DeepFilterNet |
| Upstream crate | `deep_filter` v0.5.3 (path `libDF/` in the upstream repo) |
| Upstream tag | v0.5.4 |
| Upstream commit SHA | `10d947c2b183934f34a8852a082e8a0f3c53fbb1` |
| Copied from | Lab fork of Hush, https://github.com/oele-isis-vanderbilt/Hush, vendored copy used to build the `weya_nc` cdylib |
| Hush fork path | Hush/native/weya_nc_build/vendor/deep_filter |
| Hush fork commit SHA | `8a83e4397d0b7f6c190b022b201c309bafa4e9f9` |
| Date copied | 2026-11-02 |

## License

Dual licensed MIT OR Apache-2.0, at your option. Both license files are kept in this directory (`LICENSE-MIT`, `LICENSE-APACHE`). Copyright remains with the upstream authors (Hendrik Schroeter and contributors). Keep these notices in any redistribution.

## Why this copy exists

The element must run the same signal processing (STFT, ERB features, deep filtering, thresholds, state handling) as the build that produced the Hush baseline results. Pinning a copy here keeps results reproducible and builds offline and deterministic. Upstream changes cannot silently alter behavior.
