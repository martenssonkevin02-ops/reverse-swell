use crate::dsp::CaptureSlot;
use crate::export;
use crate::{SharedState, SwellParams};
use nih_plug::prelude::*;
use nih_plug_egui::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    Ui, Vec2,
};
use nih_plug_egui::create_egui_editor;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

const BG: Color32 = Color32::from_rgb(14, 14, 17);
const PANEL: Color32 = Color32::from_rgb(24, 24, 29);
const PANEL_EDGE: Color32 = Color32::from_rgb(40, 40, 48);
const TEXT: Color32 = Color32::from_rgb(225, 225, 232);
const TEXT_DIM: Color32 = Color32::from_rgb(130, 130, 145);
const ACCENT: Color32 = Color32::from_rgb(255, 46, 84);
const ACCENT_DARK: Color32 = Color32::from_rgb(90, 18, 40);

const KNOB_W: f32 = 66.0;
const KNOB_H: f32 = 92.0;

struct EditorState {
    last_triggers: u32,
    flash_until: f64,
    capture: CaptureView,
}

/// The GUI's own copy of the newest captured swell.
#[derive(Default)]
struct CaptureView {
    version: u32,
    left: Vec<f32>,
    right: Vec<f32>,
    sample_rate: f32,
    bpm: f32,
    note_index: i32,
    /// Min/max per pixel column, normalised.
    peaks: Vec<(f32, f32)>,
    /// The WAV file written for this capture, if any.
    saved: Option<PathBuf>,
    message: String,
    message_until: f64,
}

impl CaptureView {
    fn update(&mut self, slot: &CaptureSlot) {
        let version = slot.version.load(Ordering::Acquire);
        if version == self.version {
            return;
        }
        {
            let d = slot.data.lock();
            self.left.clear();
            self.right.clear();
            self.left.extend_from_slice(&d.left[..d.len]);
            self.right.extend_from_slice(&d.right[..d.len]);
            self.sample_rate = d.sample_rate;
            self.bpm = d.bpm;
            self.note_index = d.note_index;
        }
        self.version = version;
        self.saved = None;

        const COLUMNS: usize = 360;
        let len = self.left.len();
        let peak = self
            .left
            .iter()
            .chain(self.right.iter())
            .fold(0.0f32, |a, &b| a.max(b.abs()))
            .max(1e-9);
        self.peaks.clear();
        for c in 0..COLUMNS {
            let from = c * len / COLUMNS;
            let to = ((c + 1) * len / COLUMNS).max(from + 1).min(len);
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for i in from..to {
                let v = 0.5 * (self.left[i] + self.right[i]);
                lo = lo.min(v);
                hi = hi.max(v);
            }
            self.peaks.push((lo / peak, hi / peak));
        }
    }

    fn has_swell(&self) -> bool {
        !self.left.is_empty()
    }

    fn label(&self) -> String {
        export::length_label(self.note_index, self.left.len(), self.sample_rate.max(1.0))
    }

    /// Writes the WAV the first time it's needed and returns its path.
    fn ensure_saved(&mut self) -> Result<PathBuf, String> {
        if let Some(path) = &self.saved {
            if path.exists() {
                return Ok(path.clone());
            }
        }
        let path = export::write_swell(
            &self.left,
            &self.right,
            self.sample_rate,
            &self.label(),
            self.bpm,
        )?;
        self.saved = Some(path.clone());
        Ok(path)
    }

    fn say(&mut self, text: impl Into<String>, now: f64) {
        self.message = text.into();
        self.message_until = now + 6.0;
    }
}

