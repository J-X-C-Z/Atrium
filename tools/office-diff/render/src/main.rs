//! Renders Atrium's own layout for the Office-versus-LibreOffice
//! comparison (`tools/office-diff`).
//!
//! ```text
//! atrium-office-diff pages <file.docx|file.pptx> <out-dir> [--dpi 96]
//! atrium-office-diff sheet <file.xlsx> <out.json>
//! ```
//!
//! `pages` writes `page-001.svg`, `page-001.png` and `text.json` (every text
//! run's position in page points, for the text-position score). `sheet`
//! writes every cell's displayed text, so number formats can be compared
//! with LibreOffice's CSV export.

mod shaper;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use atrium_layout::Item;
use atrium_office::look::Rgb;
use atrium_office::{Deck, Office, WordDoc};

use crate::shaper::SystemShaper;

fn main() {
    if let Err(err) = run() {
        eprintln!("office-diff-render: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("pages") => {
            let (input, out) = two_paths(&args, "pages")?;
            let dpi = flag(&args, "--dpi")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(96.0);
            pages(&input, &out, dpi)
        }
        Some("sheet") => {
            let (input, out) = two_paths(&args, "sheet")?;
            sheet(&input, &out)
        }
        _ => {
            eprintln!(
                "usage:\n  atrium-office-diff pages <docx|pptx> <out-dir> [--dpi 96]\n  \
                 atrium-office-diff sheet <xlsx> <out.json>"
            );
            std::process::exit(2);
        }
    }
}

fn two_paths(args: &[String], command: &str) -> Result<(PathBuf, PathBuf)> {
    let input = args
        .get(1)
        .context(format!("{command}: missing input file"))?;
    let out = args
        .get(2)
        .context(format!("{command}: missing output path"))?;
    Ok((PathBuf::from(input), PathBuf::from(out)))
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let at = args.iter().position(|arg| arg == name)?;
    args.get(at + 1).cloned()
}

fn pages(input: &Path, out_dir: &Path, dpi: f32) -> Result<()> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot make {}", out_dir.display()))?;
    let document = atrium_office::open(input)
        .map_err(|err| anyhow::anyhow!("cannot open {}: {err}", input.display()))?;
    let shaper = SystemShaper::new();
    let layout = match &document.office {
        Office::Word(doc) => layout_word(doc, &shaper),
        Office::Slides(deck) => layout_deck(deck, &shaper),
        Office::Sheet(_) => bail!("{} is a workbook; use the sheet command", input.display()),
    };

    let mut text_pages = Vec::new();
    for (index, page) in layout.pages.iter().enumerate() {
        let svg = svg_page(page);
        let stem = format!("page-{:03}", index + 1);
        std::fs::write(out_dir.join(format!("{stem}.svg")), &svg)
            .with_context(|| format!("cannot write {stem}.svg"))?;
        render_png(&svg, &out_dir.join(format!("{stem}.png")), dpi)
            .with_context(|| format!("cannot render {stem}.png"))?;
        text_pages.push(text_page(index, page));
    }
    let json = serde_json::json!({
        "file": input.display().to_string(),
        "width_pt": layout.pages.first().map(|p| p.width),
        "height_pt": layout.pages.first().map(|p| p.height),
        "pages": text_pages,
    });
    std::fs::write(out_dir.join("text.json"), serde_json::to_vec_pretty(&json)?)?;
    Ok(())
}

fn layout_word(doc: &WordDoc, shaper: &SystemShaper) -> atrium_layout::Layout {
    atrium_layout::layout(doc, shaper)
}

fn layout_deck(deck: &Deck, shaper: &SystemShaper) -> atrium_layout::Layout {
    atrium_layout::layout_deck(deck, shaper)
}

fn text_page(index: usize, page: &atrium_layout::Page) -> serde_json::Value {
    let runs: Vec<serde_json::Value> = page
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Text {
                x,
                baseline,
                text,
                font,
                rotation,
                ..
            } => Some(serde_json::json!({
                "text": text,
                "x": x,
                "y": baseline,
                "size": font.size,
                "rotation": rotation,
            })),
            _ => None,
        })
        .collect();
    serde_json::json!({
        "index": index + 1,
        "width": page.width,
        "height": page.height,
        "text": runs,
    })
}

