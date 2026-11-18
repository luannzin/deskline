//! Deskline's look. The interface is luannzin.com's neutral system: one size of Inter, ink in
//! three tiers, hairlines, rows that light up on hover, no accent colour. Colour only means
//! something: red while someone is in your computer, green when you have control. The dusk from
//! the mark (violet, plum, ember, gold in a 4x4 Bayer dither) is the one piece of art, and it
//! does a job: it is the link between the two computers, denser and warmer the better it is.
use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Rect, Response, RichText, Sense, Shape, Stroke,
    TextStyle, Ui, Vec2, vec2,
};

pub const GROUND: Color32 = Color32::from_rgb(0x16, 0x16, 0x16);
pub const TILE: Color32 = Color32::from_rgb(0x1f, 0x1f, 0x1f);
pub const WELL: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1c);
pub const HAIRLINE: Color32 = Color32::from_rgb(0x24, 0x24, 0x24);
pub const RAISED: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);
pub const HOVER: Color32 = Color32::from_rgb(0x32, 0x32, 0x32);
pub const PRESSED: Color32 = Color32::from_rgb(0x40, 0x40, 0x40);
pub const INK: Color32 = Color32::from_rgb(0xf5, 0xf5, 0xf5);
pub const SOFT: Color32 = Color32::from_rgb(0xb2, 0xb2, 0xb2);
/// 4.5:1 or better on the ground, the tile and the well.
pub const MUTED: Color32 = Color32::from_rgb(0x88, 0x88, 0x88);
/// Someone is in this computer.
pub const RED: Color32 = Color32::from_rgb(0x9b, 0x2c, 0x2c);
/// You have control.
pub const GREEN: Color32 = Color32::from_rgb(0x5a, 0xc8, 0x78);
/// The dusk of the mark and of luannzin.com's dither strip, sky to horizon.
pub const DUSK: [Color32; 4] =
    [Color32::from_rgb(0x3a, 0x2a, 0x6b), Color32::from_rgb(0x7a, 0x33, 0x61), Color32::from_rgb(0xe0, 0x70, 0x3f), Color32::from_rgb(0xff, 0xd1, 0x80)];
const BAYER: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// One size for everything; hierarchy is weight and ink tier.
pub const SIZE: f32 = 15.0;
/// The code, the one thing on Home read aloud: Geist Mono, the only text above `SIZE`.
const CODE: f32 = 24.0;
const MEDIUM: &str = "medium";

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("inter".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Inter-Regular.ttf"))));
    fonts.font_data.insert("inter-medium".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/Inter-Medium.ttf"))));
    // luannzin.com's code face, for the code and anything measured.
    fonts.font_data.insert("geist-mono".into(), Arc::new(FontData::from_static(include_bytes!("../assets/fonts/GeistMono-Regular.ttf"))));
    fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "geist-mono".into());
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts.families.insert(FontFamily::Proportional, [vec!["inter".into()], fallbacks.clone()].concat());
    fonts.families.insert(FontFamily::Name(MEDIUM.into()), [vec!["inter-medium".into()], fallbacks].concat());
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    (v.panel_fill, v.window_fill, v.faint_bg_color, v.extreme_bg_color, v.code_bg_color) = (GROUND, TILE, TILE, WELL, WELL);
    v.window_stroke = Stroke::new(1.0, HAIRLINE);
    v.window_corner_radius = CornerRadius::same(14);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow.color = Color32::from_black_alpha(140);
    v.popup_shadow.color = Color32::from_black_alpha(120);
    v.hyperlink_color = INK;
    v.weak_text_color = Some(MUTED);
    v.selection.bg_fill = PRESSED;
    v.selection.stroke = Stroke::new(1.0, INK);
    v.text_cursor.stroke = Stroke::new(1.5, INK);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, SOFT);
    w.noninteractive.corner_radius = CornerRadius::same(6);
    // Pressed, or holding keyboard focus (egui draws both as active): an ink edge, so focus is
    // plain to see on every egui widget.
    for (s, fill, edge) in [(&mut w.inactive, RAISED, HAIRLINE), (&mut w.hovered, HOVER, PRESSED), (&mut w.active, PRESSED, INK), (&mut w.open, HOVER, PRESSED)] {
        (s.bg_fill, s.weak_bg_fill) = (fill, fill);
        s.fg_stroke = Stroke::new(1.0, INK);
        s.bg_stroke = Stroke::new(1.0, edge);
        s.corner_radius = CornerRadius::same(6);
        s.expansion = 0.0;
    }
    ctx.all_styles_mut(|style| {
        style.visuals = v.clone();
        style.text_styles = [
            (TextStyle::Body, FontId::proportional(SIZE)),
            (TextStyle::Button, FontId::proportional(SIZE)),
            (TextStyle::Heading, FontId::new(SIZE, FontFamily::Name(MEDIUM.into()))),
            (TextStyle::Small, FontId::proportional(SIZE)),
            (TextStyle::Monospace, FontId::monospace(14.0)),
        ]
        .into();
        let s = &mut style.spacing;
        s.item_spacing = vec2(8.0, 8.0);
        s.button_padding = vec2(12.0, 6.0);
        s.interact_size.y = 30.0;
        s.text_edit_width = 280.0;
    });
}