pub fn create(
    params: Arc<SwellParams>,
    shared: Arc<SharedState>,
    capture: Arc<CaptureSlot>,
) -> Option<Box<dyn Editor>> {
    let egui_state = params.editor_state.clone();
    create_egui_editor(
        egui_state,
        EditorState {
            last_triggers: shared.triggers.load(Ordering::Relaxed),
            flash_until: 0.0,
            capture: CaptureView::default(),
        },
        |ctx, _| {
            let mut style = (*ctx.style()).clone();
            style.visuals = egui::Visuals::dark();
            style.visuals.panel_fill = BG;
            ctx.set_style(style);
        },
        move |ctx, setter, state| {
            let now = ctx.input(|i| i.time);
            let triggers = shared.triggers.load(Ordering::Relaxed);
            if triggers != state.last_triggers {
                state.last_triggers = triggers;
                state.flash_until = now + 0.18;
            }
            let flashing = now < state.flash_until;
            state.capture.update(&capture);

            egui::CentralPanel::default()
                .frame(egui::Frame::default().fill(BG).inner_margin(14))
                .show(ctx, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(10.0, 10.0);
                    header(ui, flashing);
                    swell_display(ui, &params, &shared);

                    ui.horizontal(|ui| {
                        panel(ui, "SWELL", 3, |ui| {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                toggle(ui, &params.sync, setter, "SYNC");
                            });
                        }, |ui| {
                            if params.sync.value() {
                                knob(ui, &params.length_note, setter, "LENGTH");
                            } else {
                                knob(ui, &params.length_ms, setter, "LENGTH");
                            }
                            knob(ui, &params.curve, setter, "CURVE");
                            knob(ui, &params.source_ms, setter, "SOURCE");
                        });
                        panel(ui, "TONE", 4, |_| {}, |ui| {
                            knob(ui, &params.low_cut, setter, "LOW CUT");
                            knob(ui, &params.high_cut, setter, "HIGH CUT");
                            knob(ui, &params.darkness, setter, "DARK");
                            knob(ui, &params.width, setter, "WIDTH");
                        });
                    });

                    ui.horizontal(|ui| {
                        panel(ui, "TRIGGER", 2, |_| {}, |ui| {
                            knob(ui, &params.threshold, setter, "THRESHOLD");
                            knob(ui, &params.gap_ms, setter, "GAP");
                        });
                        panel(ui, "OUTPUT", 5, |_| {}, |ui| {
                            knob(ui, &params.wet, setter, "WET");
                            ui.add_space(6.0);
                            ui.vertical(|ui| {
                                ui.add_space(18.0);
                                toggle(ui, &params.cut_at_hit, setter, "CUT AT HIT");
                                ui.add_space(4.0);
                                toggle(ui, &params.dry, setter, "DRY");
                            });
                        });
                    });

                    capture_strip(ui, &mut state.capture, now);
                });

            ctx.request_repaint();
        },
    )
}

fn header(ui: &mut Ui, flashing: bool) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 34.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.text(
        rect.left_center() + Vec2::new(0.0, -4.0),
        Align2::LEFT_CENTER,
        "REVERSE SWELL",
        FontId::proportional(22.0),
        TEXT,
    );
    p.text(
        rect.left_bottom() + Vec2::new(1.0, 0.0),
        Align2::LEFT_BOTTOM,
        "SECOND GENERATION",
        FontId::proportional(10.0),
        TEXT_DIM,
    );

    let led = Pos2::new(rect.right() - 8.0, rect.center().y);
    let color = if flashing { ACCENT } else { ACCENT_DARK };
    if flashing {
        p.circle_filled(led, 11.0, ACCENT.gamma_multiply(0.25));
    }
    p.circle_filled(led, 6.0, color);
    p.text(
        led - Vec2::new(16.0, 0.0),
        Align2::RIGHT_CENTER,
        "HIT",
        FontId::proportional(11.0),
        if flashing { TEXT } else { TEXT_DIM },
    );
}

