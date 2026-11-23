//! `styles.xml` for sheets: fonts, fills, borders, cell formats and the
//! records a formatting edit adds.

use crate::{
    OpenError,
    look::{Border, Borders, CellStyle, Fonts, HAlign, Rgb, StyleMask, VAlign},
    numfmt,
    package::Package,
    word::Marks,
    xml::XmlNode,
};
use std::collections::HashMap;

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const STRICT: &str = "http://purl.oclc.org/ooxml/spreadsheetml/main";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const AS: &str = "http://purl.oclc.org/ooxml/drawingml/main";

fn ns(n: &XmlNode) -> bool {
    matches!(n.namespace.as_deref(), Some(NS | STRICT))
}

fn is(n: &XmlNode, name: &str) -> bool {
    ns(n) && n.local_name == name
}

fn child<'a>(n: &'a XmlNode, name: &str) -> Option<&'a XmlNode> {
    n.children.iter().find(|e| is(e, name))
}

fn attr<'a>(n: &'a XmlNode, name: &str) -> Option<&'a str> {
    n.attr(None, name)
}

fn a_is(n: &XmlNode, name: &str) -> bool {
    matches!(n.namespace.as_deref(), Some(A | AS)) && n.local_name == name
}

fn a_child<'a>(n: &'a XmlNode, name: &str) -> Option<&'a XmlNode> {
    n.children.iter().find(|e| a_is(e, name))
}

fn flag(n: Option<&XmlNode>) -> bool {
    n.is_some_and(|n| !matches!(attr(n, "val"), Some("0" | "false" | "off")))
}

/// Theme colours and font scheme, resolved once per workbook.
#[derive(Debug, Default)]
pub(crate) struct Theme {
    /// Excel's theme indexes: 0 light 1, 1 dark 1, 2 light 2, 3 dark 2,
    /// then accent 1–6, hyperlink and followed hyperlink.
    pub colors: Vec<Option<Rgb>>,
    pub minor_latin: String,
    pub minor_ea: String,
    pub major_latin: String,
    pub major_ea: String,
}

/// Everything the model needs from `styles.xml`, plus the records the writer
/// compares against.
#[derive(Debug)]
pub(crate) struct ReadStyles {
    pub list: Vec<CellStyle>,
    pub formats: Vec<String>,
    pub fonts: Vec<Marks>,
    pub fills: Vec<Option<Rgb>>,
    pub borders: Vec<Borders>,
    pub num_fmts: HashMap<u32, String>,
    pub dxfs: Vec<CellStyle>,
    pub theme: Theme,
    pub digit: f32,
    pub path: Option<String>,
    pub bytes: Vec<u8>,
}

impl Default for ReadStyles {
    fn default() -> Self {
        Self {
            list: vec![CellStyle::default()],
            formats: vec!["General".into()],
            fonts: vec![Marks::default()],
            fills: vec![None],
            borders: vec![Borders::default()],
            num_fmts: HashMap::new(),
            dxfs: Vec::new(),
            theme: Theme::default(),
            digit: 7.0,
            path: None,
            bytes: Vec::new(),
        }
    }
}