/// Ink, at the medium weight: titles and names.
pub fn strong(text: impl Into<String>) -> RichText {
    RichText::new(text).family(FontFamily::Name(MEDIUM.into())).color(INK)
}

pub fn soft(text: impl Into<String>) -> RichText {
    RichText::new(text).color(SOFT)
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(MUTED)
}

/// Text that runs to more than a line: the explanations. Leading at about 1.5 so a paragraph
/// reads as one.
pub fn prose(text: impl Into<String>) -> RichText {
    soft(text).line_height(Some(22.0))
}

/// The one filled button on a screen: paper ink, ground text.
pub fn primary(ui: &mut Ui, text: impl Into<String>) -> Response {
    primary_enabled(ui, true, text)
}

pub fn primary_enabled(ui: &mut Ui, enabled: bool, text: impl Into<String>) -> Response {
    let label = RichText::new(text).family(FontFamily::Name(MEDIUM.into())).color(if enabled { GROUND } else { MUTED });
    let ink = [INK, Color32::from_gray(0xde), Color32::from_gray(0xc8)];
    filled(ui, enabled, label, if enabled { ink } else { [RAISED; 3] }, INK)
}

/// The red bar's way out: white, in the bar's red.
pub fn stop(ui: &mut Ui, text: impl Into<String>) -> Response {
    let label = RichText::new(text).color(RED);
    filled(ui, true, label, [Color32::WHITE, Color32::from_gray(0xe6), Color32::from_gray(0xd0)], Color32::WHITE)
}

/// The red bar's other buttons: white words on a veil of white.
pub fn on_red(ui: &mut Ui, text: impl Into<String>) -> Response {
    let veil = |a| Color32::from_white_alpha(a);
    filled(ui, true, RichText::new(text).color(Color32::WHITE), [veil(28), veil(52), veil(76)], Color32::WHITE)
}

/// A filled button: `fills` at rest, under the pointer and pressed (or focused), no edge; with
/// keyboard focus, a `ring` just outside it, since a darker fill alone is easy to miss.
fn filled(ui: &mut Ui, enabled: bool, label: RichText, fills: [Color32; 3], ring: Color32) -> Response {
    let r = ui
        .scope(|ui| {
            let w = &mut ui.visuals_mut().widgets;
            for (s, fill) in [(&mut w.inactive, fills[0]), (&mut w.hovered, fills[1]), (&mut w.active, fills[2]), (&mut w.noninteractive, fills[0])] {
                (s.weak_bg_fill, s.bg_stroke) = (fill, Stroke::NONE);
            }
            ui.add_enabled(enabled, egui::Button::new(label).min_size(vec2(0.0, 32.0)))
        })
        .inner;
    if r.has_focus() {
        focus_ring(ui, r.rect.expand(2.0), 8.0, ring, egui::StrokeKind::Outside);
    }
    r
}