fn swell_display(ui: &mut Ui, params: &SwellParams, shared: &SharedState) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 120.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(6), PANEL);
    p.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0_f32, PANEL_EDGE),
        StrokeKind::Inside,
    );

    let inner = rect.shrink2(Vec2::new(14.0, 12.0));
    let plot = Rect::from_min_max(
        Pos2::new(inner.left(), inner.top() + 14.0),
        Pos2::new(inner.left() + inner.width() * 0.78, inner.bottom()),
    );
    let hit_x = plot.right();

    // Grid
    for i in 1..4 {
        let x = plot.left() + plot.width() * i as f32 / 4.0;
        p.line_segment(
            [Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())],
            Stroke::new(1.0_f32, Color32::from_rgb(34, 34, 41)),
        );
    }

    // Swell envelope: the reversed reverb tail rises as t^curve into the hit
    let curve = params.curve.value();
    let darkness = params.darkness.value();
    let steps = 140;
    let mut line = Vec::with_capacity(steps + 1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let amp = t.powf(curve);
        let x = plot.left() + plot.width() * t;
        let y = plot.bottom() - amp * plot.height();
        // Darker at the start of the swell, brightening into the hit
        let brightness = 1.0 - darkness * (1.0 - t).powf(0.6) * 0.85;
        let fill = ACCENT.gamma_multiply(0.10 + 0.30 * brightness * t.max(0.15));
        p.line_segment(
            [Pos2::new(x, plot.bottom()), Pos2::new(x, y)],
            Stroke::new(plot.width() / steps as f32 + 0.6, fill),
        );
        line.push(Pos2::new(x, y));
    }
    p.add(Shape::line(line, Stroke::new(2.0_f32, ACCENT)));

    // The hit itself (dry signal) after the swell
    p.line_segment(
        [Pos2::new(hit_x, inner.top() + 6.0), Pos2::new(hit_x, plot.bottom())],
        Stroke::new(2.0_f32, TEXT),
    );
    let hit_block = Rect::from_min_max(
        Pos2::new(hit_x + 3.0, plot.top() + 6.0),
        Pos2::new(inner.right(), plot.bottom()),
    );
    let mut bars = Vec::new();
    let n = 24;
    for i in 0..n {
        let t = i as f32 / n as f32;
        let h = (1.0 - t).powf(1.5) * hit_block.height();
        let x = hit_block.left() + hit_block.width() * t;
        bars.push(Shape::line_segment(
            [
                Pos2::new(x, hit_block.center().y - h / 2.0),
                Pos2::new(x, hit_block.center().y + h / 2.0),
            ],
            Stroke::new(2.0_f32, Color32::from_rgb(70, 70, 82)),
        ));
    }
    p.extend(bars);
    p.text(
        Pos2::new(hit_x + 6.0, inner.top()),
        Align2::LEFT_TOP,
        "HIT",
        FontId::proportional(10.0),
        TEXT_DIM,
    );

    // Labels
    let bpm = shared.bpm.load(Ordering::Relaxed);
    let bpm = if bpm > 0.0 { bpm } else { crate::DEFAULT_BPM };
    let swell_ms = params.swell_ms(bpm);
    let length_text = if params.sync.value() {
        format!(
            "{}  ·  {:.0} ms @ {:.0} BPM",
            params.length_note,
            swell_ms,
            bpm
        )
    } else {
        format!("{swell_ms:.0} ms")
    };
    p.text(
        inner.left_top(),
        Align2::LEFT_TOP,
        length_text,
        FontId::proportional(11.0),
        TEXT,
    );
    let latency = shared.latency_ms.load(Ordering::Relaxed);
    if latency > 0.0 {
        p.text(
            Pos2::new(hit_x - 6.0, inner.top()),
            Align2::RIGHT_TOP,
            format!("latency {latency:.0} ms"),
            FontId::proportional(10.0),
            TEXT_DIM,
        );
    }
}

/// A titled panel holding a row of knobs. `extra` draws next to the title.
fn panel(
    ui: &mut Ui,
    title: &str,
    knobs: usize,
    extra: impl FnOnce(&mut Ui),
    content: impl FnOnce(&mut Ui),
) {
    egui::Frame::default()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, PANEL_EDGE))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_min_width(knobs as f32 * (KNOB_W + 6.0));
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                ui.horizontal(|ui| {
                    ui.set_min_height(20.0);
                    ui.label(
                        egui::RichText::new(title)
                            .color(TEXT_DIM)
                            .size(11.0)
                            .strong(),
                    );
                    ui.add_space(4.0);
                    extra(ui);
                });
                ui.horizontal(content);
            });
        });
}

