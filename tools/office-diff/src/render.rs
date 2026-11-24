//! Rasterizes Atrium's layout items into a PNG, so the two renderers can be
//! compared as images. Text is drawn from the same system faces the shaper
//! measured, through `rustybuzz` and `ttf-parser` outlines.

use anyhow::{Context as _, Result};
use atrium_layout::{Item, Rect, Seg};
use atrium_office::look::{Dash, Rgb};
use tiny_skia::{
    Color, FillRule, FilterQuality, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pattern,
    Pixmap, Point, SpreadMode, Stroke, StrokeDash, Transform,
};

use crate::shaper::{DrawRun, SystemShaper};

/// A page rendered by Atrium, before it becomes a PNG.
pub struct RenderTarget {
    pub width_pt: f32,
    pub height_pt: f32,
    pub background: Option<Rgb>,
    pub items: Vec<Item>,
}

/// Draws `target` at `width_px` wide. The height follows the page's aspect.
pub fn render(shaper: &SystemShaper, target: &RenderTarget, width_px: u32) -> Result<Pixmap> {
    let width_pt = target.width_pt.max(1.0);
    let height_pt = target.height_pt.max(1.0);
    let scale = width_px.max(1) as f32 / width_pt;
    let width = width_px.max(1);
    let height = ((height_pt * scale).round() as u32).clamp(1, 20_000);
    let mut pixmap = Pixmap::new(width, height).context("a page too large to rasterize")?;
    pixmap.fill(Color::from_rgba8(255, 255, 255, 255));
    if let Some(background) = target.background {
        pixmap.fill(color(background, 255));
    }
    let base = Transform::from_scale(scale, scale);
    for item in &target.items {
        draw_item(shaper, &mut pixmap, item, base, scale);
    }
    Ok(pixmap)
}