/// Where keyboard focus is: a 2px ring.
pub fn focus_ring(ui: &Ui, rect: Rect, radius: f32, color: Color32, kind: egui::StrokeKind) {
    ui.painter().rect_stroke(rect, radius, Stroke::new(2.0, color), kind);
}

/// A button that is only text until the pointer is on it.
pub fn quiet(ui: &mut Ui, text: impl Into<String>) -> Response {
    ui.add(egui::Button::new(soft(text)).frame_when_inactive(false))
}

pub fn hairline(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, HAIRLINE);
}

/// A row that lights up as a tile under the pointer, like luannzin.com's lists. The text stays
/// in line with the column; the tile reaches 12px past it on both sides. When `sense` takes
/// clicks the whole tile is one target (buttons on the row still take their own clicks), and
/// `Sense::click()` puts it in the Tab order too. Returns the row's response beside what `add`
/// returned.
pub fn row<R>(ui: &mut Ui, sense: Sense, add: impl FnOnce(&mut Ui) -> R) -> (Response, R) {
    let click = sense.senses_click();
    let tile = ui.available_rect_before_wrap().expand2(vec2(12.0, 0.0));
    // Sensed before the row's own widgets are added, so they sit on top of it.
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(tile).sense(sense));
    // Selectable text would take the click for itself.
    child.style_mut().interaction.selectable_labels = !click;
    let response = child.response();
    let bg = child.painter().add(Shape::Noop);
    let inner = egui::Frame::new().inner_margin(Margin::symmetric(12, 8)).show(&mut child, |ui| {
        ui.set_width(ui.available_width());
        add(ui)
    });
    let rect = inner.response.rect;
    if child.rect_contains_pointer(rect) || response.has_focus() {
        child.painter().set(bg, Shape::rect_filled(rect, 14.0, TILE));
    }
    if response.has_focus() {
        focus_ring(&child, rect, 14.0, INK, egui::StrokeKind::Inside);
    }
    if click && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    drop(child);
    ui.advance_cursor_after_rect(rect.shrink2(vec2(12.0, 0.0)));
    (response, inner.inner)
}

/// A close mark drawn as two strokes (no glyph) in a 24px target: muted, ink under the pointer
/// or with keyboard focus, which also gets a ring.
pub fn close(ui: &mut Ui) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
    let lit = response.hovered() || response.has_focus();
    let color = if lit { INK } else { MUTED };
    if response.has_focus() {
        focus_ring(ui, rect, 6.0, INK, egui::StrokeKind::Inside);
    }
    let r = Rect::from_center_size(rect.center(), vec2(7.0, 7.0));
    let stroke = Stroke::new(1.5, color);
    ui.painter().line_segment([r.left_top(), r.right_bottom()], stroke);
    ui.painter().line_segment([r.right_top(), r.left_bottom()], stroke);
    response
}

/// The width a field can take on a line that ends with a button labelled `label`.
pub fn beside(ui: &Ui, label: &str) -> f32 {
    let text = ui.painter().layout_no_wrap(label.to_string(), FontId::new(SIZE, FontFamily::Name(MEDIUM.into())), INK).size().x;
    let s = ui.spacing();
    (ui.available_width() - text - 2.0 * s.button_padding.x - s.item_spacing.x).max(80.0)
}

/// This computer's code, big enough to read over the phone and set as type, not boxed: three
/// even lines (the 13 groups go 5, 5, 3), never a lone group dangling at the end.
pub fn code(ui: &mut Ui, code: &str) {
    let groups: Vec<&str> = code.split(' ').collect();
    let text = groups.chunks(groups.len().div_ceil(3).max(1)).map(|c| c.join("  ")).collect::<Vec<_>>().join("\n");
    // Smaller only in a window too narrow for the lines at full size; a line never wraps.
    let wide = ui.painter().layout_no_wrap(text.clone(), FontId::monospace(CODE), INK).size().x;
    let size = (CODE * ui.available_width() / wide.max(1.0)).min(CODE).floor();
    ui.label(RichText::new(text).monospace().size(size).color(INK));
}