/// Rotary knob. Drag up/down to change, hold Shift for fine control, double-click to reset.
fn knob<P: Param>(ui: &mut Ui, param: &P, setter: &ParamSetter, label: &str) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(KNOB_W, KNOB_H), Sense::click_and_drag());
    let id = response.id;

    if response.double_clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, param.default_plain_value());
        setter.end_set_parameter(param);
    } else {
        if response.drag_started() {
            setter.begin_set_parameter(param);
            ui.data_mut(|d| d.insert_temp(id, param.unmodulated_normalized_value()));
        }
        if response.dragged() {
            let fine = ui.input(|i| i.modifiers.shift);
            let speed = if fine { 0.0008 } else { 0.005 };
            let delta = -response.drag_delta().y * speed;
            let value = ui
                .data(|d| d.get_temp::<f32>(id))
                .unwrap_or_else(|| param.unmodulated_normalized_value());
            let value = (value + delta).clamp(0.0, 1.0);
            ui.data_mut(|d| d.insert_temp(id, value));
            setter.set_parameter_normalized(param, value);
        }
        if response.drag_stopped() {
            setter.end_set_parameter(param);
        }
    }

    let p = ui.painter_at(rect.expand(2.0));
    p.text(
        Pos2::new(rect.center().x, rect.top()),
        Align2::CENTER_TOP,
        label,
        FontId::proportional(10.0),
        TEXT_DIM,
    );

    let center = Pos2::new(rect.center().x, rect.top() + 44.0);
    let radius = 22.0;
    let start = 135f32.to_radians();
    let sweep = 270f32.to_radians();
    let value = param.unmodulated_normalized_value();
    let arc = |from: f32, to: f32, r: f32| -> Vec<Pos2> {
        let steps = 48;
        (0..=steps)
            .map(|i| {
                let a = from + (to - from) * i as f32 / steps as f32;
                center + Vec2::new(a.cos(), a.sin()) * r
            })
            .collect()
    };

    let hovered = response.hovered() || response.dragged();
    p.circle_filled(center, radius - 5.0, Color32::from_rgb(34, 34, 41));
    p.add(Shape::line(
        arc(start, start + sweep, radius),
        Stroke::new(4.0_f32, Color32::from_rgb(44, 44, 54)),
    ));
    if value > 0.001 {
        p.add(Shape::line(
            arc(start, start + sweep * value, radius),
            Stroke::new(4.0_f32, if hovered { ACCENT } else { ACCENT.gamma_multiply(0.85) }),
        ));
    }
    let angle = start + sweep * value;
    let dir = Vec2::new(angle.cos(), angle.sin());
    p.line_segment(
        [center + dir * 5.0, center + dir * (radius - 7.0)],
        Stroke::new(2.0_f32, TEXT),
    );

    p.text(
        Pos2::new(rect.center().x, rect.bottom()),
        Align2::CENTER_BOTTOM,
        param.to_string(),
        FontId::proportional(11.0),
        if hovered { TEXT } else { TEXT_DIM },
    );

    response.on_hover_text("Drag to change · Shift for fine · Double-click to reset");
}

/// Pill-shaped on/off button for a bool parameter.
fn toggle(ui: &mut Ui, param: &BoolParam, setter: &ParamSetter, label: &str) {
    let font = FontId::proportional(10.0);
    let text_w = ui.fonts(|f| {
        f.layout_no_wrap(label.to_string(), font.clone(), TEXT)
            .size()
            .x
    });
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(text_w + 30.0, 20.0), Sense::click());
    if response.clicked() {
        setter.begin_set_parameter(param);
        setter.set_parameter(param, !param.value());
        setter.end_set_parameter(param);
    }
    let on = param.value();
    let p = ui.painter_at(rect);
    p.rect_filled(
        rect,
        CornerRadius::same(10),
        if on { ACCENT_DARK } else { Color32::from_rgb(34, 34, 41) },
    );
    p.rect_stroke(
        rect,
        CornerRadius::same(10),
        Stroke::new(1.0_f32, if on { ACCENT } else { PANEL_EDGE }),
        StrokeKind::Inside,
    );
    p.circle_filled(
        Pos2::new(rect.left() + 11.0, rect.center().y),
        3.5,
        if on { ACCENT } else { TEXT_DIM },
    );
    p.text(
        Pos2::new(rect.left() + 20.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        if on { TEXT } else { TEXT_DIM },
    );
}

/// Small pill-shaped push button.
fn action_button(ui: &mut Ui, label: &str, enabled: bool) -> bool {
    let font = FontId::proportional(10.0);
    let text_w = ui.fonts(|f| f.layout_no_wrap(label.to_string(), font.clone(), TEXT).size().x);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(text_w + 22.0, 22.0), Sense::click());
    let hovered = enabled && response.hovered();
    let p = ui.painter_at(rect);
    p.rect_filled(
        rect,
        CornerRadius::same(11),
        if hovered { ACCENT_DARK } else { Color32::from_rgb(34, 34, 41) },
    );
    p.rect_stroke(
        rect,
        CornerRadius::same(11),
        Stroke::new(1.0_f32, if hovered { ACCENT } else { PANEL_EDGE }),
        StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font,
        if enabled { TEXT } else { TEXT_DIM.gamma_multiply(0.6) },
    );
    enabled && response.clicked()
}

