//! Reverse Swell: a reverse reverb that swells into the first hit of a sound.

use atomic_float::AtomicF32;
use crossbeam_queue::ArrayQueue;
use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

pub mod convolver;
#[cfg(target_os = "macos")]
mod drag_mac;
pub mod dsp;
mod editor;
pub mod export;

use dsp::{CaptureSlot, Engine, EngineSettings, IrSpec, SwellIr, MAX_LENGTH_MS, MIN_LENGTH_MS};

/// Tempo used when the host doesn't report one.
pub const DEFAULT_BPM: f32 = 150.0;
/// Wait this long after the last knob movement before rebuilding the reverb.
const REBUILD_DEBOUNCE_MS: f32 = 40.0;

#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteLength {
    #[name = "1/16"]
    Sixteenth,
    #[name = "1/8"]
    Eighth,
    #[name = "1/4"]
    Quarter,
    #[name = "1/2"]
    Half,
    #[name = "1 bar"]
    Bar,
    #[name = "2 bars"]
    TwoBars,
}

impl NoteLength {
    pub fn beats(self) -> f32 {
        match self {
            NoteLength::Sixteenth => 0.25,
            NoteLength::Eighth => 0.5,
            NoteLength::Quarter => 1.0,
            NoteLength::Half => 2.0,
            NoteLength::Bar => 4.0,
            NoteLength::TwoBars => 8.0,
        }
    }
}

#[derive(Params)]
pub struct SwellParams {
    #[persist = "editor-state"]
    pub editor_state: Arc<EguiState>,

    #[id = "sync"]
    pub sync: BoolParam,
    #[id = "len_note"]
    pub length_note: EnumParam<NoteLength>,
    #[id = "len_ms"]
    pub length_ms: FloatParam,
    #[id = "curve"]
    pub curve: FloatParam,
    #[id = "source"]
    pub source_ms: FloatParam,

    #[id = "lowcut"]
    pub low_cut: FloatParam,
    #[id = "highcut"]
    pub high_cut: FloatParam,
    #[id = "dark"]
    pub darkness: FloatParam,
    #[id = "width"]
    pub width: FloatParam,

    #[id = "thresh"]
    pub threshold: FloatParam,
    #[id = "gap"]
    pub gap_ms: FloatParam,

    #[id = "wet"]
    pub wet: FloatParam,
    #[id = "cut"]
    pub cut_at_hit: BoolParam,
    #[id = "dry"]
    pub dry: BoolParam,
}