/// Paint `rect` with 2px dots on a 3px grid; `at(column, row, columns, rows)` says each cell's
/// colour, or None for none. Cells are whole device pixels, so the dither stays crisp at 125%
/// and 150% scaling instead of smearing.
fn dots(ui: &Ui, rect: Rect, mut at: impl FnMut(usize, usize, usize, usize) -> Option<Color32>) {
    let ppp = ui.ctx().pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let (cell, dot) = ((3.0 * ppp).round().max(2.0) / ppp, (2.0 * ppp).round().max(1.0) / ppp);
    let origin = egui::pos2(snap(rect.min.x), snap(rect.min.y));
    let (cols, rows) = ((rect.width() / cell) as usize, (rect.height() / cell) as usize);
    let painter = ui.painter_at(rect);
    for c in 0..cols {
        for r in 0..rows {
            if let Some(color) = at(c, r, cols, rows) {
                let p = origin + vec2(c as f32 * cell, r as f32 * cell);
                painter.rect_filled(Rect::from_min_size(p, vec2(dot, dot)), 0.0, color);
            }
        }
    }
}

fn bayer(c: usize, r: usize) -> f32 {
    f32::from(BAYER[(r % 4) * 4 + c % 4]) / 16.0
}

/// How good the link between the two computers is, 0 to 1, from its kind and round trip.
pub fn link_quality(kind: &str, ms: u32) -> f32 {
    let base = match kind {
        "LAN" => 1.0,
        "Direct" => 0.9,
        _ => 0.6,
    };
    (base - ms as f32 / 600.0).clamp(0.15, 1.0)
}

/// The link, drawn: a short dusk strip whose dots fill and warm with the link's quality. `None`
/// while there's no link (reconnecting): sparse dots that drift, so it reads as searching.
pub fn link(ui: &mut Ui, quality: Option<f32>) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(48.0, 15.0), Sense::hover());
    let drift = (ui.input(|i| i.time) * 6.0) as usize;
    dots(ui, rect, |c, r, cols, _| match quality {
        Some(q) => {
            let x = c as f32 / cols as f32;
            // the strip reaches as far as the link is good, and warms toward gold along it
            (x <= q && bayer(c, r) < 0.35 + 0.65 * q).then(|| DUSK[((x * 4.0) as usize).min(3)])
        }
        None => (bayer(c + drift, r) < 0.2).then_some(DUSK[0]),
    });
    if quality.is_none() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
    }
    response
}

/// Waiting on the other computer: the dusk filling in Bayer order, over and over, so the line
/// between the two computers is visibly being drawn.
pub fn waiting(ui: &mut Ui, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    let t = (ui.input(|i| i.time) / 2.4).fract() as f32;
    dots(ui, rect, |c, r, cols, _| {
        let x = c as f32 / cols as f32;
        // a front sweeps left to right; behind it the dots settle in
        let lit = (t * 1.3 - x).clamp(0.0, 1.0);
        (bayer(c, r) < lit).then(|| DUSK[((x * 4.0) as usize).min(3)])
    });
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
}

/// The mark at the whole multiple of its pixels nearest `size`, in device pixels, so no row or
/// column of the art is dropped or doubled.
pub fn mark(ui: &mut Ui, texture: &egui::TextureHandle, size: f32) -> Response {
    let (ppp, art) = (ui.ctx().pixels_per_point(), texture.size()[0] as f32);
    let k = (size * ppp / art).round().max(1.0);
    ui.add(egui::Image::new(texture).fit_to_exact_size(Vec2::splat(k * art / ppp)))
}
