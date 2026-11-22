use eframe::egui::{self, Button, Frame, Layout, Margin, RichText, TopBottomPanel};

use super::main_panel::icon;
use crate::Flasher;

impl Flasher {
    pub fn render_header(&mut self, ctx: &egui::Context) {
        let p = crate::palette(self.saved_config.dark_mode);
        TopBottomPanel::top("top_panel")
            .frame(
                Frame::none()
                    .fill(p.page)
                    .stroke(egui::Stroke::new(1.0_f32, p.border))
                    .inner_margin(Margin::symmetric(16., 10.)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon::BOLT).size(17.));
                    ui.label(RichText::new("PineFlash CE").size(16.).strong());
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).size(12.5).weak());
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        let (glyph, tip) = if self.saved_config.dark_mode {
                            (icon::SUN, "Switch to light mode")
                        } else {
                            (icon::MOON, "Switch to dark mode")
                        };
                        let toggle = Button::new(RichText::new(glyph).size(15.).color(p.muted)).frame(false);
                        if ui.add(toggle).on_hover_text(tip).clicked() {
                            self.saved_config.dark_mode = !self.saved_config.dark_mode;
                            crate::apply_theme(ctx, self.saved_config.dark_mode);
                            self.save_settings();
                        }
                    });
                });
            });
    }
}
