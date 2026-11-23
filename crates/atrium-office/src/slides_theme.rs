//! DrawingML themes: colour scheme, fonts and the format scheme that
//! `p:style` references resolve through.

use crate::look::{Dash, Fonts, Rgb};
use crate::xml::XmlNode;
use std::collections::HashMap;

pub(crate) const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
pub(crate) const AS: &str = "http://purl.oclc.org/ooxml/drawingml/main";

pub(crate) fn named<'n>(n: &'n XmlNode, ns: &str, name: &str) -> Option<&'n XmlNode> {
    n.children.iter().find(|n| n.is(ns, name))
}

pub(crate) fn attr<'n>(n: &'n XmlNode, key: &str) -> Option<&'n str> {
    n.attr(None, key)
}

pub(crate) fn number(n: Option<&str>) -> f64 {
    n.and_then(|s| s.trim().parse::<f64>().ok()).unwrap_or(0.0)
}

pub(crate) fn integer(n: Option<&str>) -> i64 {
    n.and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0)
}

/// EMU (914400 to the inch) to twips (1440 to the inch).
pub(crate) fn emu_twips(value: i64) -> i32 {
    (value / 635).clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

/// A master's `p:clrMap`, which names the theme entry each semantic colour
/// such as `bg1` or `tx1` means on that master.
#[derive(Clone, Debug)]
pub(crate) struct ColorMap {
    map: HashMap<String, String>,
}

impl Default for ColorMap {
    fn default() -> Self {
        Self {
            map: [
                ("bg1", "lt1"),
                ("tx1", "dk1"),
                ("bg2", "lt2"),
                ("tx2", "dk2"),
                ("accent1", "accent1"),
                ("accent2", "accent2"),
                ("accent3", "accent3"),
                ("accent4", "accent4"),
                ("accent5", "accent5"),
                ("accent6", "accent6"),
                ("hlink", "hlink"),
                ("folHlink", "folHlink"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        }
    }
}

impl ColorMap {
    pub(crate) fn from_master(n: Option<&XmlNode>) -> Self {
        let mut out = Self::default();
        let Some(n) = n else { return out };
        for key in [
            "bg1", "tx1", "bg2", "tx2", "accent1", "accent2", "accent3", "accent4", "accent5",
            "accent6", "hlink", "folHlink",
        ] {
            if let Some(value) = attr(n, key) {
                out.map.insert(key.to_owned(), value.to_owned());
            }
        }
        out
    }

    fn name<'a>(&'a self, name: &'a str) -> &'a str {
        self.map.get(name).map(String::as_str).unwrap_or(name)
    }
}

/// A fill in the theme's format scheme, with `phClr` left for the style
/// reference to fill in.
#[derive(Clone, Debug)]
pub(crate) enum ThemeFill {
    None,
    Solid(Option<Rgb>, u8),
    Gradient {
        stops: Vec<(u16, Option<Rgb>, u8)>,
        angle: i32,
        radial: bool,
    },
}

impl ThemeFill {
    pub(crate) fn resolve(&self, ph: Option<Rgb>) -> crate::look::Fill {
        match self {
            Self::None => crate::look::Fill::None,
            Self::Solid(color, alpha) => {
                crate::look::Fill::Solid(color.or(ph).unwrap_or(Rgb::BLACK), *alpha)
            }
            Self::Gradient {
                stops,
                angle,
                radial,
            } => crate::look::Fill::Gradient {
                stops: stops
                    .iter()
                    .map(|(at, color, alpha)| (*at, color.or(ph).unwrap_or(Rgb::BLACK), *alpha))
                    .collect(),
                angle: *angle,
                radial: *radial,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ThemeLine {
    pub(crate) width: i32,
    pub(crate) color: Option<Rgb>,
    pub(crate) alpha: u8,
    pub(crate) dash: Dash,
}

/// The theme part of a presentation.
#[derive(Clone, Debug)]
pub(crate) struct Theme {
    pub(crate) palette: HashMap<String, Rgb>,
    pub(crate) major: Fonts,
    pub(crate) minor: Fonts,
    fills: Vec<ThemeFill>,
    lines: Vec<ThemeLine>,
    backgrounds: Vec<ThemeFill>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            palette: [
                ("dk1", Rgb(0x000000)),
                ("lt1", Rgb(0xFFFFFF)),
                ("dk2", Rgb(0x1F497D)),
                ("lt2", Rgb(0xEEECE1)),
                ("accent1", Rgb(0x4F81BD)),
                ("accent2", Rgb(0xC0504D)),
                ("accent3", Rgb(0x9BBB59)),
                ("accent4", Rgb(0x8064A2)),
                ("accent5", Rgb(0x4BACC6)),
                ("accent6", Rgb(0xF79646)),
                ("hlink", Rgb(0x0000FF)),
                ("folHlink", Rgb(0x800080)),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
            major: Fonts {
                latin: "Calibri Light".into(),
                east_asia: String::new(),
            },
            minor: Fonts {
                latin: "Calibri".into(),
                east_asia: String::new(),
            },
            fills: vec![ThemeFill::Solid(Some(Rgb(0x4F81BD)), 255)],
            lines: vec![ThemeLine {
                width: 9525 / 635,
                color: Some(Rgb(0x4F81BD)),
                alpha: 255,
                dash: Dash::Solid,
            }],
            backgrounds: vec![ThemeFill::Solid(Some(Rgb::WHITE), 255)],
        }
    }
}

impl Theme {
    pub(crate) fn parse(root: &XmlNode, a: &str) -> Self {
        let mut theme = Self::default();
        let Some(elements) = named(root, a, "themeElements") else {
            return theme;
        };
        if let Some(scheme) = named(elements, a, "clrScheme") {
            for n in &scheme.children {
                let name = n.local_name.clone();
                if let Some(color) = literal_color(n) {
                    theme.palette.insert(name, color);
                }
            }
        }
        let cmap = ColorMap::default();
        if let Some(fonts) = named(elements, a, "fontScheme") {
            for (name, target) in [
                ("majorFont", &mut theme.major),
                ("minorFont", &mut theme.minor),
            ] {
                if let Some(font) = named(fonts, a, name) {
                    if let Some(latin) = named(font, a, "latin") {
                        target.latin = attr(latin, "typeface").unwrap_or_default().to_owned();
                    }
                    if let Some(ea) = named(font, a, "ea") {
                        target.east_asia = attr(ea, "typeface").unwrap_or_default().to_owned();
                    }
                    if target.east_asia.is_empty()
                        && let Some(font) = font
                            .children
                            .iter()
                            .find(|n| n.is(a, "font") && attr(n, "script") == Some("Hans"))
                    {
                        target.east_asia = attr(font, "typeface").unwrap_or_default().to_owned();
                    }
                }
            }
        }
        if let Some(format) = named(elements, a, "fmtScheme") {
            if let Some(list) = named(format, a, "fillStyleLst") {
                theme.fills = list.children.iter().map(|n| theme.fill(n, &cmap)).collect();
            }
            if let Some(list) = named(format, a, "lnStyleLst") {
                theme.lines = list.children.iter().map(|n| theme.line(n, &cmap)).collect();
            }
            if let Some(list) = named(format, a, "bgFillStyleLst") {
                theme.backgrounds = list.children.iter().map(|n| theme.fill(n, &cmap)).collect();
            }
        }
        theme
    }

    pub(crate) fn fill_ref(&self, idx: u32, color: Option<Rgb>) -> crate::look::Fill {
        if idx == 0 {
            return crate::look::Fill::None;
        }
        self.fills
            .get(idx as usize - 1)
            .map(|fill| fill.resolve(color))
            .unwrap_or_else(|| crate::look::Fill::Solid(color.unwrap_or(Rgb::BLACK), 255))
    }

    pub(crate) fn line_ref(&self, idx: u32, color: Option<Rgb>) -> Option<crate::look::Stroke> {
        if idx == 0 {
            return None;
        }
        let line = self.lines.get(idx as usize - 1)?;
        Some(crate::look::Stroke {
            width: line.width,
            color: color.or(line.color).unwrap_or(Rgb::BLACK),
            alpha: line.alpha,
            dash: line.dash,
            head: Default::default(),
            tail: Default::default(),
        })
    }

    pub(crate) fn background_ref(&self, idx: u32, color: Option<Rgb>) -> crate::look::Fill {
        let idx = idx.saturating_sub(1000).max(1);
        self.backgrounds
            .get(idx as usize - 1)
            .or_else(|| self.fills.get(idx as usize - 1))
            .map(|fill| fill.resolve(color))
            .unwrap_or_else(|| crate::look::Fill::Solid(color.unwrap_or(Rgb::WHITE), 255))
    }

    fn fill(&self, n: &XmlNode, cmap: &ColorMap) -> ThemeFill {
        match n.local_name.as_str() {
            "noFill" => ThemeFill::None,
            "solidFill" => {
                let (color, alpha) = n
                    .children
                    .first()
                    .and_then(|c| color(c, self, cmap, None))
                    .map(|(c, a)| (Some(c), a))
                    .unwrap_or((None, 255));
                ThemeFill::Solid(color, alpha)
            }
            "gradFill" => {
                let mut stops = Vec::new();
                if let Some(list) = named(n, A, "gsLst").or_else(|| named(n, AS, "gsLst")) {
                    for stop in &list.children {
                        let at = attr(stop, "pos")
                            .and_then(|v| v.parse::<u16>().ok())
                            .unwrap_or(0);
                        if let Some(child) = stop.children.first()
                            && let Some((color, alpha)) = color(child, self, cmap, None)
                        {
                            stops.push((at, Some(color), alpha));
                        }
                    }
                }
                let angle = named(n, A, "lin")
                    .or_else(|| named(n, AS, "lin"))
                    .and_then(|l| attr(l, "ang"))
                    .map_or(0, |v| integer(Some(v)) / 60000) as i32;
                let radial = named(n, A, "path")
                    .or_else(|| named(n, AS, "path"))
                    .is_some();
                ThemeFill::Gradient {
                    stops,
                    angle,
                    radial,
                }
            }
            _ => ThemeFill::None,
        }
    }

    fn line(&self, n: &XmlNode, cmap: &ColorMap) -> ThemeLine {
        let width = emu_twips(integer(attr(n, "w").or(Some("9525"))));
        let fill = n
            .children
            .iter()
            .find(|c| c.is(A, "solidFill") || c.is(AS, "solidFill"));
        let (color, alpha) = fill
            .and_then(|f| f.children.first())
            .and_then(|c| color(c, self, cmap, None))
            .map(|(c, a)| (Some(c), a))
            .unwrap_or((None, 255));
        let dash = named(n, A, "prstDash")
            .or_else(|| named(n, AS, "prstDash"))
            .map(|d| dash(attr(d, "val")))
            .unwrap_or(Dash::Solid);
        ThemeLine {
            width,
            color,
            alpha,
            dash,
        }
    }
}

fn literal_color(n: &XmlNode) -> Option<Rgb> {
    match n.local_name.as_str() {
        "srgbClr" => attr(n, "val").and_then(hex),
        "sysClr" => attr(n, "lastClr").and_then(hex),
        _ => n.children.iter().find_map(literal_color),
    }
}

/// A DrawingML colour with its opacity and colour transforms applied.
pub(crate) fn color(
    n: &XmlNode,
    theme: &Theme,
    cmap: &ColorMap,
    ph: Option<Rgb>,
) -> Option<(Rgb, u8)> {
    let mut rgb = match n.local_name.as_str() {
        "srgbClr" => hex(attr(n, "val")?)?,
        "sysClr" => attr(n, "lastClr")
            .and_then(hex)
            .or_else(|| attr(n, "val").and_then(system_color))?,
        "schemeClr" => {
            let name = attr(n, "val")?;
            if name == "phClr" {
                ph?
            } else {
                *theme.palette.get(cmap.name(name))?
            }
        }
        "prstClr" => preset_color(attr(n, "val")?)?,
        "scrgbClr" => {
            let pct = |key| number(attr(n, key)) / 100000.0;
            Rgb(
                ((pct("r").clamp(0.0, 1.0) * 255.0).round() as u32 * 0x10000)
                    | ((pct("g").clamp(0.0, 1.0) * 255.0).round() as u32 * 0x100)
                    | (pct("b").clamp(0.0, 1.0) * 255.0).round() as u32,
            )
        }
        _ => return None,
    };
    let mut alpha = 255u8;
    for child in &n.children {
        let value = number(attr(child, "val")) / 100000.0;
        match child.local_name.as_str() {
            "alpha" => alpha = (value.clamp(0.0, 1.0) * 255.0).round() as u8,
            "tint" => rgb = mix(rgb, Rgb::WHITE, (1.0 - value).clamp(0.0, 1.0) as f32),
            "shade" => rgb = mix(rgb, Rgb::BLACK, (1.0 - value).clamp(0.0, 1.0) as f32),
            "lumMod" => rgb = with_lightness(rgb, lightness(rgb) * value as f32),
            "lumOff" => rgb = with_lightness(rgb, lightness(rgb) + value as f32),
            _ => {}
        }
    }
    Some((rgb, alpha))
}

pub(crate) fn dash(name: Option<&str>) -> Dash {
    match name {
        Some("dot" | "sysDot") => Dash::Dot,
        Some("dash" | "sysDash" | "lgDash") => Dash::Dash,
        Some("dashDot" | "sysDashDot" | "lgDashDot" | "sysDashDotDot" | "lgDashDotDot") => {
            Dash::DashDot
        }
        _ => Dash::Solid,
    }
}

pub(crate) fn hex(value: &str) -> Option<Rgb> {
    let value = value.trim().trim_start_matches('#');
    (value.len() == 6)
        .then(|| u32::from_str_radix(value, 16).ok())
        .flatten()
        .map(Rgb)
}

fn system_color(name: &str) -> Option<Rgb> {
    Some(match name {
        "windowText" => Rgb::BLACK,
        "window" => Rgb::WHITE,
        "highlight" => Rgb(0x0078D7),
        _ => return None,
    })
}

fn preset_color(name: &str) -> Option<Rgb> {
    Some(match name {
        "black" => Rgb::BLACK,
        "white" => Rgb::WHITE,
        "red" => Rgb(0xFF0000),
        "green" => Rgb(0x008000),
        "blue" => Rgb(0x0000FF),
        "yellow" => Rgb(0xFFFF00),
        "gray" | "grey" => Rgb(0x808080),
        "orange" => Rgb(0xFFA500),
        "purple" => Rgb(0x800080),
        _ => return None,
    })
}

fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    let lerp = |x: u32, y: u32| {
        (x as f32 + (y as f32 - x as f32) * amount)
            .round()
            .clamp(0.0, 255.0) as u32
    };
    Rgb(lerp((a.0 >> 16) & 255, (b.0 >> 16) & 255) << 16
        | lerp((a.0 >> 8) & 255, (b.0 >> 8) & 255) << 8
        | lerp(a.0 & 255, b.0 & 255))
}

fn lightness(c: Rgb) -> f32 {
    let (r, g, b) = rgb_parts(c);
    (r.max(g).max(b) + r.min(g).min(b)) / 2.0
}

fn with_lightness(c: Rgb, value: f32) -> Rgb {
    let (r, g, b) = rgb_parts(c);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = value.clamp(0.0, 1.0);
    if max == min {
        let v = (l * 255.0).round() as u32;
        return Rgb((v * 0x10000) | (v * 0x100) | v);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let hue = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    let channel = |t: f32| {
        let t = if t < 0.0 {
            t + 1.0
        } else if t > 1.0 {
            t - 1.0
        } else {
            t
        };
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u32
    };
    Rgb(channel(hue + 1.0 / 3.0) << 16 | channel(hue) << 8 | channel(hue - 1.0 / 3.0))
}

fn rgb_parts(c: Rgb) -> (f32, f32, f32) {
    (
        ((c.0 >> 16) & 255) as f32 / 255.0,
        ((c.0 >> 8) & 255) as f32 / 255.0,
        (c.0 & 255) as f32 / 255.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::XmlPart;

    fn parse(xml: &str) -> XmlPart {
        XmlPart::parse(xml.as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn theme_colours_apply_tint_shade_and_alpha() {
        let part = parse(&format!(
            r#"<a:theme xmlns:a="{A}"><a:themeElements><a:clrScheme name="t"><a:dk1><a:srgbClr val="102030"/></a:dk1><a:lt1><a:srgbClr val="FFFFFF"/></a:lt1><a:accent1><a:srgbClr val="FF0000"/></a:accent1></a:clrScheme></a:themeElements></a:theme>"#
        ));
        let theme = Theme::parse(part.root(), A);
        let color_part = parse(&format!(
            r#"<a:schemeClr xmlns:a="{A}" val="accent1"><a:lumMod val="50000"/><a:alpha val="50000"/></a:schemeClr>"#
        ));
        let (color, alpha) = color(color_part.root(), &theme, &ColorMap::default(), None).unwrap();
        assert_eq!(color, Rgb(0x800000));
        assert_eq!(alpha, 128);
    }
}