pub(crate) fn read(package: &Package, part: &str) -> Result<ReadStyles, OpenError> {
    let theme = read_theme(package, part);
    let Some(path) = package.related_part(part, "styles")? else {
        return Ok(ReadStyles {
            theme,
            ..ReadStyles::default()
        });
    };
    let xml = package.part(&path)?;
    let root = xml.root();
    let mut num_fmts = HashMap::new();
    if let Some(n) = child(root, "numFmts") {
        for f in n.children.iter().filter(|n| is(n, "numFmt")) {
            if let (Some(id), Some(code)) = (attr(f, "numFmtId"), attr(f, "formatCode"))
                && let Ok(id) = id.parse::<u32>()
            {
                num_fmts.insert(id, code.to_owned());
            }
        }
    }
    let fonts: Vec<Marks> = child(root, "fonts")
        .into_iter()
        .flat_map(|n| &n.children)
        .filter(|n| is(n, "font"))
        .map(|n| read_font(n, &theme))
        .collect();
    let fills: Vec<Option<Rgb>> = child(root, "fills")
        .into_iter()
        .flat_map(|n| &n.children)
        .filter(|n| is(n, "fill"))
        .map(|n| read_fill(n, &theme))
        .collect();
    let borders: Vec<Borders> = child(root, "borders")
        .into_iter()
        .flat_map(|n| &n.children)
        .filter(|n| is(n, "border"))
        .map(|n| read_border(n, &theme))
        .collect();
    let base: Vec<Xf> = child(root, "cellStyleXfs")
        .into_iter()
        .flat_map(|n| &n.children)
        .filter(|n| is(n, "xf"))
        .map(read_xf)
        .collect();
    let mut list = Vec::new();
    if let Some(n) = child(root, "cellXfs") {
        for xf in n.children.iter().filter(|n| is(n, "xf")) {
            let xf = read_xf(xf);
            let parent = xf.xf_id.and_then(|i| base.get(i as usize));
            list.push(style_from(&xf, parent, &fonts, &fills, &borders, &num_fmts));
        }
    }
    if list.is_empty() {
        list.push(CellStyle::default());
    }
    let formats = list.iter().map(|s| s.number_format.clone()).collect();
    let dxfs: Vec<CellStyle> = child(root, "dxfs")
        .into_iter()
        .flat_map(|n| &n.children)
        .filter(|n| is(n, "dxf"))
        .map(|n| read_dxf(n, &theme))
        .collect();
    let default = fonts.first().cloned().unwrap_or_default();
    let name = if default.fonts.latin.is_empty() {
        "Calibri".to_owned()
    } else {
        default.fonts.latin.clone()
    };
    let size = default.size.unwrap_or(220) as f32 / 20.0;
    let digit = crate::look::digit_width(&name, size);
    Ok(ReadStyles {
        list,
        formats,
        fonts,
        fills,
        borders,
        num_fmts,
        dxfs,
        theme,
        digit,
        path: Some(path),
        bytes: xml.bytes().to_vec(),
    })
}

fn read_theme(package: &Package, part: &str) -> Theme {
    let mut theme = Theme::default();
    let Ok(Some(path)) = package.related_part(part, "theme") else {
        return theme;
    };
    let Ok(xml) = package.part(&path) else {
        return theme;
    };
    let root = xml.root();
    let elements = root.children.iter().find(|n| a_is(n, "themeElements"));
    let Some(elements) = elements else {
        return theme;
    };
    if let Some(scheme) = a_child(elements, "clrScheme") {
        let mut colors = Vec::new();
        for name in [
            "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5",
            "accent6", "hlink", "folHlink",
        ] {
            colors.push(color_scheme(scheme, name));
        }
        // Excel's `theme` attribute order is lt1, dk1, lt2, dk2, accents.
        if colors.len() >= 4 {
            theme.colors = vec![
                colors[1], colors[0], colors[3], colors[2], colors[4], colors[5], colors[6],
                colors[7], colors[8], colors[9], colors[10], colors[11],
            ];
        }
    }
    if let Some(scheme) = a_child(elements, "fontScheme") {
        if let Some(minor) = a_child(scheme, "minorFont") {
            theme.minor_latin = font_scheme_name(minor, "latin");
            theme.minor_ea = font_scheme_name(minor, "ea");
        }
        if let Some(major) = a_child(scheme, "majorFont") {
            theme.major_latin = font_scheme_name(major, "latin");
            theme.major_ea = font_scheme_name(major, "ea");
        }
    }
    theme
}

fn color_scheme(scheme: &XmlNode, name: &str) -> Option<Rgb> {
    let item = a_child(scheme, name)?;
    if let Some(srgb) = a_child(item, "srgbClr") {
        return hex(attr(srgb, "val").unwrap_or(""));
    }
    if let Some(sys) = a_child(item, "sysClr") {
        return hex(attr(sys, "lastClr")
            .or_else(|| attr(sys, "val"))
            .unwrap_or(""));
    }
    None
}

fn font_scheme_name(font: &XmlNode, tag: &str) -> String {
    a_child(font, tag)
        .and_then(|n| attr(n, "typeface"))
        .unwrap_or("")
        .to_owned()
}