fn svg_page(page: &atrium_layout::Page) -> String {
    let mut svg = String::new();
    let _ = write!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{w}pt" height="{h}pt" viewBox="0 0 {w} {h}">"#,
        w = page.width,
        h = page.height,
    );
    let background = page.background.unwrap_or(Rgb::WHITE);
    let _ = write!(
        svg,
        r#"<rect width="{w}" height="{h}" fill="{}"/>"#,
        color(background),
        w = page.width,
        h = page.height,
    );
    let mut gradients = String::new();
    for (index, item) in page.items.iter().enumerate() {
        draw_item(&mut svg, &mut gradients, index, item);
    }
    if !gradients.is_empty() {
        let _ = write!(svg, "<defs>{gradients}</defs>");
    }
    svg.push_str("</svg>");
    svg
}

fn draw_item(svg: &mut String, gradients: &mut String, index: usize, item: &Item) {
    match item {
        Item::Fill { rect, color: fill } => {
            let _ = write!(
                svg,
                r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="{}"/>"#,
                color(*fill),
                x = rect.x,
                y = rect.y,
                w = rect.width,
                h = rect.height,
            );
        }
        Item::Rule {
            from,
            to,
            width,
            color: stroke,
        } => {
            let _ = write!(
                svg,
                r#"<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{}" stroke-width="{width}"/>"#,
                color(*stroke),
                x1 = from.0,
                y1 = from.1,
                x2 = to.0,
                y2 = to.1,
            );
        }
        Item::Text {
            x,
            baseline,
            text,
            font,
            color: ink,
            rotation,
            center,
        } => {
            let family = family_list(&font.family);
            let transform = if *rotation != 0.0 {
                format!(
                    r#" transform="rotate({rotation} {cx} {cy})""#,
                    cx = center.0,
                    cy = center.1
                )
            } else {
                String::new()
            };
            let _ = write!(
                svg,
                r#"<text x="{x}" y="{y}" font-family="{family}" font-size="{size}"{bold}{italic} fill="{}"{transform} xml:space="preserve">{text}</text>"#,
                color(*ink),
                y = baseline,
                size = font.size,
                bold = if font.bold {
                    r#" font-weight="bold""#
                } else {
                    ""
                },
                italic = if font.italic {
                    r#" font-style="italic""#
                } else {
                    ""
                },
                text = escape(text),
            );
        }
        Item::Picture { rect, path } => match path {
            Some(path) => {
                let _ = write!(
                    svg,
                    r#"<image x="{x}" y="{y}" width="{w}" height="{h}" preserveAspectRatio="none" xlink:href="{}"/>"#,
                    href(path),
                    x = rect.x,
                    y = rect.y,
                    w = rect.width,
                    h = rect.height,
                );
            }
            None => {
                let _ = write!(
                    svg,
                    r##"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="#eeeeee" stroke="#999999"/>"##,
                    x = rect.x,
                    y = rect.y,
                    w = rect.width,
                    h = rect.height,
                );
            }
        },
        Item::Path {
            segments,
            fill,
            stroke,
            rotation,
            center,
        } => {
            let d = path_data(segments);
            let mut paint = String::new();
            match fill {
                Some((fill, alpha)) => {
                    let _ = write!(
                        paint,
                        r#" fill="{}" fill-opacity="{}""#,
                        color(*fill),
                        *alpha as f32 / 255.0
                    );
                }
                None => paint.push_str(r#" fill="none""#),
            }
            if let Some(pen) = stroke {
                let dash = dash_array(pen.dash, pen.width);
                let _ = write!(
                    paint,
                    r#" stroke="{}" stroke-opacity="{}" stroke-width="{}"{dash}"#,
                    color(pen.color),
                    pen.alpha as f32 / 255.0,
                    pen.width,
                );
            }
            let transform = if *rotation != 0.0 {
                format!(
                    r#" transform="rotate({rotation} {cx} {cy})""#,
                    cx = center.0,
                    cy = center.1
                )
            } else {
                String::new()
            };
            let _ = write!(svg, r#"<path d="{d}"{paint}{transform}/>"#);
        }
        Item::Gradient { rect, angle, stops } => {
            let id = format!("grad{index}");
            let (x1, y1, x2, y2) = gradient_line(*rect, *angle);
            let _ = write!(
                gradients,
                r#"<linearGradient id="{id}" gradientUnits="userSpaceOnUse" x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}">"#
            );
            for (offset, stop, alpha) in stops {
                let _ = write!(
                    gradients,
                    r#"<stop offset="{offset}" stop-color="{}" stop-opacity="{}"/>"#,
                    color(*stop),
                    *alpha as f32 / 255.0
                );
            }
            gradients.push_str("</linearGradient>");
            let _ = write!(
                svg,
                r#"<rect x="{x}" y="{y}" width="{w}" height="{h}" fill="url(#{id})"/>"#,
                x = rect.x,
                y = rect.y,
                w = rect.width,
                h = rect.height,
            );
        }
    }
}

fn gradient_line(rect: atrium_layout::Rect, angle: f32) -> (f32, f32, f32, f32) {
    let radians = angle.to_radians();
    let (dx, dy) = (radians.cos(), radians.sin());
    let half = (rect.width * dx.abs() + rect.height * dy.abs()) / 2.0;
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    (
        cx - dx * half,
        cy - dy * half,
        cx + dx * half,
        cy + dy * half,
    )
}

fn path_data(segments: &[atrium_layout::Seg]) -> String {
    let mut d = String::new();
    for segment in segments {
        match segment {
            atrium_layout::Seg::Move(x, y) => {
                let _ = write!(d, "M{x} {y} ");
            }
            atrium_layout::Seg::Line(x, y) => {
                let _ = write!(d, "L{x} {y} ");
            }
            atrium_layout::Seg::Quad(cx, cy, x, y) => {
                let _ = write!(d, "Q{cx} {cy} {x} {y} ");
            }
            atrium_layout::Seg::Cubic(c1x, c1y, c2x, c2y, x, y) => {
                let _ = write!(d, "C{c1x} {c1y} {c2x} {c2y} {x} {y} ");
            }
            atrium_layout::Seg::Close => d.push_str("Z "),
        }
    }
    d.trim_end().to_string()
}

fn dash_array(dash: atrium_office::look::Dash, width: f32) -> String {
    use atrium_office::look::Dash;
    let unit = width.max(0.5);
    let pattern = match dash {
        Dash::Solid => return String::new(),
        Dash::Dot => format!("{unit} {}", unit * 2.0),
        Dash::Dash => format!("{} {}", unit * 4.0, unit * 3.0),
        Dash::DashDot => format!("{} {} {} {}", unit * 4.0, unit * 3.0, unit, unit * 3.0),
        Dash::LongDash => format!("{} {}", unit * 8.0, unit * 3.0),
    };
    format!(r#" stroke-dasharray="{pattern}""#)
}

fn color(rgb: Rgb) -> String {
    format!("#{:06x}", rgb.0 & 0xff_ffff)
}

fn family_list(family: &str) -> String {
    let fallback = "\"Noto Sans CJK SC\", \"PingFang SC\", \"Helvetica\", \"Arial\", sans-serif";
    if family.is_empty() {
        fallback.to_string()
    } else {
        format!(
            "\"{}\", \"Carlito\", \"Noto Sans\", \"Noto Sans CJK SC\", \"PingFang SC\", sans-serif",
            family.replace('"', "")
        )
    }
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn href(path: &Path) -> String {
    let absolute = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    escape(&format!("file://{}", absolute.replace(' ', "%20")))
}

fn render_png(svg: &str, out: &Path, dpi: f32) -> Result<()> {
    let mut options = usvg::Options::default();
    options.dpi = dpi;
    options.fontdb_mut().load_system_fonts();
    let tree = usvg::Tree::from_str(svg, &options)?;
    let size = tree.size();
    let width = size.width().ceil().max(1.0) as u32;
    let height = size.height().ceil().max(1.0) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .with_context(|| format!("a {width}x{height} page is too large to draw"))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    std::fs::write(out, pixmap.encode_png()?)
        .with_context(|| format!("cannot write {}", out.display()))
}

fn sheet(input: &Path, out: &Path) -> Result<()> {
    let document = atrium_office::open(input)
        .map_err(|err| anyhow::anyhow!("cannot open {}: {err}", input.display()))?;
    let Office::Sheet(workbook) = &document.office else {
        bail!("{} is not a workbook", input.display());
    };
    let mut sheets = Vec::new();
    for (index, sheet) in workbook.sheets().iter().enumerate() {
        let (rows, cols) = sheet.extent();
        let rows = rows.min(500);
        let cols = cols.min(100);
        let mut values = Vec::new();
        for r in 0..rows {
            let mut row = Vec::new();
            for c in 0..cols {
                row.push(workbook.shown(index, r, c).text);
            }
            values.push(row);
        }
        sheets.push(serde_json::json!({
            "name": sheet.name,
            "hidden": sheet.hidden,
            "rows": values,
            "merged": sheet.merged,
        }));
    }
    let json = serde_json::json!({
        "file": input.display().to_string(),
        "sheets": sheets,
    });
    std::fs::write(out, serde_json::to_vec_pretty(&json)?)
        .with_context(|| format!("cannot write {}", out.display()))
}