/// The newest captured swell, ready to drag into the DAW.
fn capture_strip(ui: &mut Ui, view: &mut CaptureView, now: f64) {
    egui::Frame::default()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, PANEL_EDGE))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 6.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("LAST SWELL").color(TEXT_DIM).size(11.0).strong());
                if view.has_swell() {
                    let ms = view.left.len() as f32 * 1000.0 / view.sample_rate.max(1.0);
                    let label = view.label().replace('-', "/");
                    let info = if view.note_index >= 0 {
                        format!("{label}  ·  {ms:.0} ms  ·  {:.0} BPM", view.bpm)
                    } else {
                        format!("{ms:.0} ms")
                    };
                    ui.label(egui::RichText::new(info).color(TEXT).size(11.0));
                }
            });

            ui.horizontal(|ui| {
                let wave_w = (ui.available_width() - 132.0).max(100.0);
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(wave_w, 58.0), Sense::click_and_drag());
                draw_capture(ui, rect, view, response.hovered() || response.dragged());

                if view.has_swell() {
                    if response.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    if response.drag_started() {
                        match view.ensure_saved() {
                            Ok(path) => {
                                if let Err(err) = export::start_drag(&path) {
                                    let name = path
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    view.say(format!("Saved \"{name}\" ({err})"), now);
                                }
                            }
                            Err(err) => view.say(err, now),
                        }
                    }
                }

                ui.vertical(|ui| {
                    ui.add_space(4.0);
                    let reveal_label = if cfg!(target_os = "macos") {
                        "SHOW IN FINDER"
                    } else {
                        "SHOW FILE"
                    };
                    if action_button(ui, reveal_label, view.has_swell()) {
                        match view.ensure_saved() {
                            Ok(path) => export::reveal(&path),
                            Err(err) => view.say(err, now),
                        }
                    }
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("Saved in Music >\nReverse Swell")
                            .color(TEXT_DIM)
                            .size(9.5),
                    );
                });
            });

            if now < view.message_until && !view.message.is_empty() {
                ui.label(egui::RichText::new(&view.message).color(ACCENT).size(10.0));
            }
        });
}

fn draw_capture(ui: &Ui, rect: Rect, view: &CaptureView, highlighted: bool) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, CornerRadius::same(4), Color32::from_rgb(18, 18, 22));
    p.rect_stroke(
        rect,
        CornerRadius::same(4),
        Stroke::new(1.0_f32, if highlighted && view.has_swell() { ACCENT } else { PANEL_EDGE }),
        StrokeKind::Inside,
    );

    if !view.has_swell() {
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Play the sound on this track. Its swell shows up here, ready to drag into FL.",
            FontId::proportional(11.0),
            TEXT_DIM,
        );
        return;
    }

    let inner = rect.shrink2(Vec2::new(6.0, 5.0));
    let mid = inner.center().y;
    let half = inner.height() / 2.0;
    let n = view.peaks.len().max(1);
    let col_w = inner.width() / n as f32;
    let color = if highlighted { ACCENT } else { ACCENT.gamma_multiply(0.8) };
    let mut shapes = Vec::with_capacity(n);
    for (i, &(lo, hi)) in view.peaks.iter().enumerate() {
        let x = inner.left() + (i as f32 + 0.5) * col_w;
        let top = mid - hi.max(0.004) * half;
        let bottom = mid - lo.min(-0.004) * half;
        shapes.push(Shape::line_segment(
            [Pos2::new(x, top), Pos2::new(x, bottom)],
            Stroke::new(col_w.max(1.0), color),
        ));
    }
    p.extend(shapes);
    // The swell ends exactly at the hit
    p.line_segment(
        [Pos2::new(inner.right(), rect.top() + 3.0), Pos2::new(inner.right(), rect.bottom() - 3.0)],
        Stroke::new(1.5_f32, TEXT),
    );
    p.text(
        Pos2::new(inner.right() - 4.0, rect.top() + 4.0),
        Align2::RIGHT_TOP,
        "DRAG INTO FL",
        FontId::proportional(10.0),
        if highlighted { TEXT } else { TEXT_DIM },
    );
}
