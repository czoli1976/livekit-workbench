//! The look from the design mockups: a dark workbench with one blue accent,
//! green/amber/red reserved for status, and the system UI font.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
};

pub const BG: Color32 = Color32::from_rgb(0x0f, 0x12, 0x16);
pub const PANEL: Color32 = Color32::from_rgb(0x16, 0x1a, 0x20);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x13, 0x17, 0x1c);
pub const RAISED: Color32 = Color32::from_rgb(0x1c, 0x21, 0x29);
pub const HOVER: Color32 = Color32::from_rgb(0x22, 0x28, 0x30);
pub const BORDER: Color32 = Color32::from_rgb(0x2a, 0x31, 0x3c);
pub const BORDER_STRONG: Color32 = Color32::from_rgb(0x3a, 0x43, 0x50);
pub const TEXT: Color32 = Color32::from_rgb(0xe6, 0xe9, 0xee);
pub const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa4, 0xb2);

pub const ACCENT: Color32 = Color32::from_rgb(0x4c, 0xc3, 0xff);
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(0x7f, 0xd0, 0xff);
pub const ACCENT_BG: Color32 = Color32::from_rgb(0x12, 0x28, 0x3a);

pub const GREEN: Color32 = Color32::from_rgb(0x5f, 0xd6, 0x8a);
pub const GREEN_TEXT: Color32 = Color32::from_rgb(0x7f, 0xe3, 0xa5);
pub const GREEN_BG: Color32 = Color32::from_rgb(0x18, 0x3a, 0x29);
pub const GREEN_LINE: Color32 = Color32::from_rgb(0x3f, 0x9b, 0x63);

pub const AMBER: Color32 = Color32::from_rgb(0xf2, 0xb8, 0x4b);
pub const AMBER_TEXT: Color32 = Color32::from_rgb(0xf6, 0xc9, 0x6b);
pub const AMBER_BG: Color32 = Color32::from_rgb(0x3d, 0x2f, 0x12);
pub const AMBER_LINE: Color32 = Color32::from_rgb(0xd4, 0x9a, 0x2a);

pub const RED: Color32 = Color32::from_rgb(0xff, 0x7b, 0x72);
pub const RED_LINE: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4b);

pub const VIOLET: Color32 = Color32::from_rgb(0xc8, 0xb5, 0xff);
pub const VIOLET_BG: Color32 = Color32::from_rgb(0x2a, 0x21, 0x40);

/// Apply the visuals, spacing and fonts once at start-up.
pub fn install(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    install_fonts(ctx);
    ctx.style_mut_of(egui::Theme::Dark, |style| {
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_corner_radius = CornerRadius::same(12);
        v.menu_corner_radius = CornerRadius::same(8);
        v.faint_bg_color = PANEL;
        v.extreme_bg_color = BG;
        v.text_edit_bg_color = Some(BG);
        v.code_bg_color = RAISED;
        v.hyperlink_color = ACCENT_TEXT;
        v.warn_fg_color = AMBER;
        v.error_fg_color = RED;
        v.override_text_color = None;
        v.weak_text_color = Some(MUTED);
        v.selection.bg_fill = ACCENT_BG;
        v.selection.stroke = Stroke::new(1.0, ACCENT_TEXT);
        v.striped = true;
        v.slider_trailing_fill = true;

        let r = CornerRadius::same(6);
        let w = &mut v.widgets;
        w.noninteractive.bg_fill = PANEL;
        w.noninteractive.weak_bg_fill = PANEL;
        w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        w.noninteractive.corner_radius = r;

        w.inactive.bg_fill = RAISED;
        w.inactive.weak_bg_fill = RAISED;
        w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
        w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        w.inactive.corner_radius = r;

        w.hovered.bg_fill = HOVER;
        w.hovered.weak_bg_fill = HOVER;
        w.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
        w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
        w.hovered.corner_radius = r;
        w.hovered.expansion = 0.0;

        w.active.bg_fill = ACCENT_BG;
        w.active.weak_bg_fill = ACCENT_BG;
        w.active.bg_stroke = Stroke::new(1.0, ACCENT);
        // Also egui's "strong" text colour, so keep it neutral.
        w.active.fg_stroke = Stroke::new(1.0, TEXT);
        w.active.corner_radius = r;
        w.active.expansion = 0.0;

        w.open.bg_fill = RAISED;
        w.open.weak_bg_fill = RAISED;
        w.open.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
        w.open.fg_stroke = Stroke::new(1.0, TEXT);
        w.open.corner_radius = r;

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(8.0, 7.0);
        s.button_padding = egui::vec2(10.0, 5.0);
        s.interact_size = egui::vec2(36.0, 28.0);
        s.combo_width = 180.0;
        s.window_margin = egui::Margin::same(16);
        s.menu_margin = egui::Margin::same(6);

        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(18.0)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Small, FontId::proportional(11.0)),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
    });
}

/// Use the operating system's UI and monospace fonts, keeping egui's bundled
/// fonts as fallbacks for glyphs they lack (arrows, symbols, emoji).
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, family: FontFamily, candidates: &[(&str, u32)]| {
        for (path, index) in candidates {
            if let Ok(bytes) = std::fs::read(path) {
                let mut data = FontData::from_owned(bytes);
                data.index = *index;
                fonts.font_data.insert(name.to_string(), Arc::new(data));
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, name.to_string());
                return;
            }
        }
    };
    add(
        "ui",
        FontFamily::Proportional,
        &[
            ("/System/Library/Fonts/SFNS.ttf", 0),
            ("/System/Library/Fonts/HelveticaNeue.ttc", 0),
            ("C:\\Windows\\Fonts\\segoeui.ttf", 0),
            ("/usr/share/fonts/truetype/inter/Inter-Regular.ttf", 0),
            ("/usr/share/fonts/opentype/inter/Inter-Regular.otf", 0),
            ("/usr/share/fonts/inter/Inter-Regular.otf", 0),
            ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", 0),
            ("/usr/share/fonts/noto/NotoSans-Regular.ttf", 0),
            ("/usr/share/fonts/google-noto/NotoSans-Regular.ttf", 0),
            ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
            ("/usr/share/fonts/TTF/DejaVuSans.ttf", 0),
        ],
    );
    add(
        "mono",
        FontFamily::Monospace,
        &[
            ("/System/Library/Fonts/SFNSMono.ttf", 0),
            ("/System/Library/Fonts/Menlo.ttc", 0),
            ("C:\\Windows\\Fonts\\CascadiaMono.ttf", 0),
            ("C:\\Windows\\Fonts\\consola.ttf", 0),
            (
                "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
                0,
            ),
            ("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf", 0),
            ("/usr/share/fonts/TTF/DejaVuSansMono.ttf", 0),
        ],
    );
    ctx.set_fonts(fonts);
}

/// Frames for the three main areas, as in the mockup.
pub fn top_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .inner_margin(egui::Margin::symmetric(18, 10))
        .stroke(Stroke::new(1.0, BORDER))
}
pub fn sidebar_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(SIDEBAR)
        .inner_margin(egui::Margin::symmetric(18, 14))
        .stroke(Stroke::new(1.0, BORDER))
}
pub fn central_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(BG)
        .inner_margin(egui::Margin::symmetric(20, 14))
}
pub fn status_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL)
        .inner_margin(egui::Margin::symmetric(18, 6))
        .stroke(Stroke::new(1.0, BORDER))
}
