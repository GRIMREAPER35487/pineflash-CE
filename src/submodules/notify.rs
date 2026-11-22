//! Toast notifications drawn in PineFlash CE's own style (replaces egui-notify).
//!
//! Same call shape as before: `toasts.info("Title\nOptional detail").set_duration(..).set_closable(..)`.
//! The first line of the caption is the title; anything after it is shown as muted detail text.

use std::time::Duration;

use eframe::egui::{
    self, epaint::Shadow, Align, Align2, Color32, Context, Frame, Id, Layout, Margin, Order, RichText,
    Rounding, Sense, Stroke, Vec2,
};

use super::main_panel::{AMBER, GREEN, RED};

const BLUE: Color32 = Color32::from_rgb(56, 132, 220);
const WIDTH: f32 = 320.;
const MAX_VISIBLE: usize = 4;
const FADE: f32 = 0.25;

#[derive(Clone, Copy, PartialEq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
}

impl Level {
    fn color(self) -> Color32 {
        match self {
            Level::Info => BLUE,
            Level::Success => GREEN,
            Level::Warning => AMBER,
            Level::Error => RED,
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Level::Info => "\u{f05a}",    // circle-info
            Level::Success => "\u{f058}", // circle-check
            Level::Warning => "\u{f071}", // triangle-exclamation
            Level::Error => "\u{f057}",   // circle-xmark
        }
    }
}

pub struct Toast {
    id: u64,
    level: Level,
    title: String,
    body: Option<String>,
    /// None = stays until dismissed (shown with a spinner while it is an in-progress info toast).
    duration: Option<f32>,
    closable: bool,
    elapsed: f32,
    closing: Option<f32>,
}

impl Toast {
    pub fn set_duration(&mut self, d: Option<Duration>) -> &mut Self {
        self.duration = d.map(|d| d.as_secs_f32());
        self
    }
    pub fn set_closable(&mut self, closable: bool) -> &mut Self {
        // Errors can always be dismissed: nobody should have to wait one out.
        self.closable = closable || self.level == Level::Error;
        self
    }
    fn in_progress(&self) -> bool {
        self.duration.is_none() && self.level == Level::Info
    }
}

#[derive(Default)]
pub struct Toasts {
    toasts: Vec<Toast>,
    next_id: u64,
}

impl Toasts {
    fn add(&mut self, level: Level, caption: impl Into<String>) -> &mut Toast {
        let caption: String = caption.into();
        let mut lines = caption.splitn(2, '\n');
        let title = lines.next().unwrap_or_default().trim().to_string();
        let body = lines
            .next()
            .map(|b| b.lines().map(str::trim).collect::<Vec<_>>().join(" "))
            .filter(|b| !b.is_empty());
        self.next_id += 1;
        self.toasts.push(Toast {
            id: self.next_id,
            level,
            title,
            body,
            duration: Some(4.),
            closable: level == Level::Error,
            elapsed: 0.,
            closing: None,
        });
        self.toasts.last_mut().expect("just pushed")
    }

    pub fn info(&mut self, caption: impl Into<String>) -> &mut Toast {
        self.add(Level::Info, caption)
    }
    pub fn success(&mut self, caption: impl Into<String>) -> &mut Toast {
        self.add(Level::Success, caption)
    }
    #[allow(dead_code)]
    pub fn warning(&mut self, caption: impl Into<String>) -> &mut Toast {
        self.add(Level::Warning, caption)
    }
    pub fn error(&mut self, caption: impl Into<String>) -> &mut Toast {
        self.add(Level::Error, caption)
    }

    pub fn dismiss_all_toasts(&mut self) {
        self.toasts.clear();
    }

