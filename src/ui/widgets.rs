//! Small drawing helpers shared by the windows.

use std::collections::VecDeque;

use eframe::egui::{self, Color32, RichText, Stroke, StrokeKind};

use crate::health::{Health, Verdict};

use super::theme;

pub fn green(_ui: &egui::Ui) -> Color32 {
    theme::GREEN
}
pub fn amber(_ui: &egui::Ui) -> Color32 {
    theme::AMBER
}
pub fn red(_ui: &egui::Ui) -> Color32 {
    theme::RED
}
pub fn weak(_ui: &egui::Ui) -> Color32 {
    theme::MUTED
}

/// Series colours, in the mockup's order: cap, WebRTC target, sent, received, extra.
pub fn series(_ui: &egui::Ui, i: usize) -> Color32 {
    [
        theme::MUTED,
        theme::AMBER,
        theme::ACCENT,
        theme::GREEN,
        theme::VIOLET,
        Color32::from_rgb(0x2f, 0x7f, 0xae),
    ][i % 6]
}

pub fn health_color(_ui: &egui::Ui, h: Health) -> Option<Color32> {
    match h {
        Health::Unknown => None,
        Health::Good => Some(theme::GREEN_LINE),
        Health::Degraded => Some(theme::AMBER_LINE),
        Health::Failing => Some(theme::RED_LINE),
    }
}

/// A small filled tag such as LIVE or REPUBLISH.
pub fn badge(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), egui::FontId::proportional(9.5), fg);
    let size = galley.size() + egui::vec2(10.0, 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

pub fn live_badge(ui: &mut egui::Ui) -> egui::Response {
    badge(ui, "LIVE", theme::GREEN_TEXT, theme::GREEN_BG)
        .on_hover_text("Applies at once, without republishing.")
}

pub fn republish_badge(ui: &mut egui::Ui) -> egui::Response {
    badge(ui, "REPUBLISH", theme::AMBER_TEXT, theme::AMBER_BG).on_hover_text(
        "The stock SDK cannot change this on a published track. Applying it republishes, about 1 s without video.",
    )
}

/// A row of joined segments with one outline, as in the mockup's selectors.
pub fn segments(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .stroke(Stroke::new(1.0, theme::BORDER))
        .corner_radius(8)
        .fill(theme::BG)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal(|ui| add(ui));
        });
}

/// One segment: filled when on.
pub fn segment(
    ui: &mut egui::Ui,
    text: &str,
    on: bool,
    on_fill: Color32,
    on_text: Color32,
) -> egui::Response {
    let (fill, color) = if on {
        (on_fill, on_text)
    } else {
        (Color32::TRANSPARENT, theme::MUTED)
    };
    let rich = RichText::new(text).color(color);
    let rich = if on { rich.strong() } else { rich };
    ui.add(
        egui::Button::new(rich)
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(7)
            .min_size(egui::vec2(0.0, 30.0)),
    )
}

/// The primary action: blue outline on a blue-tinted fill.
pub fn accent_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_string())
            .color(theme::ACCENT_TEXT)
            .strong(),
    )
    .fill(theme::ACCENT_BG)
    .stroke(Stroke::new(1.0, theme::ACCENT))
    .min_size(egui::vec2(0.0, 32.0))
}

/// An action that interrupts the stream: amber outline.
pub fn warn_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_string()).color(theme::AMBER))
        .fill(theme::RAISED)
        .stroke(Stroke::new(1.0, theme::AMBER))
        .min_size(egui::vec2(0.0, 32.0))
}

/// A button that shows whether its window is open.
pub fn toggle_button(text: &str, on: bool) -> egui::Button<'static> {
    if on {
        accent_button(text)
    } else {
        egui::Button::new(text.to_string()).min_size(egui::vec2(0.0, 32.0))
    }
}

pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(4.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .size(11.5)
            .strong()
            .color(theme::MUTED),
    );
    ui.add_space(2.0);
}

/// A pipeline card whose outline shows the stage's health.
pub fn stage_card(
    ui: &mut egui::Ui,
    title: &str,
    verdict: &Verdict,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let outline = health_color(ui, verdict.health);
    let stroke = match outline {
        Some(c) => Stroke::new(2.0, c),
        None => Stroke::new(2.0, theme::BORDER),
    };
    let inner = egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(stroke)
        .corner_radius(10)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_height(132.0);
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 5.0;
            ui.label(
                RichText::new(title.to_uppercase())
                    .size(11.0)
                    .strong()
                    .color(theme::MUTED),
            );
            add(ui);
        });
    let resp = inner.response;
    if verdict.reason.is_empty() {
        resp
    } else {
        resp.on_hover_text(&verdict.reason)
    }
}

pub fn big(text: impl Into<String>) -> RichText {
    RichText::new(text.into())
        .size(17.0)
        .monospace()
        .color(theme::TEXT)
}

pub struct Series<'a> {
    pub name: &'a str,
    pub data: &'a VecDeque<f32>,
    pub color: Color32,
    pub dashed: bool,
}