fn read_font(n: &XmlNode, theme: &Theme) -> Marks {
    let size = child(n, "sz")
        .and_then(|n| attr(n, "val"))
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| (s * 20.0).round() as i32);
    let name = child(n, "name")
        .and_then(|n| attr(n, "val"))
        .unwrap_or("")
        .to_owned();
    let (latin, east_asia) = resolve_font(&name, theme);
    Marks {
        bold: flag(child(n, "b")),
        italic: flag(child(n, "i")),
        underline: child(n, "u")
            .is_some_and(|u| !matches!(attr(u, "val"), Some("none" | "0" | "false"))),
        strike: flag(child(n, "strike")),
        link: None,
        fonts: Fonts { latin, east_asia },
        size,
        color: child(n, "color").and_then(|c| read_color(c, theme)),
        highlight: None,
        baseline: Default::default(),
    }
}

fn resolve_font(name: &str, theme: &Theme) -> (String, String) {
    match name.to_ascii_lowercase().as_str() {
        "+mn-lt" => (theme.minor_latin.clone(), theme.minor_ea.clone()),
        "+mj-lt" => (theme.major_latin.clone(), theme.major_ea.clone()),
        "+mn-ea" => (theme.minor_ea.clone(), theme.minor_ea.clone()),
        "+mj-ea" => (theme.major_ea.clone(), theme.major_ea.clone()),
        _ => (name.to_owned(), name.to_owned()),
    }
}

fn read_fill(n: &XmlNode, theme: &Theme) -> Option<Rgb> {
    if let Some(pattern) = child(n, "patternFill") {
        let kind = attr(pattern, "patternType").unwrap_or("none");
        if kind == "gray125" {
            return None;
        }
        if let Some(color) = child(pattern, "fgColor").and_then(|c| read_color(c, theme)) {
            return Some(color);
        }
        if kind == "none" {
            return None;
        }
    }
    if let Some(gradient) = child(n, "gradientFill") {
        return child(gradient, "stop")
            .and_then(|s| child(s, "color"))
            .and_then(|c| read_color(c, theme));
    }
    None
}

fn read_border(n: &XmlNode, theme: &Theme) -> Borders {
    Borders {
        top: read_side(n, "top", theme),
        bottom: read_side(n, "bottom", theme),
        left: read_side(n, "left", theme),
        right: read_side(n, "right", theme),
    }
}

fn read_side(n: &XmlNode, side: &str, theme: &Theme) -> Option<Border> {
    let s = child(n, side)?;
    let style = attr(s, "style")?;
    if style == "none" {
        return None;
    }
    let width = match style {
        "hair" => 5,
        "thin" | "dotted" | "dashed" | "dashDot" | "dashDotDot" => 15,
        "medium" | "mediumDashed" | "mediumDashDot" | "mediumDashDotDot" | "slantDashDot" => 30,
        "thick" => 45,
        "double" => 30,
        _ => 15,
    };
    Some(Border {
        width,
        color: child(s, "color")
            .and_then(|c| read_color(c, theme))
            .unwrap_or(Rgb::BLACK),
        space: 0,
    })
}

pub(crate) fn read_color(n: &XmlNode, theme: &Theme) -> Option<Rgb> {
    let tint = attr(n, "tint")
        .and_then(|t| t.parse::<f32>().ok())
        .unwrap_or(0.0);
    if let Some(rgb) = attr(n, "rgb").and_then(hex) {
        return Some(apply_tint(rgb, tint));
    }
    if let Some(index) = attr(n, "theme").and_then(|s| s.parse::<usize>().ok()) {
        return theme
            .colors
            .get(index)
            .copied()
            .flatten()
            .map(|c| apply_tint(c, tint));
    }
    if let Some(index) = attr(n, "indexed").and_then(|s| s.parse::<u32>().ok()) {
        return numfmt::indexed_color(index).map(|c| apply_tint(c, tint));
    }
    if attr(n, "auto").is_some() {
        return Some(Rgb::BLACK);
    }
    None
}

