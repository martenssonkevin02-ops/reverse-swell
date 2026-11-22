//! Offline test: renders a synthetic hardstyle stab through the engine and checks the result.
//! Usage: cargo run --release --example render -- <out_dir>

use reverse_swell::dsp::{length_samples, CaptureSlot, Engine, EngineSettings, IrSpec, SwellIr};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct CountingAlloc;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

const SR: f32 = 48_000.0;

/// Supersaw chord stab, like a hardstyle lead hit.
fn stab(len_s: f32, freqs: &[f32], gain: f32) -> Vec<f32> {
    let n = (len_s * SR) as usize;
    let detune = [-0.11f32, -0.06, -0.02, 0.0, 0.02, 0.06, 0.11];
    let mut out = vec![0.0f32; n];
    for &f in freqs {
        for (k, d) in detune.iter().enumerate() {
            let freq = f * 2f32.powf(d / 12.0);
            let mut phase = k as f32 * 0.137;
            for (i, s) in out.iter_mut().enumerate() {
                let t = i as f32 / SR;
                let env = (t / 0.002).min(1.0) * (-t * 2.2).exp();
                *s += (2.0 * phase - 1.0) * env;
                phase = (phase + freq / SR).fract();
                let _ = t;
            }
        }
    }
    let peak = out.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
    out.iter_mut().for_each(|s| *s *= gain / peak);
    out
}

