//! The reverse reverb engine. Independent from the plugin framework so it can be tested offline.
//!
//! How it works:
//! A classic "reverse reverb" is made by reversing a sound, putting a 100% wet reverb on it,
//! bouncing, and reversing the result again. For a linear reverb with impulse response `h`, that
//! process equals convolving the original sound with `h` played *backwards*. Backwards means it
//! needs to look into the future, so the plugin delays the dry signal by the swell length and
//! reports that delay to the host as latency. The host (FL Studio) compensates for it, so the swell
//! lands right before the hit on the timeline.
//!
//! Only the first part of each new sound (the "source" window, opened when the input crosses the
//! threshold after a stretch of silence) is fed to the reverse reverb. That makes the swell suck
//! into the first hit instead of smearing the whole phrase.

use crate::convolver::{Convolver, BLOCK};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

/// Longest supported swell.
pub const MAX_LENGTH_MS: f32 = 4000.0;
pub const MIN_LENGTH_MS: f32 = 50.0;
/// Internal processing chunk size.
const CHUNK: usize = 256;
/// Fade used when muting the wet signal at the hit.
const CUT_FADE_MS: f32 = 3.0;
/// Crossfade used when swapping impulse responses.
const SWAP_FADE_MS: f32 = 25.0;
/// Overall wet calibration so the default settings give a swell around the level of the hit.
const WET_CALIBRATION: f32 = 1.0;

/// Everything that defines the impulse response. When this changes, a new IR gets built.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IrSpec {
    pub sample_rate: f32,
    /// Swell length in samples. Also the plugin's latency (minus one sample).
    pub length: usize,
    /// Envelope exponent. Higher = the swell builds later and sucks in harder.
    pub curve: f32,
    /// 0..1, how much darker the early part of the swell is.
    pub darkness: f32,
    pub low_cut_hz: f32,
    pub high_cut_hz: f32,
    /// 0..1 stereo width.
    pub width: f32,
}

impl IrSpec {
    /// Whether two specs are close enough that rebuilding isn't worth it.
    pub fn roughly_equal(&self, other: &IrSpec) -> bool {
        self.sample_rate == other.sample_rate
            && self.length == other.length
            && (self.curve - other.curve).abs() < 0.01
            && (self.darkness - other.darkness).abs() < 0.005
            && (self.low_cut_hz / other.low_cut_hz - 1.0).abs() < 0.01
            && (self.high_cut_hz / other.high_cut_hz - 1.0).abs() < 0.01
            && (self.width - other.width).abs() < 0.005
    }
}

/// Swell length in samples for a length in milliseconds.
pub fn length_samples(length_ms: f32, sample_rate: f32) -> usize {
    let ms = length_ms.clamp(MIN_LENGTH_MS, MAX_LENGTH_MS);
    ((ms / 1000.0 * sample_rate).round() as usize).max(BLOCK)
}

/// A stereo pair of convolvers plus the spec they were built from. Built on a background thread.
pub struct SwellIr {
    pub spec: IrSpec,
    conv: [Convolver; 2],
}

impl SwellIr {
    pub fn new(spec: IrSpec) -> Self {
        let [l, r] = build_ir(&spec);
        // The convolver plays the IR backwards, so reverse it here
        let rev = |v: &[f32]| v.iter().rev().copied().collect::<Vec<f32>>();
        Self {
            spec,
            conv: [Convolver::new(&rev(&l)), Convolver::new(&rev(&r))],
        }
    }

    /// Latency in samples that aligns the end of the swell with the dry hit.
    pub fn latency(&self) -> usize {
        self.spec.length - 1
    }

    fn reset(&mut self) {
        self.conv[0].reset();
        self.conv[1].reset();
    }
}