fn hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    let s = if s.len() == 8 { &s[2..] } else { s };
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok().map(Rgb)
}

pub(crate) fn apply_tint(color: Rgb, tint: f32) -> Rgb {
    if tint == 0.0 || !tint.is_finite() {
        return color;
    }
    let channel = |shift: u32| {
        let c = ((color.0 >> shift) & 0xFF) as f32 / 255.0;
        let out = if tint < 0.0 {
            c * (1.0 + tint)
        } else {
            c * (1.0 - tint) + tint
        };
        (out.clamp(0.0, 1.0) * 255.0).round() as u32
    };
    Rgb((channel(16) << 16) | (channel(8) << 8) | channel(0))
}

#[derive(Clone, Copy, Debug, Default)]
struct Xf {
    font: u32,
    fill: u32,
    border: u32,
    num_fmt: u32,
    xf_id: Option<u32>,
    align: Option<AlignRaw>,
}

#[derive(Clone, Copy, Debug)]
struct AlignRaw {
    h: HAlign,
    v: VAlign,
    wrap: bool,
    shrink: bool,
    indent: u8,
    rotation: i16,
}

fn read_xf(n: &XmlNode) -> Xf {
    let num = |name: &str| {
        attr(n, name)
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
    };
    Xf {
        font: num("fontId"),
        fill: num("fillId"),
        border: num("borderId"),
        num_fmt: num("numFmtId"),
        xf_id: attr(n, "xfId").and_then(|s| s.parse().ok()),
        align: child(n, "alignment").map(|a| AlignRaw {
            h: match attr(a, "horizontal") {
                Some("left") => HAlign::Left,
                Some("center") => HAlign::Center,
                Some("right") => HAlign::Right,
                Some("fill") => HAlign::Fill,
                Some("justify") => HAlign::Justify,
                Some("centerContinuous") => HAlign::CenterAcross,
                Some("distributed") => HAlign::Distributed,
                _ => HAlign::General,
            },
            v: match attr(a, "vertical") {
                Some("center") => VAlign::Center,
                Some("top") => VAlign::Top,
                _ => VAlign::Bottom,
            },
            wrap: attr(a, "wrapText").is_some_and(|s| matches!(s, "1" | "true")),
            shrink: attr(a, "shrinkToFit").is_some_and(|s| matches!(s, "1" | "true")),
            indent: attr(a, "indent")
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(0),
            rotation: attr(a, "textRotation")
                .and_then(|s| s.parse::<i16>().ok())
                .map(|r| match r {
                    255 => 255,
                    91..=180 => -(r - 90),
                    _ => r,
                })
                .unwrap_or(0),
        }),
    }
}

fn style_from(
    xf: &Xf,
    parent: Option<&Xf>,
    fonts: &[Marks],
    fills: &[Option<Rgb>],
    borders: &[Borders],
    num_fmts: &HashMap<u32, String>,
) -> CellStyle {
    let align = xf.align.or_else(|| parent.and_then(|p| p.align));
    let num_fmt = if xf.num_fmt != 0 {
        xf.num_fmt
    } else {
        parent.map_or(0, |p| p.num_fmt)
    };
    CellStyle {
        font: fonts.get(xf.font as usize).cloned().unwrap_or_default(),
        fill: fills.get(xf.fill as usize).copied().flatten(),
        borders: borders.get(xf.border as usize).copied().unwrap_or_default(),
        h_align: align.map_or(HAlign::General, |a| a.h),
        v_align: align.map_or(VAlign::Bottom, |a| a.v),
        wrap: align.is_some_and(|a| a.wrap),
        shrink: align.is_some_and(|a| a.shrink),
        indent: align.map_or(0, |a| a.indent),
        rotation: align.map_or(0, |a| a.rotation),
        number_format: num_fmts
            .get(&num_fmt)
            .cloned()
            .or_else(|| numfmt::builtin_format(num_fmt).map(str::to_owned))
            .unwrap_or_else(|| "General".into()),
        present: StyleMask::all(),
    }
}

// ---------------------------------------------------------------- writing