impl Default for SwellParams {
    fn default() -> Self {
        let ms_formatter = || {
            Arc::new(|v: f32| {
                if v >= 1000.0 {
                    format!("{:.2} s", v / 1000.0)
                } else {
                    format!("{v:.0} ms")
                }
            })
        };
        let hz_formatter = || {
            Arc::new(|v: f32| {
                if v >= 1000.0 {
                    format!("{:.1} kHz", v / 1000.0)
                } else {
                    format!("{v:.0} Hz")
                }
            })
        };
        // Accept "800 ms", "1.5 s", "800"
        let ms_parser = || {
            Arc::new(|s: &str| {
                let s = s.trim().to_lowercase();
                if let Some(v) = s.strip_suffix("ms") {
                    v.trim().parse::<f32>().ok()
                } else if let Some(v) = s.strip_suffix('s') {
                    v.trim().parse::<f32>().ok().map(|x| x * 1000.0)
                } else {
                    s.parse::<f32>().ok()
                }
            })
        };
        // Accept "250 Hz", "14 kHz", "14k", "250"
        let hz_parser = || {
            Arc::new(|s: &str| {
                let s = s.trim().to_lowercase();
                let s = s.strip_suffix("hz").unwrap_or(&s).trim();
                if let Some(v) = s.strip_suffix('k') {
                    v.trim().parse::<f32>().ok().map(|x| x * 1000.0)
                } else {
                    s.parse::<f32>().ok()
                }
            })
        };

        Self {
            editor_state: EguiState::from_size(600, 600),

            sync: BoolParam::new("Tempo Sync", true).non_automatable(),
            length_note: EnumParam::new("Length (Sync)", NoteLength::Half).non_automatable(),
            length_ms: FloatParam::new(
                "Length (ms)",
                800.0,
                FloatRange::Skewed {
                    min: MIN_LENGTH_MS,
                    max: MAX_LENGTH_MS,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .non_automatable()
            .with_value_to_string(ms_formatter())
            .with_string_to_value(ms_parser()),
            curve: FloatParam::new(
                "Curve",
                3.0,
                FloatRange::Skewed {
                    min: 1.0,
                    max: 8.0,
                    factor: FloatRange::skew_factor(-1.0),
                },
            )
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            source_ms: FloatParam::new(
                "Source",
                120.0,
                FloatRange::Skewed {
                    min: 20.0,
                    max: 500.0,
                    factor: FloatRange::skew_factor(-1.0),
                },
            )
            .with_value_to_string(ms_formatter())
            .with_string_to_value(ms_parser()),

            low_cut: FloatParam::new(
                "Low Cut",
                250.0,
                FloatRange::Skewed {
                    min: 20.0,
                    max: 2000.0,
                    factor: FloatRange::skew_factor(-2.0),
                },
            )
            .with_value_to_string(hz_formatter())
            .with_string_to_value(hz_parser()),
            high_cut: FloatParam::new(
                "High Cut",
                14_000.0,
                FloatRange::Skewed {
                    min: 1000.0,
                    max: 20_000.0,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .with_value_to_string(hz_formatter())
            .with_string_to_value(hz_parser()),
            darkness: FloatParam::new("Darkness", 0.4, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit("%")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            width: FloatParam::new("Width", 1.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit("%")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),

            threshold: FloatParam::new(
                "Threshold",
                -30.0,
                FloatRange::Linear {
                    min: -60.0,
                    max: 0.0,
                },
            )
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_rounded(1)),
            gap_ms: FloatParam::new(
                "Retrigger Gap",
                250.0,
                FloatRange::Skewed {
                    min: 20.0,
                    max: 2000.0,
                    factor: FloatRange::skew_factor(-1.5),
                },
            )
            .with_value_to_string(ms_formatter())
            .with_string_to_value(ms_parser()),

            wet: FloatParam::new(
                "Wet",
                util::db_to_gain(0.0),
                FloatRange::Skewed {
                    min: util::db_to_gain(-36.0),
                    max: util::db_to_gain(12.0),
                    factor: FloatRange::gain_skew_factor(-36.0, 12.0),
                },
            )
            .with_smoother(SmoothingStyle::Logarithmic(30.0))
            .with_unit(" dB")
            .with_value_to_string(formatters::v2s_f32_gain_to_db(1))
            .with_string_to_value(formatters::s2v_f32_gain_to_db()),
            cut_at_hit: BoolParam::new("Cut At Hit", true),
            dry: BoolParam::new("Dry", true),
        }
    }
}

impl SwellParams {
    /// Swell length in ms for the given tempo.
    pub fn swell_ms(&self, bpm: f32) -> f32 {
        let ms = if self.sync.value() {
            self.length_note.value().beats() * 60_000.0 / bpm.max(20.0)
        } else {
            self.length_ms.value()
        };
        ms.clamp(MIN_LENGTH_MS, MAX_LENGTH_MS)
    }

    fn ir_spec(&self, sample_rate: f32, bpm: f32) -> IrSpec {
        IrSpec {
            sample_rate,
            length: dsp::length_samples(self.swell_ms(bpm), sample_rate),
            curve: self.curve.value(),
            darkness: self.darkness.value(),
            low_cut_hz: self.low_cut.value(),
            high_cut_hz: self.high_cut.value(),
            width: self.width.value(),
        }
    }
}

/// Values the GUI reads to show what the audio side is doing.
pub struct SharedState {
    pub triggers: AtomicU32,
    pub latency_ms: AtomicF32,
    pub bpm: AtomicF32,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            triggers: AtomicU32::new(0),
            latency_ms: AtomicF32::new(0.0),
            bpm: AtomicF32::new(DEFAULT_BPM),
        }
    }
}

pub enum Task {
    Build(IrSpec),
    Collect,
}

pub struct ReverseSwell {
    params: Arc<SwellParams>,
    shared: Arc<SharedState>,
    /// The newest swell, for dragging out of the GUI.
    capture: Arc<CaptureSlot>,
    engine: Option<Engine>,
    /// Finished IRs coming back from the background thread.
    ready: Arc<ArrayQueue<Box<SwellIr>>>,
    /// Old IRs going to the background thread to be freed.
    garbage: Arc<ArrayQueue<Box<SwellIr>>>,
    requested: Option<IrSpec>,
    pending: Option<IrSpec>,
    pending_age: usize,
    bpm: f32,
}

impl Default for ReverseSwell {
    fn default() -> Self {
        Self {
            params: Arc::new(SwellParams::default()),
            shared: Arc::new(SharedState::default()),
            capture: Arc::new(CaptureSlot::default()),
            engine: None,
            ready: Arc::new(ArrayQueue::new(4)),
            garbage: Arc::new(ArrayQueue::new(16)),
            requested: None,
            pending: None,
            pending_age: 0,
            bpm: DEFAULT_BPM,
        }
    }
}

impl ReverseSwell {
    fn dispose(&self, ir: Option<Box<SwellIr>>) -> bool {
        match ir {
            Some(ir) => {
                // If the queue is somehow full we have no choice but to free it here
                let _ = self.garbage.push(ir);
                true
            }
            None => false,
        }
    }
}

impl Plugin for ReverseSwell {
    const NAME: &'static str = "Reverse Swell";
    const VENDOR: &'static str = "Second Generation";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = Task;

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn task_executor(&mut self) -> TaskExecutor<Self> {
        let ready = self.ready.clone();
        let garbage = self.garbage.clone();
        Box::new(move |task| {
            while let Some(old) = garbage.pop() {
                drop(old);
            }
            if let Task::Build(spec) = task {
                let ir = Box::new(SwellIr::new(spec));
                // If the audio thread hasn't picked up an older result yet, replace it
                drop(ready.force_push(ir));
            }
        })
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        editor::create(
            self.params.clone(),
            self.shared.clone(),
            self.capture.clone(),
        )
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        context: &mut impl InitContext<Self>,
    ) -> bool {
        let sample_rate = buffer_config.sample_rate;
        self.capture.ensure_capacity(sample_rate);
        let mut engine = Engine::new(sample_rate);
        let spec = self.params.ir_spec(sample_rate, self.bpm);
        for old in engine.install_ir(Box::new(SwellIr::new(spec))) {
            drop(old);
        }
        context.set_latency_samples(engine.latency() as u32);
        self.shared.latency_ms.store(
            engine.latency() as f32 * 1000.0 / sample_rate,
            Ordering::Relaxed,
        );
        self.engine = Some(engine);
        self.requested = Some(spec);
        self.pending = None;
        // Anything still in flight was built for the old settings
        while let Some(old) = self.ready.pop() {
            drop(old);
        }
        true
    }

    fn reset(&mut self) {
        if let Some(engine) = self.engine.as_mut() {
            engine.reset();
        }
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let Some(mut engine) = self.engine.take() else {
            return ProcessStatus::Normal;
        };
        let sample_rate = engine.sample_rate();
        let num_samples = buffer.samples();

        if let Some(bpm) = context.transport().tempo {
            if bpm > 0.0 {
                self.bpm = bpm as f32;
            }
        }
        self.shared.bpm.store(self.bpm, Ordering::Relaxed);

        // Pick up freshly built reverbs
        let mut newest: Option<Box<SwellIr>> = None;
        let mut collect = false;
        while let Some(ir) = self.ready.pop() {
            collect |= self.dispose(newest.replace(ir));
        }
        if let Some(ir) = newest {
            if ir.spec.sample_rate == sample_rate {
                for old in engine.install_ir(ir) {
                    collect |= self.dispose(old);
                }
                context.set_latency_samples(engine.latency() as u32);
                self.shared.latency_ms.store(
                    engine.latency() as f32 * 1000.0 / sample_rate,
                    Ordering::Relaxed,
                );
            } else {
                collect |= self.dispose(Some(ir));
            }
        }
        collect |= self.dispose(engine.take_finished_old_ir());
        if collect {
            context.execute_background(Task::Collect);
        }

        // Ask for a rebuild once the settings have stopped moving for a moment
        let wanted = self.params.ir_spec(sample_rate, self.bpm);
        let up_to_date = self
            .requested
            .map(|r| r.roughly_equal(&wanted))
            .unwrap_or(false);
        if up_to_date {
            self.pending = None;
        } else {
            if self.pending.map(|p| p.roughly_equal(&wanted)) != Some(true) {
                self.pending = Some(wanted);
                self.pending_age = 0;
            } else {
                self.pending_age += num_samples;
            }
            let debounce = (REBUILD_DEBOUNCE_MS / 1000.0 * sample_rate) as usize;
            if self.pending_age >= debounce {
                context.execute_background(Task::Build(wanted));
                self.requested = Some(wanted);
                self.pending = None;
            }
        }

        let wet_gain = {
            // Advance the smoother by the block length and use its end value; the engine ramps
            let mut g = self.params.wet.smoothed.next();
            for _ in 1..num_samples {
                g = self.params.wet.smoothed.next();
            }
            g
        };
        let settings = EngineSettings {
            threshold_db: self.params.threshold.value(),
            source_ms: self.params.source_ms.value(),
            gap_ms: self.params.gap_ms.value(),
            cut_at_hit: self.params.cut_at_hit.value(),
            wet_gain,
            dry: self.params.dry.value(),
        };

        let channels = buffer.as_slice();
        if channels.len() >= 2 {
            let (left, right) = channels.split_at_mut(1);
            engine.process(left[0], right[0], &settings);
        }

        let note_index = if self.params.sync.value() {
            self.params.length_note.value() as i32
        } else {
            -1
        };
        engine.publish_capture(&self.capture, self.bpm, note_index);

        self.shared
            .triggers
            .store(engine.trigger_count(), Ordering::Relaxed);
        self.engine = Some(engine);
        ProcessStatus::Normal
    }
}

impl ClapPlugin for ReverseSwell {
    const CLAP_ID: &'static str = "com.second-generation.reverse-swell";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Reverse reverb that swells into the first hit of a sound");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Reverb,
    ];
}

impl Vst3Plugin for ReverseSwell {
    const VST3_CLASS_ID: [u8; 16] = *b"SecGenRevSwell01";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Reverb];
}

nih_export_clap!(ReverseSwell);
nih_export_vst3!(ReverseSwell);