/// Simple RBJ biquad, used only while building the IR.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn new(highpass: bool, freq: f32, sample_rate: f32) -> Self {
        let freq = freq.clamp(10.0, sample_rate * 0.49);
        let w0 = 2.0 * std::f32::consts::PI * freq / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * std::f32::consts::FRAC_1_SQRT_2);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = if highpass {
            ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0)
        } else {
            ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0)
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: -2.0 * cos / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Builds a stereo, synthetic reverb tail: decorrelated noise with an envelope that reaches zero
/// at exactly `length` samples, and highs that die away faster than lows (like a real room).
pub fn build_ir(spec: &IrSpec) -> [Vec<f32>; 2] {
    let n = spec.length;
    let sr = spec.sample_rate;
    let mut out = [vec![0.0f32; n], vec![0.0f32; n]];

    let mut seed: u32 = 0x9E37_79B9;
    let mut noise = move || {
        // xorshift32 + a cheap approximation of a Gaussian (sum of 4 uniforms)
        let mut acc = 0.0f32;
        for _ in 0..4 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            acc += seed as f32 / u32::MAX as f32;
        }
        (acc - 2.0) * 1.732
    };

    // Two cascaded one-pole lowpasses per channel whose cutoff drops further into the tail
    let mut lp = [[0.0f32; 2]; 2];
    let bright_hz = 18_000.0f32.min(sr * 0.45);
    let dark_hz = 600.0f32;
    let attack = ((0.0015 * sr) as usize).max(1);

    for i in 0..n {
        let t = i as f32 / n as f32;
        let env = (1.0 - t).powf(spec.curve);
        let cutoff = bright_hz * (dark_hz / bright_hz).powf(spec.darkness * t.powf(0.6));
        let a = 1.0 - (-2.0 * std::f32::consts::PI * cutoff / sr).exp();
        // Tiny fade-in so the IR doesn't start on a hard edge
        let fade_in = if i < attack {
            0.5 - 0.5 * (std::f32::consts::PI * i as f32 / attack as f32).cos()
        } else {
            1.0
        };
        for (ch, state) in lp.iter_mut().enumerate() {
            let x = noise();
            state[0] += a * (x - state[0]);
            state[1] += a * (state[0] - state[1]);
            out[ch][i] = state[1] * env * fade_in;
        }
    }

    // Stereo width (mid/side)
    for i in 0..n {
        let mid = 0.5 * (out[0][i] + out[1][i]);
        let side = 0.5 * (out[0][i] - out[1][i]) * spec.width;
        out[0][i] = mid + side;
        out[1][i] = mid - side;
    }

    // Low cut (24 dB/oct) and high cut (12 dB/oct) baked into the IR
    for ch in out.iter_mut() {
        let mut hp1 = Biquad::new(true, spec.low_cut_hz, sr);
        let mut hp2 = Biquad::new(true, spec.low_cut_hz, sr);
        let mut lp = Biquad::new(false, spec.high_cut_hz, sr);
        for s in ch.iter_mut() {
            *s = lp.run(hp2.run(hp1.run(*s)));
        }
    }

    // Normalise the energy so the swell volume doesn't jump around when changing settings
    let energy: f64 = out
        .iter()
        .flat_map(|ch| ch.iter())
        .map(|&s| (s as f64) * (s as f64))
        .sum::<f64>()
        / 2.0;
    if energy > 0.0 {
        let gain = (WET_CALIBRATION as f64 / energy.sqrt()) as f32;
        for ch in out.iter_mut() {
            ch.iter_mut().for_each(|s| *s *= gain);
        }
    }

    out
}

/// Per-block settings that don't require rebuilding the IR.
#[derive(Debug, Clone, Copy)]
pub struct EngineSettings {
    pub threshold_db: f32,
    /// How much of the first hit feeds the reverb, in ms.
    pub source_ms: f32,
    /// Silence needed before the next swell can trigger, in ms.
    pub gap_ms: f32,
    pub cut_at_hit: bool,
    /// Linear wet gain.
    pub wet_gain: f32,
    pub dry: bool,
}

/// The most recent swell, handed from the audio thread to the GUI.
pub struct CaptureData {
    /// Preallocated buffers; only the first `len` samples are valid.
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    pub len: usize,
    pub sample_rate: f32,
    /// Tempo at capture time, for naming the file.
    pub bpm: f32,
    /// Tempo-synced length index, or -1 when the length was set in ms.
    pub note_index: i32,
}

pub struct CaptureSlot {
    pub data: Mutex<CaptureData>,
    /// Bumped every time a new swell has been captured.
    pub version: AtomicU32,
}

impl Default for CaptureSlot {
    fn default() -> Self {
        Self {
            data: Mutex::new(CaptureData {
                left: Vec::new(),
                right: Vec::new(),
                len: 0,
                sample_rate: 0.0,
                bpm: 0.0,
                note_index: -1,
            }),
            version: AtomicU32::new(0),
        }
    }
}

impl CaptureSlot {
    /// Makes room for the longest possible swell. Call from a non-realtime thread.
    pub fn ensure_capacity(&self, sample_rate: f32) {
        let needed = length_samples(MAX_LENGTH_MS, sample_rate);
        let mut d = self.data.lock();
        if d.left.len() < needed {
            d.left.resize(needed, 0.0);
            d.right.resize(needed, 0.0);
        }
    }
}