    pub fn show(&mut self, ctx: &Context) {
        if self.toasts.is_empty() {
            return;
        }
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let p = crate::palette(ctx.style().visuals.dark_mode);

        // Age toasts, start fading the expired ones, drop the faded ones.
        for t in self.toasts.iter_mut() {
            if let Some(c) = t.closing.as_mut() {
                *c += dt;
            } else if let Some(d) = t.duration {
                if t.elapsed >= d {
                    t.closing = Some(0.);
                }
            }
        }
        self.toasts.retain(|t| t.closing.map_or(true, |c| c < FADE));
        let overflow = self.toasts.len().saturating_sub(MAX_VISIBLE);

        let mut dismiss = Vec::new();
        let mut hovered_ids = Vec::new();

        egui::Area::new(Id::new("pineflash_toasts"))
            // Bottom-right, clear of the form controls; stacks upwards like Windows notifications.
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-16., -16.))
            .order(Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 8.;
                // Newest at the bottom, nearest the corner.
                let card = if ctx.style().visuals.dark_mode { p.field_hover } else { p.page };
                for t in self.toasts.iter().skip(overflow) {
                    let opacity = t.closing.map_or(1., |c| 1. - c / FADE);
                    let accent = t.level.color();
                    let frame = Frame::none()
                        .fill(card.gamma_multiply(opacity))
                        .stroke(Stroke::new(1.0_f32, p.border_strong.gamma_multiply(opacity)))
                        .rounding(Rounding::same(8.))
                        .inner_margin(Margin { left: 16., right: 12., top: 11., bottom: 12. })
                        .shadow(Shadow {
                            offset: Vec2::new(0., 4.),
                            blur: 14.,
                            spread: 0.,
                            color: Color32::from_black_alpha((60. * opacity) as u8),
                        });

                    let resp = frame.show(ui, |ui| {
                        ui.set_width(WIDTH - 28.);
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 10.;
                            if t.in_progress() {
                                ui.add(egui::Spinner::new().size(15.).color(accent.gamma_multiply(opacity)));
                            } else {
                                ui.label(RichText::new(t.level.icon()).size(15.).color(accent.gamma_multiply(opacity)));
                            }
                            let text_w = ui.available_width() - if t.closable { 22. } else { 0. };
                            ui.allocate_ui_with_layout(Vec2::new(text_w, 0.), Layout::top_down(Align::Min), |ui| {
                                ui.spacing_mut().item_spacing.y = 3.;
                                ui.label(RichText::new(&t.title).strong().color(p.text.gamma_multiply(opacity)));
                                if let Some(body) = &t.body {
                                    ui.label(RichText::new(body).size(12.5).color(p.muted.gamma_multiply(opacity)));
                                }
                            });
                            if t.closable {
                                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                                    let x = ui.add(
                                        egui::Label::new(RichText::new("\u{f00d}").size(12.).color(p.muted))
                                            .sense(Sense::click()),
                                    );
                                    let x = x.on_hover_cursor(egui::CursorIcon::PointingHand);
                                    if x.clicked() {
                                        dismiss.push(t.id);
                                    }
                                });
                            }
                        });
                    });

                    let rect = resp.response.rect;
                    let painter = ui.painter();
                    // Accent strip down the left edge.
                    painter.rect_filled(
                        egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + 4., rect.max.y)),
                        Rounding { nw: 8., sw: 8., ne: 0., se: 0. },
                        accent.gamma_multiply(opacity),
                    );
                    // Time-left line along the bottom.
                    if let (Some(d), None) = (t.duration, t.closing) {
                        let left = (1. - t.elapsed / d).clamp(0., 1.);
                        let y = rect.max.y - 2.5;
                        let x0 = rect.min.x + 10.;
                        let x1 = rect.max.x - 10.;
                        painter.line_segment(
                            [egui::pos2(x0, y), egui::pos2(x0 + (x1 - x0) * left, y)],
                            Stroke::new(2.0_f32, accent.gamma_multiply(0.45)),
                        );
                    }
                    if ui.rect_contains_pointer(rect) {
                        hovered_ids.push(t.id);
                    }
                }
            });

        for t in self.toasts.iter_mut() {
            if dismiss.contains(&t.id) {
                t.closing = Some(0.);
            } else if t.closing.is_none() && !hovered_ids.contains(&t.id) {
                t.elapsed += dt; // hovering pauses the countdown
            }
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }
}