fn draw_item(shaper: &SystemShaper, pixmap: &mut Pixmap, item: &Item, base: Transform, scale: f32) {
    match item {
        Item::Text {
            x,
            baseline,
            text,
            font,
            color,
            rotation,
            center,
        } => {
            let transform = base.pre_concat(rotation_at(*rotation, *center));
            draw_text(shaper, pixmap, text, font, *x, *baseline, *color, transform);
        }
        Item::Fill { rect, color: fill } => {
            let path = rect_path(*rect, base);
            let mut paint = Paint::default();
            paint.set_color(color(*fill, 255));
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        Item::Rule {
            from,
            to,
            width,
            color: pen,
        } => {
            let mut pb = PathBuilder::new();
            pb.move_to(from.0, from.1);
            pb.line_to(to.0, to.1);
            if let Some(path) = pb.finish().and_then(|p| p.transform(base)) {
                let mut paint = Paint::default();
                paint.set_color(color(*pen, 255));
                let stroke = Stroke {
                    width: (width * scale).max(0.5),
                    line_cap: LineCap::Butt,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
        Item::Picture { rect, path } => {
            draw_picture(pixmap, *rect, path.as_deref(), base, scale);
        }
        Item::Path {
            segments,
            fill,
            stroke,
            rotation,
            center,
        } => {
            let Some(path) = path_from_segments(segments) else {
                return;
            };
            let Some(path) = path.transform(base.pre_concat(rotation_at(*rotation, *center)))
            else {
                return;
            };
            if let Some((fill, alpha)) = fill {
                let mut paint = Paint::default();
                paint.set_color(color(*fill, *alpha));
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
            if let Some(pen) = stroke {
                let mut paint = Paint::default();
                paint.set_color(color(pen.color, pen.alpha));
                pixmap.stroke_path(
                    &path,
                    &paint,
                    &stroke_style(pen.width * scale, pen.dash),
                    Transform::identity(),
                    None,
                );
            }
        }
        Item::Gradient { rect, angle, stops } => {
            let Some(shader) = gradient_shader(*rect, *angle, stops, base) else {
                return;
            };
            let path = rect_path(*rect, base);
            let paint = Paint {
                shader,
                anti_alias: true,
                ..Paint::default()
            };
            pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    shaper: &SystemShaper,
    pixmap: &mut Pixmap,
    text: &str,
    font: &atrium_layout::FontKey,
    x: f32,
    baseline: f32,
    fill: Rgb,
    transform: Transform,
) {
    if font.size <= 0.0 || text.is_empty() {
        return;
    }
    let color = color(fill, 255);
    // The pen carries across script groups: `draw_runs` splits one text item
    // into a Latin group and a CJK group, and the second starts where the
    // first ended.
    let mut pen_x = 0.0;
    for DrawRun { face, size, glyphs } in shaper.draw_runs(text, font) {
        let Ok(parsed) = ttf_parser::Face::parse(&face.data, face.index) else {
            continue;
        };
        let k = size / parsed.units_per_em().max(1) as f32;
        for glyph in glyphs {
            let mut outline = GlyphPath {
                pb: PathBuilder::new(),
                x0: x + pen_x + glyph.x,
                baseline,
                k,
                gy: glyph.y,
            };
            parsed.outline_glyph(ttf_parser::GlyphId(glyph.glyph), &mut outline);
            if let Some(path) = outline.pb.finish().and_then(|p| p.transform(transform)) {
                let mut paint = Paint::default();
                paint.set_color(color);
                pixmap.fill_path(
                    &path,
                    &paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
            pen_x += glyph.advance;
        }
    }
}

struct GlyphPath {
    pb: PathBuilder,
    x0: f32,
    baseline: f32,
    k: f32,
    gy: f32,
}

impl ttf_parser::OutlineBuilder for GlyphPath {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb
            .move_to(self.x0 + x * self.k, self.baseline - self.gy - y * self.k);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.pb
            .line_to(self.x0 + x * self.k, self.baseline - self.gy - y * self.k);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(
            self.x0 + x1 * self.k,
            self.baseline - self.gy - y1 * self.k,
            self.x0 + x * self.k,
            self.baseline - self.gy - y * self.k,
        );
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(
            self.x0 + x1 * self.k,
            self.baseline - self.gy - y1 * self.k,
            self.x0 + x2 * self.k,
            self.baseline - self.gy - y2 * self.k,
            self.x0 + x * self.k,
            self.baseline - self.gy - y * self.k,
        );
    }

    fn close(&mut self) {
        self.pb.close();
    }
}

fn rotation_at(degrees: f32, center: (f32, f32)) -> Transform {
    if degrees.abs() < 0.01 {
        Transform::identity()
    } else {
        Transform::from_rotate_at(degrees, center.0, center.1)
    }
}

fn rect_path(rect: Rect, transform: Transform) -> tiny_skia::Path {
    let mut pb = PathBuilder::new();
    pb.push_rect(
        tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.width, rect.height)
            .unwrap_or(tiny_skia::Rect::from_xywh(0.0, 0.0, 0.0, 0.0).unwrap()),
    );
    pb.finish()
        .and_then(|path| path.transform(transform))
        .expect("a rectangle is a path")
}

fn path_from_segments(segments: &[Seg]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    for seg in segments {
        match seg {
            Seg::Move(x, y) => pb.move_to(*x, *y),
            Seg::Line(x, y) => pb.line_to(*x, *y),
            Seg::Quad(cx, cy, x, y) => pb.quad_to(*cx, *cy, *x, *y),
            Seg::Cubic(c1x, c1y, c2x, c2y, x, y) => pb.cubic_to(*c1x, *c1y, *c2x, *c2y, *x, *y),
            Seg::Close => pb.close(),
        }
    }
    pb.finish()
}

fn stroke_style(width: f32, dash: Dash) -> Stroke {
    let (array, offset) = match dash {
        Dash::Solid => (None, 0.0),
        Dash::Dot => (Some(vec![width * 1.0, width * 2.0]), 0.0),
        Dash::Dash => (Some(vec![width * 3.0, width * 2.0]), 0.0),
        Dash::DashDot => (
            Some(vec![width * 3.0, width * 1.5, width * 1.0, width * 1.5]),
            0.0,
        ),
        Dash::LongDash => (Some(vec![width * 6.0, width * 2.0]), 0.0),
    };
    Stroke {
        width: width.max(0.4),
        line_cap: LineCap::Butt,
        line_join: LineJoin::Miter,
        miter_limit: 4.0,
        dash: array.and_then(|a| StrokeDash::new(a, offset)),
    }
}

fn draw_picture(
    pixmap: &mut Pixmap,
    rect: Rect,
    path: Option<&std::path::Path>,
    base: Transform,
    scale: f32,
) {
    if let Some(path) = path
        && let Ok(bytes) = std::fs::read(path)
        && let Ok(image) = Pixmap::decode_png(&bytes)
    {
        let sx = (rect.width * scale) / image.width() as f32;
        let sy = (rect.height * scale) / image.height() as f32;
        let t = Transform::from_translate(rect.x * scale, rect.y * scale)
            .pre_concat(Transform::from_scale(sx, sy));
        let paint = Paint {
            shader: Pattern::new(
                image.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Bilinear,
                1.0,
                t,
            ),
            ..Paint::default()
        };
        let dest = rect_path(rect, base);
        pixmap.fill_path(
            &dest,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        return;
    }
    // A placeholder when the picture could not be read: a light grey box
    // with a diagonal, so a missing picture still shows in the diff.
    let path = rect_path(rect, base);
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba8(230, 230, 230, 255));
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn gradient_shader(
    rect: Rect,
    angle: f32,
    stops: &[(f32, Rgb, u8)],
    base: Transform,
) -> Option<tiny_skia::Shader<'static>> {
    if stops.is_empty() {
        return None;
    }
    let radians = angle.to_radians();
    let (dx, dy) = (radians.cos(), radians.sin());
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let half = (rect.width * dx.abs() + rect.height * dy.abs()) / 2.0;
    let start = Point::from_xy(cx - dx * half, cy - dy * half);
    let end = Point::from_xy(cx + dx * half, cy + dy * half);
    let stops: Vec<tiny_skia::GradientStop> = stops
        .iter()
        .map(|(at, rgb, alpha)| tiny_skia::GradientStop::new(*at, color(*rgb, *alpha)))
        .collect();
    LinearGradient::new(start, end, stops, SpreadMode::Pad, base)
}

fn color(rgb: Rgb, alpha: u8) -> Color {
    let value = rgb.0;
    Color::from_rgba8(
        ((value >> 16) & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (value & 0xFF) as u8,
        alpha,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_layout::FontKey;

    #[test]
    fn a_text_page_has_ink() {
        let shaper = SystemShaper::new();
        let target = RenderTarget {
            width_pt: 200.0,
            height_pt: 100.0,
            background: None,
            items: vec![Item::Text {
                x: 10.0,
                baseline: 40.0,
                text: "Hello 预算".into(),
                font: FontKey {
                    family: String::new(),
                    size: 20.0,
                    bold: false,
                    italic: false,
                    cjk: false,
                },
                color: Rgb(0x000000),
                rotation: 0.0,
                center: (0.0, 0.0),
            }],
        };
        let pixmap = render(&shaper, &target, 400).unwrap();
        let dark = pixmap
            .pixels()
            .iter()
            .filter(|p| p.red() < 128 && p.green() < 128 && p.blue() < 128)
            .count();
        assert!(dark > 20, "only {dark} dark pixels");
    }

    #[test]
    fn a_fill_rectangle_covers_its_rect() {
        let shaper = SystemShaper::new();
        let target = RenderTarget {
            width_pt: 100.0,
            height_pt: 100.0,
            background: None,
            items: vec![Item::Fill {
                rect: Rect {
                    x: 25.0,
                    y: 25.0,
                    width: 50.0,
                    height: 50.0,
                },
                color: Rgb(0xFF0000),
            }],
        };
        let pixmap = render(&shaper, &target, 100).unwrap();
        let p = pixmap.pixel(50, 50).unwrap();
        assert_eq!((p.red(), p.green(), p.blue()), (255, 0, 0));
        let p = pixmap.pixel(5, 5).unwrap();
        assert_eq!((p.red(), p.green(), p.blue()), (255, 255, 255));
    }
}