#[derive(Clone, Copy)]
struct CutEvent {
    start: u64,
    end: u64,
}

const MAX_EVENTS: usize = 64;

pub struct Engine {
    sample_rate: f32,

    ir: Option<Box<SwellIr>>,
    /// The previous IR while crossfading to a new one with the same length.
    old_ir: Option<Box<SwellIr>>,
    swap_pos: usize,
    swap_len: usize,

    delay: usize,
    delay_buf: [Vec<f32>; 2],
    write_pos: usize,

    // Hit detection
    env: f32,
    env_release: f32,
    armed: bool,
    quiet_samples: usize,

    // Source gate
    gate_pos: usize,
    gate_len: usize,

    // Wet muting at the hit
    events: [CutEvent; MAX_EVENTS],
    event_read: usize,
    event_count: usize,
    cut_gain: f32,
    cut_step: f32,

    /// Running sample counter (input and output share it; output = input delayed by `delay`).
    now: u64,
    silent_samples: usize,
    wet_gain: f32,

    triggers: u32,

    /// Recent wet output (swell only), indexed by output time, for capturing.
    hist: [Vec<f32>; 2],
    /// Hit time and swell length of the newest swell that still has to be captured.
    pending_capture: Option<(u64, usize)>,

    gated: [[f32; CHUNK]; 2],
    wet: [[f32; CHUNK]; 2],
    wet_old: [[f32; CHUNK]; 2],
}

