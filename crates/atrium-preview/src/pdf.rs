//! PDF pages to PNG, and text search over the same pages.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use hayro::hayro_interpret::font::{FontData, FontQuery};
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask, TransformExt, interpret_page,
};
use hayro::hayro_syntax::Pdf;
use hayro::hayro_syntax::page::Page as PdfPage;
use hayro::kurbo::{Affine, BezPath, Point, Rect};
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings, render_into};

use crate::{Fonts, Hit, Page, Refused};

/// Open a PDF, separating password protection from damage.
pub(crate) fn open(bytes: &[u8]) -> Result<Pdf, Refused> {
    let data = bytes.to_vec();
    match catch_unwind(AssertUnwindSafe(|| Pdf::new(data))) {
        Ok(Ok(pdf)) => Ok(pdf),
        Ok(Err(hayro::hayro_syntax::LoadPdfError::Decryption(_))) => Err(Refused::Password),
        Ok(Err(_)) => Err(Refused::Damaged),
        Err(_) => Err(Refused::Damaged),
    }
}

pub(crate) fn count(pdf: &Pdf) -> usize {
    pdf.pages().len()
}

/// Draw the wanted pages, `width_px` wide, into `cache`.
pub(crate) fn render_pages(
    pdf: &Pdf,
    cache: &crate::cache::Cache,
    key: &str,
    width_px: u32,
    wanted: std::ops::Range<usize>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Vec<Page>, Refused> {
    use std::sync::atomic::Ordering;

    let pages = pdf.pages();
    let mut out = Vec::new();
    let settings = interpreter_settings();
    let render_settings = RenderSettings::default();
    let mut render_cache = RenderCache::new();
    for index in wanted {
        if cancel.load(Ordering::SeqCst) {
            return Err(Refused::Timeout);
        }
        let Some(page) = pages.get(index) else {
            break;
        };
        let (width_pt, height_pt) = page.render_dimensions();
        let path = match cache.page(key, index, width_px) {
            Some(path) => path,
            None => {
                let png = draw_page(
                    page,
                    &mut render_cache,
                    &settings,
                    &render_settings,
                    width_px,
                )?;
                cache.put_page(key, index, width_px, &png).map_err(|err| {
                    Refused::Failed(format!("the preview could not be saved: {err}"))
                })?
            }
        };
        out.push(Page {
            index,
            png: path,
            width_pt,
            height_pt,
        });
    }
    Ok(out)
}

fn draw_page<'a>(
    page: &'a PdfPage<'a>,
    cache: &mut RenderCache<'a>,
    settings: &InterpreterSettings,
    render_settings: &RenderSettings,
    width_px: u32,
) -> Result<Vec<u8>, Refused> {
    let (width_pt, height_pt) = page.render_dimensions();
    if width_pt <= 0.0 || height_pt <= 0.0 {
        return Err(Refused::Damaged);
    }
    let width = width_px.max(1).min(u16::MAX as u32) as u16;
    let scale = width as f64 / width_pt as f64;
    let height = ((height_pt as f64 * scale).round() as u32).clamp(1, u16::MAX as u32) as u16;

    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut ctx = RenderContext::new(width, height);
        let transform =
            Affine::scale_non_uniform(scale, scale) * page.initial_transform(true).to_kurbo();
        render_into(page, cache, settings, render_settings, &mut ctx, transform);
        ctx.flush();
        let mut pixmap = Pixmap::new(width, height);
        let mut resources = Resources::default();
        ctx.render_with(
            &mut pixmap,
            &mut resources,
            RasterizerSettings {
                target_init: TargetInit::Clear(WHITE),
                ..Default::default()
            },
        );
        encode_png(&pixmap)
    }));
    match result {
        Ok(Ok(png)) => Ok(png),
        Ok(Err(why)) => Err(Refused::Failed(why)),
        Err(_) => Err(Refused::Damaged),
    }
}

fn encode_png(pixmap: &Pixmap) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut encoder =
            png::Encoder::new(&mut out, pixmap.width() as u32, pixmap.height() as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|err| format!("the page could not be saved: {err}"))?;
        writer
            .write_image_data(pixmap.data_as_u8_slice())
            .map_err(|err| format!("the page could not be saved: {err}"))?;
    }
    Ok(out)
}

fn interpreter_settings() -> InterpreterSettings {
    InterpreterSettings {
        font_resolver: Arc::new(|query| match query {
            FontQuery::Standard(font) => Some(font.get_font_data()),
            FontQuery::Fallback(_) => cjk_font(),
        }),
        ..Default::default()
    }
}

/// A system CJK face for PDFs whose fonts are not embedded. Loaded once.
fn cjk_font() -> Option<(FontData, u32)> {
    static FONT: OnceLock<Option<(FontData, u32)>> = OnceLock::new();
    FONT.get_or_init(|| {
        let path = cjk_font_path()?;
        let bytes = std::fs::read(path).ok()?;
        let data: FontData = Arc::new(bytes);
        Some((data, 0))
    })
    .clone()
}