/// Adds `cellXfs` entries for the styles a save uses. Existing records are
/// compared field by field and reused; only new records are inserted.
pub(crate) struct StyleWriter {
    raw: String,
    list: Vec<CellStyle>,
    fonts: Vec<Marks>,
    fills: Vec<Option<Rgb>>,
    borders: Vec<Borders>,
    num_fmts: HashMap<u32, String>,
    default_font: Marks,
    next_num_fmt: u32,
    dirty: bool,
    prefix: String,
}

impl StyleWriter {
    pub fn new(styles: &ReadStyles) -> Self {
        let list = styles.list.clone();
        let raw = String::from_utf8(styles.bytes.clone()).unwrap_or_default();
        let prefix = root_prefix(&raw);
        Self {
            raw,
            list,
            fonts: styles.fonts.clone(),
            fills: styles.fills.clone(),
            borders: styles.borders.clone(),
            num_fmts: styles.num_fmts.clone(),
            default_font: styles.fonts.first().cloned().unwrap_or_default(),
            next_num_fmt: styles
                .num_fmts
                .keys()
                .copied()
                .max()
                .unwrap_or(163)
                .max(163)
                + 1,
            dirty: false,
            prefix,
        }
    }

    /// The `cellXfs` index for `style`, adding records as needed.
    pub fn ensure(&mut self, style: &CellStyle) -> u32 {
        if let Some(i) = self.list.iter().position(|s| s == style) {
            return i as u32;
        }
        let font = self.font_id(&style.font);
        let fill = self.fill_id(style.fill);
        let border = self.border_id(&style.borders);
        let num_fmt = self.num_fmt_id(&style.number_format);
        let q = &self.prefix;
        let mut xf = format!(
            "<{q}xf numFmtId=\"{num_fmt}\" fontId=\"{font}\" fillId=\"{fill}\" borderId=\"{border}\" xfId=\"0\" applyNumberFormat=\"1\" applyFont=\"1\" applyFill=\"1\" applyBorder=\"1\""
        );
        let align = alignment_xml(style, &self.prefix);
        if align.is_empty() {
            xf.push_str("/>");
        } else {
            xf.push_str(" applyAlignment=\"1\">");
            xf.push_str(&align);
            xf.push_str(&format!("</{q}xf>"));
        }
        self.insert_before("cellXfs", &xf);
        self.list.push(style.clone());
        self.dirty = true;
        (self.list.len() - 1) as u32
    }

    pub fn finish(self) -> Option<Vec<u8>> {
        if !self.dirty {
            return None;
        }
        Some(self.raw.into_bytes())
    }

    fn font_id(&mut self, font: &Marks) -> u32 {
        let mut font = font.clone();
        if font.fonts.latin.is_empty() {
            font.fonts = self.default_font.fonts.clone();
        }
        if font.size.is_none() {
            font.size = self.default_font.size;
        }
        if font.color.is_none() {
            font.color = self.default_font.color;
        }
        if let Some(i) = self.fonts.iter().position(|f| *f == font) {
            return i as u32;
        }
        let q = &self.prefix;
        let name = quick_xml::escape::escape(font.fonts.latin.as_str());
        let size = font.size.unwrap_or(220) as f32 / 20.0;
        let mut s = format!("<{q}font>");
        if font.bold {
            s.push_str("<b/>");
        }
        if font.italic {
            s.push_str("<i/>");
        }
        if font.underline {
            s.push_str("<u/>");
        }
        if font.strike {
            s.push_str("<strike/>");
        }
        s.push_str(&format!(
            "<sz val=\"{size}\"/><color rgb=\"FF{:06X}\"/><name val=\"{name}\"/></{q}font>",
            font.color.unwrap_or(Rgb::BLACK).0
        ));
        self.insert_before("fonts", &s);
        self.fonts.push(font);
        (self.fonts.len() - 1) as u32
    }