/// A simple time-series line chart; the newest sample is on the right.
pub fn line_chart(ui: &mut egui::Ui, height: f32, unit: &str, series: &[Series], capacity: usize) {
    let size = egui::vec2(ui.available_width(), height);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 10.0, theme::BG);
    painter.rect_stroke(
        rect,
        10.0,
        Stroke::new(1.0, theme::BORDER),
        StrokeKind::Inside,
    );
    let max = series
        .iter()
        .flat_map(|s| s.data.iter().copied())
        .fold(0.0f32, f32::max);
    let max = nice_max(max);
    let plot = rect.shrink2(egui::vec2(12.0, 14.0));
    let grid = ui
        .visuals()
        .widgets
        .noninteractive
        .bg_stroke
        .color
        .gamma_multiply(0.6);
    for f in [0.25, 0.5, 0.75] {
        let y = plot.bottom() - plot.height() * f;
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            Stroke::new(0.5, grid),
        );
    }
    let cap = capacity.max(2) as f32 - 1.0;
    for s in series {
        if s.data.len() < 2 {
            continue;
        }
        let offset = capacity.saturating_sub(s.data.len()) as f32;
        let pts: Vec<egui::Pos2> = s
            .data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                egui::pos2(
                    plot.left() + plot.width() * (offset + i as f32) / cap,
                    plot.bottom() - plot.height() * (v / max).clamp(0.0, 1.0),
                )
            })
            .collect();
        if s.dashed {
            painter.extend(egui::Shape::dashed_line(
                &pts,
                Stroke::new(1.2, s.color),
                5.0,
                4.0,
            ));
        } else {
            painter.add(egui::Shape::line(pts, Stroke::new(1.8, s.color)));
        }
    }
    painter.text(
        rect.left_top() + egui::vec2(6.0, 3.0),
        egui::Align2::LEFT_TOP,
        format!("{} {unit}", fmt_axis(max)),
        egui::FontId::proportional(10.5),
        weak(ui),
    );
    painter.text(
        rect.left_bottom() + egui::vec2(6.0, -3.0),
        egui::Align2::LEFT_BOTTOM,
        "0",
        egui::FontId::proportional(10.5),
        weak(ui),
    );
    // Hover: values at the pointer.
    if let Some(pos) = resp.hover_pos() {
        let idx = (((pos.x - plot.left()) / plot.width()) * cap).round() as isize;
        let mut lines = Vec::new();
        for s in series {
            let offset = capacity.saturating_sub(s.data.len()) as isize;
            let i = idx - offset;
            if i >= 0 && (i as usize) < s.data.len() {
                lines.push(format!("{}: {}", s.name, fmt_axis(s.data[i as usize])));
            }
        }
        if !lines.is_empty() {
            let secs_ago = (cap as isize - idx).max(0);
            resp.on_hover_text(format!("{secs_ago} s ago\n{}", lines.join("\n")));
        }
    }
}

pub fn legend(ui: &mut egui::Ui, series: &[Series]) {
    ui.horizontal_wrapped(|ui| {
        for s in series {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 10.0), egui::Sense::hover());
            let y = rect.center().y;
            if s.dashed {
                ui.painter().extend(egui::Shape::dashed_line(
                    &[egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                    Stroke::new(1.5, s.color),
                    3.0,
                    2.0,
                ));
            } else {
                ui.painter().line_segment(
                    [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                    Stroke::new(2.5, s.color),
                );
            }
            ui.label(RichText::new(s.name).small());
            ui.add_space(6.0);
        }
    });
}

fn nice_max(v: f32) -> f32 {
    if v <= 0.0 {
        return 1.0;
    }
    let v = v * 1.1;
    let mag = 10f32.powf(v.log10().floor());
    for m in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if m * mag >= v {
            return m * mag;
        }
    }
    10.0 * mag
}

fn fmt_axis(v: f32) -> String {
    if v >= 100.0 || v == v.round() {
        format!("{v:.0}")
    } else if v >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    }
}

pub fn dims(w: u32, h: u32) -> String {
    if w == 0 {
        "–".into()
    } else {
        format!("{w}×{h}")
    }
}

pub fn or_dash(s: &str) -> String {
    if s.is_empty() {
        "–".into()
    } else {
        s.to_string()
    }
}

/// A key/value row in a grid.
pub fn kv(ui: &mut egui::Ui, k: &str, v: impl Into<egui::WidgetText>) {
    ui.label(RichText::new(k).color(weak(ui)));
    ui.label(v);
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_rounding() {
        assert_eq!(nice_max(0.0), 1.0);
        assert_eq!(nice_max(9.0), 10.0);
        assert_eq!(nice_max(18.5), 25.0);
        assert_eq!(nice_max(41.0), 50.0);
        assert_eq!(fmt_axis(2.5), "2.50");
        assert_eq!(fmt_axis(25.0), "25");
    }
}
