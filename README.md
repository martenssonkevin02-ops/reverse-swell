# Reverse Swell

A VST3/CLAP reverse reverb that swells into the first hit of a sound. Written in Rust with
[nih-plug](https://github.com/robbert-vdh/nih-plug).

## How it works

The classic reverse reverb is "reverse the audio, add a 100% wet reverb, bounce, reverse again".
For a linear reverb with impulse response `h`, that equals convolving the audio with `h` played
backwards. That needs to see into the future, so the plugin delays the dry signal by the swell
length and reports it as latency; the host compensates, so the swell lands right before the hit.

- `src/dsp.rs`: hit detection (threshold + retrigger gap), the gated source window, the dry delay
  line, the cut-at-hit muting, and the synthetic reverb IR (decorrelated stereo noise with a
  `(1 - t)^curve` envelope, time-varying damping, low/high cut and width baked in).
- `src/convolver.rs`: zero-latency uniformly partitioned FFT convolution, allocation-free.
- `src/lib.rs`: parameters, plugin glue. IRs are rebuilt on a background thread when settings
  change and swapped in lock-free, with a crossfade when the length (latency) stays the same.
- `src/editor.rs`: egui GUI.

## Building

GitHub Actions builds the Mac version on every push (`.github/workflows/build-mac.yml`): a
universal (Apple Silicon + Intel), ad-hoc signed VST3, checked with Steinberg's validator, an
audio test in a real host (`ci/host_test.py`) and a GUI/drag-out test (`ci/mac_gui_test.py`).

By hand, on a Mac with Rust and Xcode's command line tools:

    rustup target add x86_64-apple-darwin aarch64-apple-darwin
    cargo xtask bundle-universal reverse_swell --release --features objc-exception-guard

The plugin ends up in `target/bundled/Reverse Swell.vst3`.

## Rendering swells

Every swell is also captured (`Engine::publish_capture` → `CaptureSlot`) and shown in the GUI's
LAST SWELL box. Dragging it writes a 32-bit float WAV of the swell only, ending at the hit, to
`~/Music/Reverse Swell`, and starts a macOS file drag (`src/drag_mac.rs`) so it can be dropped
into the DAW.

## Testing

- `cargo test --release --lib`: checks the convolver against direct convolution.
- `cargo run --release --example render -- <out_dir>`: renders a synthetic supersaw stab through
  the engine, prints timing/level checks, checks no memory is allocated while processing, and
  writes WAVs.

## License

GPL-3.0-or-later (nih-plug's VST3 bindings are GPLv3).