pub(crate) fn cjk_font_path() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push("/System/Library/Fonts/Hiragino Sans GB.ttc".into());
        candidates.push("/System/Library/Fonts/STHeiti Light.ttc".into());
        candidates.push("/System/Library/Fonts/Supplemental/Songti.ttc".into());
    } else if cfg!(windows) {
        let win = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| "C:\\Windows".into());
        candidates.push(win.join("Fonts\\msyh.ttc"));
        candidates.push(win.join("Fonts\\simsun.ttc"));
    } else {
        candidates.push("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into());
        candidates.push("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc".into());
        candidates.push("/usr/share/fonts/opentype/noto/NotoSerifCJK-Regular.ttc".into());
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// The family name of [`cjk_font_path`], for Typst's last-resort fallback.
pub(crate) fn cjk_family() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("Hiragino Sans GB")
    } else if cfg!(windows) {
        Some("Microsoft YaHei")
    } else {
        Some("Noto Sans CJK SC")
    }
}

/// Text search over the pages, with a box per match in page points.
pub(crate) fn find(pdf: &Pdf, needle: &str, fonts: &Fonts) -> Result<Vec<Hit>, Refused> {
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let settings = interpreter_settings();
    let mut hits = Vec::new();
    for (page_index, page) in pdf.pages().iter().enumerate() {
        let mut device = TextDevice::default();
        let (width, height) = page.render_dimensions();
        let cache = InterpreterCache::new();
        let mut context = Context::new(
            page.initial_transform(true).to_kurbo(),
            Rect::new(0.0, 0.0, width as f64, height as f64),
            &cache,
            page.xref(),
            settings.clone(),
        );
        let result = catch_unwind(AssertUnwindSafe(|| {
            interpret_page(page, &mut context, &mut device);
        }));
        if result.is_err() {
            continue;
        }
        for rect in device.matches(needle) {
            hits.push(Hit {
                page: page_index,
                rect_pt: [
                    rect.x0 as f32,
                    rect.y0 as f32,
                    rect.width() as f32,
                    rect.height() as f32,
                ],
            });
            if hits.len() >= 500 {
                return Ok(hits);
            }
        }
    }
    let _ = fonts;
    Ok(hits)
}

#[derive(Default)]
struct TextDevice {
    spans: Vec<TextSpan>,
}

struct TextSpan {
    text: String,
    rect: Rect,
}

impl TextDevice {
    fn matches(&self, needle: &str) -> Vec<Rect> {
        let mut text = String::new();
        let mut starts = Vec::with_capacity(self.spans.len());
        let mut chars = 0usize;
        for span in &self.spans {
            let count = span.text.chars().count();
            starts.push((chars, count, span.rect));
            chars += count;
            text.push_str(&span.text);
        }
        let needle_len = needle.chars().count();
        let mut out = Vec::new();
        for (byte, _) in text.match_indices(needle) {
            let start = text[..byte].chars().count();
            let end = start + needle_len;
            let mut found: Option<Rect> = None;
            for (at, count, rect) in &starts {
                if *at + *count <= start {
                    continue;
                }
                if *at >= end {
                    break;
                }
                found = Some(match found {
                    Some(current) => current.union(*rect),
                    None => *rect,
                });
            }
            if let Some(rect) = found {
                out.push(rect);
            }
        }
        out
    }
}

impl<'a> Device<'a> for TextDevice {
    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn draw_glyph_run(
        &mut self,
        run: &hayro::hayro_interpret::font::GlyphRun<'_, 'a>,
        props: DrawProps<'a>,
        _: &DrawMode,
    ) {
        for glyph in run.glyphs() {
            let Some(unicode) = glyph.as_unicode() else {
                continue;
            };
            let text = match unicode {
                hayro::hayro_interpret::hayro_cmap::BfString::Char(c) => c.to_string(),
                hayro::hayro_interpret::hayro_cmap::BfString::String(s) => s,
            };
            if text.is_empty() {
                continue;
            }
            let matrix = props.transform * glyph.transform();
            let origin = matrix * Point::new(0.0, 0.0);
            let advance = match &**glyph {
                hayro::hayro_interpret::font::Glyph::Outline(outline) => {
                    outline.advance_width().unwrap_or(1.0) as f64
                }
                hayro::hayro_interpret::font::Glyph::Type3(type3) => {
                    type3.advance_width().unwrap_or(1.0) as f64
                }
            };
            let next = matrix * Point::new(advance, 0.0);
            let unit = matrix * Point::new(0.0, 1.0);
            let size = (unit.y - origin.y).abs().max(1.0);
            let left = origin.x.min(next.x);
            let width = (next.x - origin.x).abs().max(size * 0.2);
            let rect = Rect::new(
                left,
                origin.y - size * 0.82,
                left + width,
                origin.y + size * 0.18,
            );
            self.spans.push(TextSpan { text, rect });
        }
    }
}