    fn fill_id(&mut self, fill: Option<Rgb>) -> u32 {
        if let Some(i) = self.fills.iter().position(|f| *f == fill) {
            return i as u32;
        }
        let q = &self.prefix;
        let s = match fill {
            Some(color) => format!(
                "<{q}fill><{q}patternFill patternType=\"solid\"><{q}fgColor rgb=\"FF{:06X}\"/><{q}bgColor indexed=\"64\"/></{q}patternFill></{q}fill>",
                color.0
            ),
            None => format!("<{q}fill><{q}patternFill patternType=\"none\"/></{q}fill>"),
        };
        self.insert_before("fills", &s);
        self.fills.push(fill);
        (self.fills.len() - 1) as u32
    }

    fn border_id(&mut self, borders: &Borders) -> u32 {
        if let Some(i) = self.borders.iter().position(|b| b == borders) {
            return i as u32;
        }
        let q = &self.prefix;
        let side = |name: &str, b: Option<Border>| match b {
            Some(b) => format!(
                "<{q}{name} style=\"{}\"><{q}color rgb=\"FF{:06X}\"/></{q}{name}>",
                border_style(b.width),
                b.color.0
            ),
            None => format!("<{q}{name}/>"),
        };
        let s = format!(
            "<{q}border>{}{}{}{}<{q}diagonal/></{q}border>",
            side("left", borders.left),
            side("right", borders.right),
            side("top", borders.top),
            side("bottom", borders.bottom)
        );
        self.insert_before("borders", &s);
        self.borders.push(*borders);
        (self.borders.len() - 1) as u32
    }

    fn num_fmt_id(&mut self, code: &str) -> u32 {
        if code.is_empty() || code.eq_ignore_ascii_case("general") {
            return 0;
        }
        for id in 0..=163 {
            if numfmt::builtin_format(id).is_some_and(|c| c == code) {
                return id;
            }
        }
        if let Some((id, _)) = self.num_fmts.iter().find(|(_, c)| c.as_str() == code) {
            return *id;
        }
        let id = self.next_num_fmt;
        self.next_num_fmt += 1;
        let q = &self.prefix;
        let escaped = quick_xml::escape::escape(code);
        let record = format!("<{q}numFmt numFmtId=\"{id}\" formatCode=\"{escaped}\"/>");
        if self.raw.contains(&format!("<{q}numFmts")) {
            self.insert_before("numFmts", &record);
        } else {
            let block = format!("<{q}numFmts count=\"1\">{record}</{q}numFmts>");
            self.insert_before("fonts", &block);
        }
        self.num_fmts.insert(id, code.to_owned());
        id
    }

    fn insert_before(&mut self, element: &str, record: &str) {
        let close = format!("</{}", element);
        if let Some(at) = self.raw.rfind(&close) {
            self.raw.insert_str(at, record);
            return;
        }
        // The styles part may have omitted the collection. Insert a complete
        // one before the first element that must follow it, or before the root
        // close.
        let q = &self.prefix;
        let block = format!("<{q}{element}>{record}</{q}{element}>");
        let before = match element {
            "fonts" => [
                "fills",
                "borders",
                "cellStyleXfs",
                "cellXfs",
                "cellStyles",
                "dxfs",
            ]
            .iter()
            .find_map(|name| self.raw.find(&format!("<{q}{name}")))
            .unwrap_or_else(|| self.root_close()),
            "fills" => ["borders", "cellStyleXfs", "cellXfs", "cellStyles", "dxfs"]
                .iter()
                .find_map(|name| self.raw.find(&format!("<{q}{name}")))
                .unwrap_or_else(|| self.root_close()),
            "borders" => ["cellStyleXfs", "cellXfs", "cellStyles", "dxfs"]
                .iter()
                .find_map(|name| self.raw.find(&format!("<{q}{name}")))
                .unwrap_or_else(|| self.root_close()),
            "numFmts" => self
                .raw
                .find(&format!("<{q}fonts"))
                .unwrap_or_else(|| self.root_close()),
            _ => self.root_close(),
        };
        self.raw.insert_str(before, &block);
    }

    fn root_close(&self) -> usize {
        self.raw.rfind("</").unwrap_or(self.raw.len())
    }
}