fn write_wav(path: &str, l: &[f32], r: &[f32]) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SR as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..l.len() {
        w.write_sample(l[i]).unwrap();
        w.write_sample(r[i]).unwrap();
    }
    w.finalize().unwrap();
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|s| s * s).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn db(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

fn main() {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let a_minor = [220.0f32, 261.63, 329.63];

    // 1.5 s silence, a stab, 1.2 s silence, then three quick stabs with short gaps (only the
    // first of those should trigger, since the gaps are shorter than the retrigger gap)
    let mut input = vec![0.0f32; (1.5 * SR) as usize];
    let onset1 = input.len();
    input.extend(stab(0.7, &a_minor, 0.5));
    input.extend(vec![0.0; (1.2 * SR) as usize]);
    let onset2 = input.len();
    for _ in 0..3 {
        input.extend(stab(0.25, &a_minor, 0.5));
        input.extend(vec![0.0; (0.12 * SR) as usize]);
    }
    input.extend(vec![0.0; (1.5 * SR) as usize]);

    let spec = IrSpec {
        sample_rate: SR,
        length: length_samples(800.0, SR),
        curve: 3.0,
        darkness: 0.4,
        low_cut_hz: 250.0,
        high_cut_hz: 14_000.0,
        width: 1.0,
    };
    let base = EngineSettings {
        threshold_db: -30.0,
        source_ms: 120.0,
        gap_ms: 250.0,
        cut_at_hit: true,
        wet_gain: 1.0,
        dry: true,
    };

    let run = |settings: EngineSettings, block: usize| -> (Vec<f32>, Vec<f32>, usize, u32, usize) {
        let mut engine = Engine::new(SR);
        for g in engine.install_ir(Box::new(SwellIr::new(spec))) {
            drop(g);
        }
        let mut l = input.clone();
        let mut r = input.clone();
        let mut pos = 0;
        ALLOCS.store(0, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        while pos < l.len() {
            let n = block.min(l.len() - pos);
            engine.process(&mut l[pos..pos + n], &mut r[pos..pos + n], &settings);
            pos += n;
        }
        COUNTING.store(false, Ordering::Relaxed);
        let allocs = ALLOCS.load(Ordering::Relaxed);
        (l, r, engine.latency(), engine.trigger_count(), allocs)
    };

    let (out_l, out_r, latency, triggers, allocs) = run(base, 512);
    let (wet_l, wet_r, _, _, _) = run(EngineSettings { dry: false, ..base }, 512);
    let (wet_nocut, _, _, _, _) =
        run(EngineSettings { dry: false, cut_at_hit: false, ..base }, 512);
    // Odd block size must give the same result
    let (odd_l, _, _, _, _) = run(base, 97);

    let s = spec.length;
    let hit1 = onset1 + latency;
    let hit2 = onset2 + latency;
    println!("latency: {latency} samples ({:.1} ms)", latency as f32 / SR * 1000.0);
    println!("triggers: {triggers} (expected 2)");
    println!("allocations during processing: {allocs} (expected 0)");

    // Dry alignment: the delayed dry stab should start exactly at onset + latency
    let dry_l: Vec<f32> = out_l.iter().zip(wet_l.iter()).map(|(o, w)| o - w).collect();
    let first_dry = dry_l.iter().position(|s| s.abs() > 1e-6).unwrap();
    println!("dry signal starts at {first_dry}, expected {hit1}");

    // Swell placement
    let ms = |x: f32| (x / 1000.0 * SR) as usize;
    let win = ms(40.0);
    let before_swell = rms(&wet_l[hit1 - s - ms(300.0)..hit1 - s - ms(10.0)]);
    println!("\nwet level by time relative to the first hit (40 ms windows):");
    for &t in &[-800.0f32, -700.0, -600.0, -500.0, -400.0, -300.0, -200.0, -100.0, -40.0] {
        let start = (hit1 as i64 + ms(t.abs()) as i64 * t.signum() as i64) as usize;
        println!("  {t:>6.0} ms: {:>6.1} dB", db(rms(&wet_l[start..start + win])));
    }
    let after = rms(&wet_l[hit1 + ms(5.0)..hit1 + ms(100.0)]);
    let after_nocut = rms(&wet_nocut[hit1 + ms(5.0)..hit1 + ms(100.0)]);
    println!("  after hit with cut: {:.1} dB, without cut: {:.1} dB", db(after), db(after_nocut));
    println!("  before the swell window: {:.1} dB", db(before_swell));

    let swell_peak = wet_l[hit1 - s..hit1]
        .iter()
        .chain(wet_r[hit1 - s..hit1].iter())
        .fold(0.0f32, |a, &b| a.max(b.abs()));
    let hit_peak = 0.5f32;
    println!(
        "\nswell peak {:.1} dBFS vs hit peak {:.1} dBFS",
        db(swell_peak),
        db(hit_peak)
    );
    let swell2 = rms(&wet_l[hit2 - ms(200.0)..hit2]);
    let fake3 = rms(&wet_l[hit2 + ms(370.0) - ms(200.0)..hit2 + ms(370.0)]);
    println!("second phrase swell {:.1} dB, before its 2nd stab {:.1} dB (should be quiet)", db(swell2), db(fake3));

    let odd_diff = out_l
        .iter()
        .zip(odd_l.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    println!("max diff block 512 vs block 97: {odd_diff:e}");

    // Stereo decorrelation of the swell
    let (mut lr, mut ll, mut rr) = (0.0f64, 0.0f64, 0.0f64);
    for i in hit1 - s..hit1 {
        lr += (wet_l[i] * wet_r[i]) as f64;
        ll += (wet_l[i] * wet_l[i]) as f64;
        rr += (wet_r[i] * wet_r[i]) as f64;
    }
    println!("swell L/R correlation: {:.2}", lr / (ll * rr).sqrt());

    // Biggest sample-to-sample jump in the wet signal (click detector)
    let max_jump = wet_l
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    println!("max wet sample-to-sample jump: {max_jump:.3}");

    write_wav(&format!("{out_dir}/input.wav"), &input, &input);
    write_wav(&format!("{out_dir}/output.wav"), &out_l, &out_r);
    write_wav(&format!("{out_dir}/wet_only.wav"), &wet_l, &wet_r);

    // Capture: run like the plugin does (publish after every block) and compare with the output
    {
        let slot = CaptureSlot::default();
        slot.ensure_capacity(SR);
        let mut engine = Engine::new(SR);
        for g in engine.install_ir(Box::new(SwellIr::new(spec))) {
            drop(g);
        }
        let settings = EngineSettings { dry: false, ..base };
        let mut l = input.clone();
        let mut r = input.clone();
        let mut versions = Vec::with_capacity(16);
        let mut pos = 0;
        ALLOCS.store(0, Ordering::Relaxed);
        COUNTING.store(true, Ordering::Relaxed);
        while pos < l.len() {
            let n = 512.min(l.len() - pos);
            engine.process(&mut l[pos..pos + n], &mut r[pos..pos + n], &settings);
            if engine.publish_capture(&slot, 150.0, 3) {
                versions.push(pos + n);
            }
            pos += n;
        }
        COUNTING.store(false, Ordering::Relaxed);
        println!("\ncapture: published {} swells at output samples {:?} (hits at {hit1}, {hit2}); allocations {}",
            versions.len(), versions, ALLOCS.load(Ordering::Relaxed));
        let d = slot.data.lock();
        let len = d.len;
        // The trigger fires on the first sample over the threshold, a few samples into the stab
        let trig = (onset2..).find(|&i| input[i].abs() >= 10f32.powf(-30.0 / 20.0)).unwrap();
        let hit = trig + latency;
        let exact = (0..len).all(|k| d.left[k] == l[hit - len + k] && d.right[k] == r[hit - len + k]);
        let after_ok = l[hit..hit + ms(1.0)].iter().any(|v| v.abs() > 0.0);
        println!("trigger {} samples into the stab; output right after the hit is still the (fading) swell: {after_ok}", trig - onset2);
        let peak = d.left[..len].iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        println!("captured length {len} samples (swell length {s}), identical to output before hit 2: {exact}, peak {:.1} dBFS, last 5 ms rms {:.1} dB",
            db(peak), db(rms(&d.left[len - ms(5.0)..len])));
        // Write it through the export path too
        let path = reverse_swell::export::write_swell(&d.left[..len], &d.right[..len], d.sample_rate, "1-2 bar", 150.0).unwrap();
        let reader = hound::WavReader::open(&path).unwrap();
        println!("exported {} ({} frames, {} Hz, {} bit {:?})", path.display(), reader.duration(), reader.spec().sample_rate, reader.spec().bits_per_sample, reader.spec().sample_format);
        std::fs::copy(&path, format!("{out_dir}/captured_swell.wav")).unwrap();
    }

    // CPU: 60 s of constant triggering with the longest swell
    let long = IrSpec { length: length_samples(4000.0, SR), ..spec };
    let mut engine = Engine::new(SR);
    for g in engine.install_ir(Box::new(SwellIr::new(long))) {
        drop(g);
    }
    let mut busy: Vec<f32> = Vec::new();
    while busy.len() < (60.0 * SR) as usize {
        busy.extend(stab(0.3, &a_minor, 0.5));
        busy.extend(vec![0.0; (0.3 * SR) as usize]);
    }
    let mut bl = busy.clone();
    let mut br = busy;
    let settings = EngineSettings { gap_ms: 100.0, ..base };
    let t0 = std::time::Instant::now();
    for (l, r) in bl.chunks_mut(256).zip(br.chunks_mut(256)) {
        engine.process(l, r, &settings);
    }
    let el = t0.elapsed().as_secs_f32();
    println!(
        "\nCPU worst case (4 s swell, constant hits): 60 s of audio in {el:.2} s = {:.1}% of one core",
        el / 60.0 * 100.0
    );
    let t0 = std::time::Instant::now();
    let _ = SwellIr::new(long);
    println!("building a 4 s IR takes {:.0} ms (background thread)", t0.elapsed().as_secs_f32() * 1000.0);
}