impl Engine {
    pub fn new(sample_rate: f32) -> Self {
        let max_delay = length_samples(MAX_LENGTH_MS, sample_rate) + CHUNK + 16;
        // Room for the longest swell plus a second of slack in case the GUI holds the lock
        let hist_len = max_delay + sample_rate as usize;
        Self {
            sample_rate,
            ir: None,
            old_ir: None,
            swap_pos: 0,
            swap_len: ((SWAP_FADE_MS / 1000.0) * sample_rate) as usize,
            delay: 0,
            delay_buf: [vec![0.0; max_delay], vec![0.0; max_delay]],
            write_pos: 0,
            env: 0.0,
            env_release: (-1.0 / (0.030 * sample_rate)).exp(),
            armed: true,
            quiet_samples: 0,
            gate_pos: 0,
            gate_len: 0,
            events: [CutEvent { start: 0, end: 0 }; MAX_EVENTS],
            event_read: 0,
            event_count: 0,
            cut_gain: 1.0,
            cut_step: 1.0 / (CUT_FADE_MS / 1000.0 * sample_rate).max(1.0),
            now: 0,
            silent_samples: usize::MAX / 2,
            wet_gain: 1.0,
            triggers: 0,
            hist: [vec![0.0; hist_len], vec![0.0; hist_len]],
            pending_capture: None,
            gated: [[0.0; CHUNK]; 2],
            wet: [[0.0; CHUNK]; 2],
            wet_old: [[0.0; CHUNK]; 2],
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Current latency in samples.
    pub fn latency(&self) -> usize {
        self.delay
    }

    pub fn current_spec(&self) -> Option<IrSpec> {
        self.ir.as_ref().map(|ir| ir.spec)
    }

    /// Number of swells triggered so far (for the GUI's trigger light).
    pub fn trigger_count(&self) -> u32 {
        self.triggers
    }

    /// Installs a new IR. Returns whatever the caller should dispose of off the audio thread.
    pub fn install_ir(&mut self, ir: Box<SwellIr>) -> [Option<Box<SwellIr>>; 2] {
        let mut garbage = [None, None];
        match self.ir.take() {
            Some(old) if old.spec.length == ir.spec.length => {
                // Same latency: crossfade so tone tweaks don't click
                garbage[0] = self.old_ir.replace(old);
                self.swap_pos = 0;
            }
            Some(old) => {
                // Latency changes and the host re-aligns anyway, so switch hard
                garbage[0] = Some(old);
                garbage[1] = self.old_ir.take();
            }
            None => {}
        }
        self.delay = ir.latency();
        self.ir = Some(ir);
        garbage
    }

    /// Returns the crossfade's old IR once it's no longer needed.
    pub fn take_finished_old_ir(&mut self) -> Option<Box<SwellIr>> {
        if self.old_ir.is_some() && self.swap_pos >= self.swap_len {
            self.old_ir.take()
        } else {
            None
        }
    }

    pub fn reset(&mut self) {
        for buf in self.delay_buf.iter_mut() {
            buf.iter_mut().for_each(|s| *s = 0.0);
        }
        if let Some(ir) = self.ir.as_mut() {
            ir.reset();
        }
        if let Some(ir) = self.old_ir.as_mut() {
            ir.reset();
        }
        self.swap_pos = self.swap_len;
        self.env = 0.0;
        self.armed = true;
        self.quiet_samples = 0;
        self.gate_pos = 0;
        self.gate_len = 0;
        self.event_read = 0;
        self.event_count = 0;
        self.cut_gain = 1.0;
        self.silent_samples = usize::MAX / 2;
        for buf in self.hist.iter_mut() {
            buf.iter_mut().for_each(|s| *s = 0.0);
        }
        self.pending_capture = None;
    }

    /// Copies the newest finished swell into `slot` for the GUI. Realtime safe: gives up if the
    /// GUI is holding the lock and tries again on the next call. Returns true when published.
    pub fn publish_capture(&mut self, slot: &CaptureSlot, bpm: f32, note_index: i32) -> bool {
        let Some((hit, len)) = self.pending_capture else {
            return false;
        };
        // The whole swell (everything up to the hit) must have been output already
        if self.now < hit {
            return false;
        }
        let hist_len = self.hist[0].len();
        if self.now - hit + len as u64 > hist_len as u64 {
            // Waited too long, it's been overwritten
            self.pending_capture = None;
            return false;
        }
        let Some(mut d) = slot.data.try_lock() else {
            return false;
        };
        if d.left.len() < len || d.right.len() < len {
            self.pending_capture = None;
            return false;
        }
        for k in 0..len {
            let t = hit as i64 - len as i64 + k as i64;
            let (l, r) = if t < 0 {
                (0.0, 0.0)
            } else {
                let idx = t as usize % hist_len;
                (self.hist[0][idx], self.hist[1][idx])
            };
            d.left[k] = l;
            d.right[k] = r;
        }
        d.len = len;
        d.sample_rate = self.sample_rate;
        d.bpm = bpm;
        d.note_index = note_index;
        drop(d);
        slot.version.fetch_add(1, Ordering::Release);
        self.pending_capture = None;
        true
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], settings: &EngineSettings) {
        let mut pos = 0;
        while pos < left.len() {
            let n = (left.len() - pos).min(CHUNK);
            self.process_chunk(&mut left[pos..pos + n], &mut right[pos..pos + n], settings);
            pos += n;
        }
    }

    fn process_chunk(&mut self, left: &mut [f32], right: &mut [f32], s: &EngineSettings) {
        let n = left.len();
        let sr = self.sample_rate;
        let threshold = 10f32.powf(s.threshold_db / 20.0);
        let rearm_level = threshold * 0.5;
        let gap_samples = (s.gap_ms / 1000.0 * sr) as usize;
        let buf_len = self.delay_buf[0].len();
        let chunk_start = self.write_pos;

        // 1. Detect hits, build the gated source, and write the dry delay line
        let mut any_source = false;
        for i in 0..n {
            let (l, r) = (left[i], right[i]);
            let peak = l.abs().max(r.abs());
            self.env = if peak > self.env {
                peak
            } else {
                self.env * self.env_release
            };

            let gate_open = self.gate_pos < self.gate_len;
            if !self.armed {
                if self.env < rearm_level {
                    self.quiet_samples += 1;
                } else {
                    self.quiet_samples = 0;
                }
                if self.quiet_samples >= gap_samples && !gate_open {
                    self.armed = true;
                }
            }

            if self.armed && peak >= threshold && !gate_open {
                self.armed = false;
                self.quiet_samples = 0;
                self.gate_pos = 0;
                self.gate_len = ((s.source_ms / 1000.0 * sr) as usize).max(16);
                self.triggers = self.triggers.wrapping_add(1);
                let hit_at = self.now + i as u64 + self.delay as u64;
                if let Some(ir) = self.ir.as_ref() {
                    self.pending_capture = Some((hit_at, ir.spec.length));
                }
                if self.event_count < MAX_EVENTS {
                    let idx = (self.event_read + self.event_count) % MAX_EVENTS;
                    self.events[idx] = CutEvent {
                        start: hit_at,
                        end: hit_at + self.gate_len as u64,
                    };
                    self.event_count += 1;
                }
            }

            let g = if self.gate_pos < self.gate_len {
                let w = gate_window(self.gate_pos, self.gate_len, sr);
                self.gate_pos += 1;
                any_source = true;
                w
            } else {
                0.0
            };
            self.gated[0][i] = l * g;
            self.gated[1][i] = r * g;

            let wp = (chunk_start + i) % buf_len;
            self.delay_buf[0][wp] = l;
            self.delay_buf[1][wp] = r;
        }
        self.write_pos = (chunk_start + n) % buf_len;

        // 2. Reverse reverb on the gated source. Skipped entirely once everything has decayed.
        if any_source {
            self.silent_samples = 0;
        } else {
            self.silent_samples = self.silent_samples.saturating_add(n);
        }
        let ir_len = self.ir.as_ref().map(|ir| ir.spec.length).unwrap_or(0);
        let active = self.silent_samples <= ir_len + 2 * BLOCK;
        let crossfading = self.old_ir.is_some() && self.swap_pos < self.swap_len;
        match self.ir.as_mut() {
            Some(ir) if active => {
                for ch in 0..2 {
                    ir.conv[ch].process(&self.gated[ch][..n], &mut self.wet[ch][..n]);
                }
            }
            _ => {
                for ch in 0..2 {
                    self.wet[ch][..n].iter_mut().for_each(|x| *x = 0.0);
                }
            }
        }
        if crossfading {
            if let Some(old) = self.old_ir.as_mut() {
                if active {
                    for ch in 0..2 {
                        old.conv[ch].process(&self.gated[ch][..n], &mut self.wet_old[ch][..n]);
                    }
                } else {
                    for ch in 0..2 {
                        self.wet_old[ch][..n].iter_mut().for_each(|x| *x = 0.0);
                    }
                }
                for i in 0..n {
                    let a = ((self.swap_pos + i) as f32 / self.swap_len as f32).min(1.0);
                    for ch in 0..2 {
                        self.wet[ch][i] = self.wet[ch][i] * a + self.wet_old[ch][i] * (1.0 - a);
                    }
                }
                self.swap_pos += n;
            }
        }

        // 3. Mix delayed dry + wet, muting the wet at each hit if requested
        let hist_len = self.hist[0].len() as u64;
        let gain_start = self.wet_gain;
        let gain_step = (s.wet_gain - gain_start) / n as f32;
        for i in 0..n {
            let t = self.now + i as u64;
            let mut target = 1.0;
            while self.event_count > 0 {
                let ev = self.events[self.event_read];
                if t >= ev.end {
                    self.event_read = (self.event_read + 1) % MAX_EVENTS;
                    self.event_count -= 1;
                    continue;
                }
                if t >= ev.start {
                    target = 0.0;
                }
                break;
            }
            if !s.cut_at_hit {
                target = 1.0;
            }
            if self.cut_gain < target {
                self.cut_gain = (self.cut_gain + self.cut_step).min(target);
            } else if self.cut_gain > target {
                self.cut_gain = (self.cut_gain - self.cut_step).max(target);
            }

            let rp = (chunk_start + i + buf_len - self.delay) % buf_len;
            let (dl, dr) = if s.dry {
                (self.delay_buf[0][rp], self.delay_buf[1][rp])
            } else {
                (0.0, 0.0)
            };
            let gain = gain_start + gain_step * (i + 1) as f32;
            let (wl, wr) = (self.wet[0][i] * gain, self.wet[1][i] * gain);
            let hi = (t % hist_len) as usize;
            self.hist[0][hi] = wl;
            self.hist[1][hi] = wr;
            left[i] = dl + wl * self.cut_gain;
            right[i] = dr + wr * self.cut_gain;
        }
        self.wet_gain = s.wet_gain;
        self.now += n as u64;
    }
}

/// Short fade in at the start of the source window and a longer fade out at the end, so gating
/// doesn't put clicks into the reverb.
fn gate_window(pos: usize, len: usize, sample_rate: f32) -> f32 {
    let fade_in = ((0.001 * sample_rate) as usize).min(len / 4).max(1);
    let fade_out = ((0.010 * sample_rate) as usize).min(len / 4).max(1);
    if pos < fade_in {
        let x = pos as f32 / fade_in as f32;
        0.5 - 0.5 * (std::f32::consts::PI * x).cos()
    } else if pos + fade_out >= len {
        let x = (len - pos) as f32 / fade_out as f32;
        0.5 - 0.5 * (std::f32::consts::PI * x.min(1.0)).cos()
    } else {
        1.0
    }
}