fn root_prefix(raw: &str) -> String {
    let Some(start) = raw.find('<') else {
        return String::new();
    };
    let rest = &raw[start + 1..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    let name = &rest[..end];
    name.split_once(':')
        .map(|(p, _)| format!("{p}:"))
        .unwrap_or_default()
}

fn alignment_xml(style: &CellStyle, q: &str) -> String {
    if style.h_align == HAlign::General
        && style.v_align == VAlign::Bottom
        && !style.wrap
        && !style.shrink
        && style.indent == 0
        && style.rotation == 0
    {
        return String::new();
    }
    let h = match style.h_align {
        HAlign::General => None,
        HAlign::Left => Some("left"),
        HAlign::Center => Some("center"),
        HAlign::Right => Some("right"),
        HAlign::Fill => Some("fill"),
        HAlign::Justify => Some("justify"),
        HAlign::CenterAcross => Some("centerContinuous"),
        HAlign::Distributed => Some("distributed"),
    };
    let v = match style.v_align {
        VAlign::Bottom => None,
        VAlign::Top => Some("top"),
        VAlign::Center => Some("center"),
    };
    let mut attrs = String::new();
    if let Some(h) = h {
        attrs.push_str(&format!(" horizontal=\"{h}\""));
    }
    if let Some(v) = v {
        attrs.push_str(&format!(" vertical=\"{v}\""));
    }
    if style.wrap {
        attrs.push_str(" wrapText=\"1\"");
    }
    if style.shrink {
        attrs.push_str(" shrinkToFit=\"1\"");
    }
    if style.indent > 0 {
        attrs.push_str(&format!(" indent=\"{}\"", style.indent));
    }
    if style.rotation != 0 {
        attrs.push_str(&format!(" textRotation=\"{}\"", style.rotation));
    }
    format!("<{q}alignment{attrs}/>")
}

fn border_style(width: i32) -> &'static str {
    match width {
        0..=7 => "hair",
        8..=18 => "thin",
        19..=35 => "medium",
        _ => "thick",
    }
}

/// A conditional format's `dxf`, as a partial style.
pub(crate) fn read_dxf(n: &XmlNode, theme: &Theme) -> CellStyle {
    let mut style = CellStyle::partial();
    if let Some(font) = child(n, "font") {
        style.font = read_font(font, theme);
        style.present.font_bold = child(font, "b").is_some();
        style.present.font_italic = child(font, "i").is_some();
        style.present.font_underline = child(font, "u").is_some();
        style.present.font_strike = child(font, "strike").is_some();
        style.present.font_size = child(font, "sz").is_some();
        style.present.font_color = child(font, "color").is_some();
        style.present.font_name = child(font, "name").is_some();
    }
    if let Some(fill) = child(n, "fill") {
        style.fill = read_fill(fill, theme);
        style.present.fill = true;
    }
    if let Some(border) = child(n, "border") {
        style.borders = read_border(border, theme);
        style.present.borders = true;
    }
    if let Some(num) = child(n, "numFmt") {
        if let Some(code) = attr(num, "formatCode") {
            style.number_format = code.to_owned();
            style.present.number_format = true;
        }
        if let Some(id) = attr(num, "numFmtId").and_then(|s| s.parse::<u32>().ok())
            && let Some(code) = numfmt::builtin_format(id)
        {
            style.number_format = code.to_owned();
            style.present.number_format = true;
        }
    }
    if let Some(a) = child(n, "alignment") {
        if let Some(h) = attr(a, "horizontal") {
            style.h_align = match h {
                "left" => HAlign::Left,
                "center" => HAlign::Center,
                "right" => HAlign::Right,
                "fill" => HAlign::Fill,
                "justify" => HAlign::Justify,
                "centerContinuous" => HAlign::CenterAcross,
                "distributed" => HAlign::Distributed,
                _ => HAlign::General,
            };
            style.present.h_align = true;
        }
        if let Some(v) = attr(a, "vertical") {
            style.v_align = match v {
                "center" => VAlign::Center,
                "top" => VAlign::Top,
                _ => VAlign::Bottom,
            };
            style.present.v_align = true;
        }
        if attr(a, "wrapText").is_some() {
            style.wrap = attr(a, "wrapText").is_some_and(|s| matches!(s, "1" | "true"));
            style.present.wrap = true;
        }
    }
    style
}
