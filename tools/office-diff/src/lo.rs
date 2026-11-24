//! LibreOffice headless rendering, and text positions read back out of the
//! PDF it makes.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, anyhow, bail};
use atrium_preview::{Ask, Fonts, Layout, Preview, Previewer};
use hayro::hayro_interpret::font::GlyphRun;
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::kurbo::Point;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image, ImageDrawProps,
    InterpreterCache, InterpreterSettings, RectExt, SoftMask, TransformExt, interpret_page,
};
use hayro::hayro_syntax::Pdf;

use crate::atrium::Token;
use crate::shaper::is_cjk;

/// A page rendered by LibreOffice: its image and the words found in it.
pub struct LoPage {
    pub png: PathBuf,
    pub width_pt: f32,
    pub height_pt: f32,
    pub tokens: Vec<Token>,
}

/// Whether the binary runs and answers.
pub fn available(soffice: &Path) -> bool {
    Command::new(soffice)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Converts `input` to a PDF in `work`. Returns the PDF path.
pub fn convert(soffice: &Path, input: &Path, work: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(work).with_context(|| format!("creating {}", work.display()))?;
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("a file without a name"))?;
    let pdf = work.join(format!("{stem}.pdf"));
    let _ = std::fs::remove_file(&pdf);
    let profile = work.join(format!("lo-profile-{}-{}", std::process::id(), stem));
    std::fs::create_dir_all(&profile).ok();
    let profile_url = format!(
        "-env:UserInstallation=file://{}",
        profile.to_string_lossy().replace(' ', "%20")
    );
    let output = Command::new(soffice)
        .args([
            "--headless",
            "--nologo",
            "--nodefault",
            "--nofirststartwizard",
            "--nolockcheck",
            &profile_url,
            "--convert-to",
            "pdf",
            "--outdir",
        ])
        .arg(work)
        .arg(input)
        .output()
        .with_context(|| format!("running {}", soffice.display()))?;
    if !output.status.success() {
        bail!(
            "LibreOffice failed on {}: {}",
            input.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if !pdf.is_file() {
        bail!(
            "LibreOffice wrote no PDF for {}: {}",
            input.display(),
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    Ok(pdf)
}

/// Rasterizes the PDF and reads its text. `width_px` is the common width the
/// comparison uses.
pub fn pages(pdf: &Path, work: &Path, width_px: u32) -> Result<Vec<LoPage>> {
    let cache = work.join("atrium-preview-cache");
    std::fs::create_dir_all(&cache).ok();
    let previewer = Previewer::new(cache, Fonts::system());
    let mut rendered = Vec::new();
    let mut total;
    let mut start = 0usize;
    loop {
        let preview = previewer
            .preview(
                pdf,
                &Ask {
                    width_px,
                    pages: start..start.saturating_add(20),
                    sheet: None,
                    layout: Layout::Pages,
                },
            )
            .map_err(|err| anyhow!("previewing {}: {err}", pdf.display()))?;
        let Preview::Pages {
            pages,
            total: count,
            ..
        } = preview
        else {
            bail!("{} did not preview as pages", pdf.display());
        };
        total = count;
        for page in pages {
            rendered.push(page);
        }
        start = rendered.len();
        if start >= total {
            break;
        }
    }
    let tokens = text_tokens(pdf)?;
    let mut out = Vec::new();
    for page in rendered {
        let words = tokens.get(page.index).cloned().unwrap_or_default();
        out.push(LoPage {
            png: page.png,
            width_pt: page.width_pt,
            height_pt: page.height_pt,
            tokens: words,
        });
    }
    Ok(out)
}

/// Every word on every page of the PDF, from its glyph runs.
pub fn text_tokens(pdf: &Path) -> Result<Vec<Vec<Token>>> {
    let bytes = std::fs::read(pdf).with_context(|| format!("reading {}", pdf.display()))?;
    let document = Pdf::new(bytes).map_err(|err| anyhow!("opening the PDF: {err:?}"))?;
    let settings = InterpreterSettings::default();
    let mut out = Vec::new();
    for index in 0..document.pages().len() {
        let page = &document.pages()[index];
        let cache = InterpreterCache::new();
        let target = page.intersected_crop_box().to_kurbo();
        let transform = page.initial_transform(true).to_kurbo();
        let mut ctx = Context::new(transform, target, &cache, page.xref(), settings.clone());
        let mut device = TextDevice::default();
        interpret_page(page, &mut ctx, &mut device);
        out.push(device.tokens());
    }
    Ok(out)
}

#[derive(Default)]
struct TextDevice {
    glyphs: Vec<RawGlyph>,
}

struct RawGlyph {
    text: String,
    x: f32,
    y: f32,
    size: f32,
}

impl<'a> Device<'a> for TextDevice {
    fn draw_path(
        &mut self,
        _: &hayro::hayro_interpret::kurbo::BezPath,
        _: DrawProps<'a>,
        _: &DrawMode,
    ) {
    }

    fn push_clip_path(&mut self, _: &ClipPath) {}

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}

    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for glyph in run.glyphs() {
            let matrix = props.transform * glyph.transform();
            let point = matrix * Point::ORIGIN;
            let [a, b, c, d, _, _] = matrix.as_coeffs();
            let size = (a * d - b * c).abs().sqrt() as f32;
            let text = match glyph.as_unicode() {
                Some(BfString::Char(c)) => c.to_string(),
                Some(BfString::String(s)) => s,
                None => continue,
            };
            if text.is_empty() {
                continue;
            }
            self.glyphs.push(RawGlyph {
                text,
                x: point.x as f32,
                y: point.y as f32,
                size: size.max(0.1),
            });
        }
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}

    fn pop_clip(&mut self) {}

    fn pop_transparency_group(&mut self) {}
}

impl TextDevice {
    fn tokens(self) -> Vec<Token> {
        let mut out = Vec::new();
        let mut current: Option<Token> = None;
        for glyph in self.glyphs {
            let mut chars = glyph.text.chars();
            // A multi-character mapping (a ligature) is kept as one token.
            let first = chars.next();
            let rest: String = chars.collect();
            for c in first.into_iter().chain(rest.chars()) {
                if c.is_whitespace() {
                    if let Some(token) = current.take() {
                        out.push(token);
                    }
                    continue;
                }
                if is_cjk(c) {
                    if let Some(token) = current.take() {
                        out.push(token);
                    }
                    out.push(Token {
                        text: c.to_string(),
                        x: glyph.x,
                        y: glyph.y,
                        size: glyph.size,
                    });
                    continue;
                }
                match &mut current {
                    Some(token) => {
                        // A large jump on the same baseline starts a new word
                        // even without a space.
                        if (glyph.y - token.y).abs() > glyph.size * 0.5 {
                            out.push(current.take().unwrap());
                            current = Some(Token {
                                text: c.to_string(),
                                x: glyph.x,
                                y: glyph.y,
                                size: glyph.size,
                            });
                        } else {
                            token.text.push(c);
                        }
                    }
                    None => {
                        current = Some(Token {
                            text: c.to_string(),
                            x: glyph.x,
                            y: glyph.y,
                            size: glyph.size,
                        })
                    }
                }
            }
        }
        if let Some(token) = current {
            out.push(token);
        }
        out
    }
}
