//! DrawingML text and shape properties, resolved against a theme.

use crate::look::{
    Arrow, Baseline, Dash, Fill, LabelSuffix, LineSpacing, ListLook, NumFormat, ParaLook, PathCmd,
    Shadow, Stroke, TWIPS_PER_PT, TabKind, TabStop, VAlign, VText,
};
use crate::slides_theme::{A, AS, ColorMap, Theme, attr, color, dash, emu_twips, integer, named};
use crate::xml::XmlNode;
use crate::{Align, Marks};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub(crate) struct StyleState {
    pub(crate) marks: Marks,
    pub(crate) para: ParaLook,
    pub(crate) align: Align,
}

impl StyleState {
    pub(crate) fn new(marks: Marks) -> Self {
        Self {
            marks,
            para: ParaLook::default(),
            align: Align::Left,
        }
    }

    pub(crate) fn apply_lst(
        &mut self,
        list: &XmlNode,
        level: u8,
        theme: &Theme,
        cmap: &ColorMap,
        list_id: u32,
    ) {
        let name = format!("lvl{}pPr", level.saturating_add(1));
        let chosen = named(list, A, &name)
            .or_else(|| named(list, AS, &name))
            .or_else(|| named(list, A, "lvl1pPr"))
            .or_else(|| named(list, AS, "lvl1pPr"));
        if let Some(pr) = chosen {
            self.apply_para(pr, theme, cmap, list_id, level);
        }
    }

    pub(crate) fn apply_para(
        &mut self,
        pr: &XmlNode,
        theme: &Theme,
        cmap: &ColorMap,
        list_id: u32,
        level: u8,
    ) {
        if let Some(v) = attr(pr, "marL") {
            self.para.left = emu_twips(integer(Some(v)));
        }
        if let Some(v) = attr(pr, "indent") {
            self.para.first_line = emu_twips(integer(Some(v)));
        }
        if let Some(v) = attr(pr, "algn") {
            self.align = match v {
                "ctr" => Align::Center,
                "r" => Align::Right,
                "just" | "dist" | "thaiDist" => Align::Justify,
                _ => Align::Left,
            };
        }
        for child in &pr.children {
            match child.local_name.as_str() {
                "lnSpc" => {
                    if let Some(spacing) = spacing(child) {
                        self.para.line = spacing;
                    }
                }
                "spcBef" => {
                    self.para.before = spacing(child).map_or(0, |s| match s {
                        LineSpacing::AtLeast(v) | LineSpacing::Exact(v) => v,
                        LineSpacing::Auto(n) => n as i32 * 12,
                    })
                }
                "spcAft" => {
                    self.para.after = spacing(child).map_or(0, |s| match s {
                        LineSpacing::AtLeast(v) | LineSpacing::Exact(v) => v,
                        LineSpacing::Auto(n) => n as i32 * 12,
                    })
                }
                "defRPr" => self.marks = marks(child, &self.marks, theme, cmap),
                "buNone" => self.para.list = None,
                "buChar" => {
                    let mark = attr(child, "char").unwrap_or("•").to_owned();
                    self.bullet(list_id, level, NumFormat::Bullet(mark.clone()), mark, None);
                }
                "buAutoNum" => {
                    let (format, pattern, suffix) = numbered(attr(child, "type"), child);
                    self.bullet(list_id, level, format, pattern, Some(suffix));
                }
                "tabLst" => {
                    self.para.tabs = child
                        .children
                        .iter()
                        .filter(|n| n.is(A, "tab") || n.is(AS, "tab"))
                        .map(|n| TabStop {
                            at: emu_twips(integer(attr(n, "pos"))),
                            kind: match attr(n, "algn") {
                                Some("ctr") => TabKind::Center,
                                Some("r") => TabKind::Right,
                                Some("dec") => TabKind::Decimal,
                                _ => TabKind::Left,
                            },
                            leader: attr(n, "leader").and_then(|l| match l {
                                "dot" => Some('.'),
                                "hyphen" => Some('-'),
                                "underscore" => Some('_'),
                                _ => None,
                            }),
                        })
                        .collect();
                }
                _ => {}
            }
        }
    }

    fn bullet(
        &mut self,
        list_id: u32,
        level: u8,
        format: NumFormat,
        pattern: String,
        suffix: Option<LabelSuffix>,
    ) {
        let formats = if let Some(list) = &self.para.list {
            let mut formats = list.formats.clone();
            formats.resize(level as usize + 1, NumFormat::Decimal);
            formats[level as usize] = format;
            formats
        } else {
            vec![format; level as usize + 1]
        };
        let mut starts = self
            .para
            .list
            .as_ref()
            .map(|l| l.starts.clone())
            .unwrap_or_default();
        starts.resize(level as usize + 1, 1);
        self.para.list = Some(ListLook {
            list: list_id,
            level,
            pattern,
            formats,
            starts,
            marks: None,
            suffix: suffix.unwrap_or(LabelSuffix::Tab),
        });
    }
}

fn spacing(n: &XmlNode) -> Option<LineSpacing> {
    for child in &n.children {
        match child.local_name.as_str() {
            "spcPts" => {
                let value = integer(attr(child, "val"));
                return Some(if value < 0 {
                    LineSpacing::Exact((value / 100 * TWIPS_PER_PT as i64) as i32)
                } else {
                    LineSpacing::AtLeast((value / 100 * TWIPS_PER_PT as i64) as i32)
                });
            }
            "spcPct" => {
                let value = integer(attr(child, "val"));
                return Some(LineSpacing::Auto(
                    ((value as f64 / 100000.0) * 240.0)
                        .round()
                        .clamp(0.0, 65535.0) as u16,
                ));
            }
            _ => {}
        }
    }
    None
}

fn numbered(kind: Option<&str>, n: &XmlNode) -> (NumFormat, String, LabelSuffix) {
    let start = integer(attr(n, "startAt")).max(1);
    let (format, pattern, suffix) = match kind.unwrap_or("arabicPeriod") {
        "arabicPlain" => (NumFormat::Decimal, "%1".into(), LabelSuffix::Nothing),
        "arabicParenR" => (NumFormat::Decimal, "%1)".into(), LabelSuffix::Space),
        "arabicParenBoth" => (NumFormat::Decimal, "(%1)".into(), LabelSuffix::Space),
        "romanLcPeriod" => (NumFormat::LowerRoman, "%1.".into(), LabelSuffix::Tab),
        "romanUcPeriod" => (NumFormat::UpperRoman, "%1.".into(), LabelSuffix::Tab),
        "alphaLcPeriod" => (NumFormat::LowerLetter, "%1.".into(), LabelSuffix::Tab),
        "alphaUcPeriod" => (NumFormat::UpperLetter, "%1.".into(), LabelSuffix::Tab),
        "ea1ChsPeriod" => (NumFormat::Chinese, "%1.".into(), LabelSuffix::Tab),
        "circleNumDbPlain" => (NumFormat::Circled, "%1".into(), LabelSuffix::Tab),
        _ => (NumFormat::Decimal, "%1.".into(), LabelSuffix::Tab),
    };
    let _ = start;
    (format, pattern, suffix)
}

pub(crate) fn marks(r: &XmlNode, base: &Marks, theme: &Theme, cmap: &ColorMap) -> Marks {
    let mut out = base.clone();
    let yes = |value: Option<&str>| matches!(value, Some("1" | "true" | "on"));
    if let Some(value) = attr(r, "b") {
        out.bold = yes(Some(value));
    }
    if let Some(value) = attr(r, "i") {
        out.italic = yes(Some(value));
    }
    if let Some(value) = attr(r, "u") {
        out.underline = value != "none";
    }
    if let Some(value) = attr(r, "strike") {
        out.strike = matches!(value, "sngStrike" | "dblStrike");
    }
    if let Some(value) = attr(r, "sz").and_then(|v| v.parse::<i32>().ok()) {
        out.size = Some(value * TWIPS_PER_PT / 100);
    }
    if let Some(value) = attr(r, "baseline").and_then(|v| v.parse::<i32>().ok()) {
        out.baseline = if value > 0 {
            Baseline::Superscript
        } else if value < 0 {
            Baseline::Subscript
        } else {
            Baseline::Normal
        };
    }
    for child in &r.children {
        match child.local_name.as_str() {
            "solidFill" => {
                if let Some((c, _)) = child
                    .children
                    .first()
                    .and_then(|c| color(c, theme, cmap, None))
                {
                    out.color = Some(c);
                }
            }
            "highlight" => {
                if let Some((c, _)) = child
                    .children
                    .first()
                    .and_then(|c| color(c, theme, cmap, None))
                {
                    out.highlight = Some(c);
                }
            }
            "latin" => {
                if let Some(v) = attr(child, "typeface") {
                    out.fonts.latin = v.to_owned();
                }
            }
            "ea" => {
                if let Some(v) = attr(child, "typeface") {
                    out.fonts.east_asia = v.to_owned();
                }
            }
            "cs" => out.fonts.latin = attr(child, "typeface").unwrap_or("").to_owned(),
            _ => {}
        }
    }
    out
}

pub(crate) fn direct_marks(
    props: Option<&XmlNode>,
    base: &Marks,
    theme: &Theme,
    cmap: &ColorMap,
) -> Marks {
    props.map_or_else(|| base.clone(), |n| marks(n, base, theme, cmap))
}

pub(crate) fn theme_marks(theme: &Theme, major: bool) -> Marks {
    Marks {
        fonts: if major {
            theme.major.clone()
        } else {
            theme.minor.clone()
        },
        ..Marks::default()
    }
}

pub(crate) fn fill(
    node: &XmlNode,
    theme: &Theme,
    cmap: &ColorMap,
    picture: &mut dyn FnMut(&str, [i32; 4]) -> Option<PathBuf>,
) -> Fill {
    match node.local_name.as_str() {
        "noFill" => Fill::None,
        "solidFill" => node
            .children
            .first()
            .and_then(|c| color(c, theme, cmap, None))
            .map_or(Fill::None, |(c, a)| Fill::Solid(c, a)),
        "gradFill" => gradient(node, theme, cmap),
        "blipFill" => blip(node, picture),
        _ => Fill::None,
    }
}

fn gradient(n: &XmlNode, theme: &Theme, cmap: &ColorMap) -> Fill {
    let mut stops = Vec::new();
    if let Some(list) = named(n, A, "gsLst").or_else(|| named(n, AS, "gsLst")) {
        for stop in &list.children {
            let at = attr(stop, "pos")
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(0);
            if let Some(child) = stop.children.first()
                && let Some((c, a)) = color(child, theme, cmap, None)
            {
                stops.push((at, c, a));
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
    Fill::Gradient {
        stops,
        angle,
        radial,
    }
}

pub(crate) fn blip(
    n: &XmlNode,
    picture: &mut dyn FnMut(&str, [i32; 4]) -> Option<PathBuf>,
) -> Fill {
    let embed = named(n, A, "blip")
        .or_else(|| named(n, AS, "blip"))
        .and_then(|b| {
            b.attr(
                Some("http://schemas.openxmlformats.org/officeDocument/2006/relationships"),
                "embed",
            )
            .or_else(|| {
                b.attr(
                    Some("http://purl.oclc.org/ooxml/officeDocument/relationships"),
                    "embed",
                )
            })
        });
    let Some(id) = embed else {
        return Fill::None;
    };
    let mut crop = [0; 4];
    if let Some(src) = named(n, A, "srcRect").or_else(|| named(n, AS, "srcRect")) {
        crop = ["t", "r", "b", "l"].map(|side| integer(attr(src, side)) as i32);
    }
    picture(id, crop).map_or(Fill::None, |path| Fill::Picture { path, crop })
}

pub(crate) fn stroke(ln: &XmlNode, theme: &Theme, cmap: &ColorMap) -> Option<Stroke> {
    let fill = ln
        .children
        .iter()
        .find(|c| c.is(A, "solidFill") || c.is(AS, "solidFill"));
    let (color, alpha) = fill
        .and_then(|f| f.children.first())
        .and_then(|c| color(c, theme, cmap, None))
        .map(|(c, a)| (c, a))
        .unwrap_or((crate::look::Rgb::BLACK, 255));
    let has_paint = fill.is_some();
    let width = attr(ln, "w")
        .map(|v| emu_twips(integer(Some(v))))
        .unwrap_or(12700 / 635);
    if !has_paint && ln.children.is_empty() && attr(ln, "w").is_none() {
        return None;
    }
    Some(Stroke {
        width,
        color,
        alpha,
        dash: named(ln, A, "prstDash")
            .or_else(|| named(ln, AS, "prstDash"))
            .map(|d| dash(attr(d, "val")))
            .unwrap_or(Dash::Solid),
        head: arrow(named(ln, A, "headEnd").or_else(|| named(ln, AS, "headEnd"))),
        tail: arrow(named(ln, A, "tailEnd").or_else(|| named(ln, AS, "tailEnd"))),
    })
}

fn arrow(n: Option<&XmlNode>) -> Arrow {
    match n.and_then(|n| attr(n, "type")) {
        Some("triangle") => Arrow::Triangle,
        Some("stealth") => Arrow::Stealth,
        Some("oval") => Arrow::Oval,
        Some("diamond") => Arrow::Diamond,
        Some("arrow") => Arrow::Open,
        _ => Arrow::None,
    }
}

pub(crate) fn shadow(sp_pr: &XmlNode, theme: &Theme, cmap: &ColorMap) -> Option<Shadow> {
    let effects = named(sp_pr, A, "effectLst").or_else(|| named(sp_pr, AS, "effectLst"))?;
    let n = named(effects, A, "outerShdw").or_else(|| named(effects, AS, "outerShdw"))?;
    let (color, alpha) = n
        .children
        .first()
        .and_then(|c| color(c, theme, cmap, None))
        .unwrap_or((crate::look::Rgb::BLACK, 255));
    Some(Shadow {
        color,
        alpha,
        blur: emu_twips(integer(attr(n, "blurRad"))),
        distance: emu_twips(integer(attr(n, "dist"))),
        direction: (integer(attr(n, "dir")) / 60000) as i32,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct BodyProps {
    pub(crate) insets: [i32; 4],
    pub(crate) v_align: VAlign,
    pub(crate) rotation: i32,
    pub(crate) font_scale: Option<u16>,
    pub(crate) vert: VText,
    pub(crate) auto_grow: bool,
}

pub(crate) fn body(body: Option<&XmlNode>) -> BodyProps {
    let mut out = BodyProps {
        insets: [0; 4],
        v_align: VAlign::Top,
        rotation: 0,
        font_scale: None,
        vert: VText::Horizontal,
        auto_grow: false,
    };
    let Some(n) = body else { return out };
    for (i, key) in ["tIns", "rIns", "bIns", "lIns"].into_iter().enumerate() {
        if let Some(value) = attr(n, key) {
            out.insets[i] = emu_twips(integer(Some(value)));
        }
    }
    out.v_align = match attr(n, "anchor") {
        Some("ctr") => VAlign::Center,
        Some("b") => VAlign::Bottom,
        _ => VAlign::Top,
    };
    out.rotation = (integer(attr(n, "rot")) / 60000) as i32;
    out.vert = match attr(n, "vert") {
        Some("vert") => VText::Vertical,
        Some("vert270") => VText::Vertical270,
        Some("wordArtVert" | "wordArtVertRtl") => VText::WordArtVertical,
        _ => VText::Horizontal,
    };
    for child in &n.children {
        match child.local_name.as_str() {
            "normAutofit" => {
                out.font_scale = attr(child, "fontScale")
                    .and_then(|v| v.parse::<u32>().ok())
                    .map(|v| v.min(u16::MAX as u32) as u16);
            }
            "spAutoFit" => out.auto_grow = true,
            _ => {}
        }
    }
    out
}

/// Reads a shape's path from `prstGeom` or `custGeom`; lengths are in twips.
pub(crate) fn path(geom: Option<&XmlNode>, w: i32, h: i32) -> Vec<PathCmd> {
    let Some(geom) = geom else { return Vec::new() };
    if geom.local_name == "custGeom" || geom.local_name == "custGeom" {
        custom(geom, w, h)
    } else if geom.local_name == "prstGeom" {
        let preset = attr(geom, "prst").unwrap_or("rect");
        let av = named(geom, A, "avLst")
            .or_else(|| named(geom, AS, "avLst"))
            .map(adjustments)
            .unwrap_or_default();
        super::slides_geom::preset(preset, w, h, &av)
    } else {
        Vec::new()
    }
}

fn adjustments(list: &XmlNode) -> Vec<(String, i32)> {
    list.children
        .iter()
        .filter(|n| n.local_name == "gd")
        .filter_map(|n| {
            Some((
                attr(n, "name")?.to_owned(),
                attr(n, "fmla")?.trim_start_matches("val ").parse().ok()?,
            ))
        })
        .collect()
}

fn custom(geom: &XmlNode, w: i32, h: i32) -> Vec<PathCmd> {
    let Some(list) = named(geom, A, "pathLst").or_else(|| named(geom, AS, "pathLst")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for path in &list.children {
        let pw = integer(attr(path, "w")).max(1) as f64;
        let ph = integer(attr(path, "h")).max(1) as f64;
        let point = |n: Option<&XmlNode>| {
            let n = n?;
            let x = integer(attr(n, "x")) as f64;
            let y = integer(attr(n, "y")) as f64;
            Some((
                (x / pw * w as f64).round() as i32,
                (y / ph * h as f64).round() as i32,
            ))
        };
        for cmd in &path.children {
            match cmd.local_name.as_str() {
                "moveTo" => {
                    if let Some(p) = point(named(cmd, A, "pt").or_else(|| named(cmd, AS, "pt"))) {
                        out.push(PathCmd::Move(p.0, p.1));
                    }
                }
                "lnTo" => {
                    if let Some(p) = point(named(cmd, A, "pt").or_else(|| named(cmd, AS, "pt"))) {
                        out.push(PathCmd::Line(p.0, p.1));
                    }
                }
                "cubicBezTo" => {
                    let pts: Vec<_> = cmd
                        .children
                        .iter()
                        .filter(|n| n.local_name == "pt")
                        .filter_map(|n| point(Some(n)))
                        .collect();
                    if pts.len() == 3 {
                        out.push(PathCmd::Cubic(
                            pts[0].0, pts[0].1, pts[1].0, pts[1].1, pts[2].0, pts[2].1,
                        ));
                    }
                }
                "quadBezTo" => {
                    let pts: Vec<_> = cmd
                        .children
                        .iter()
                        .filter(|n| n.local_name == "pt")
                        .filter_map(|n| point(Some(n)))
                        .collect();
                    if pts.len() == 2 {
                        out.push(PathCmd::Quad(pts[0].0, pts[0].1, pts[1].0, pts[1].1));
                    }
                }
                "close" => out.push(PathCmd::Close),
                _ => {}
            }
        }
    }
    out
}

pub(crate) fn rotation(xfrm: Option<&XmlNode>) -> (i32, bool, bool) {
    let Some(n) = xfrm else {
        return (0, false, false);
    };
    (
        (integer(attr(n, "rot")) / 60000) as i32,
        matches!(attr(n, "flipH"), Some("1" | "true")),
        matches!(attr(n, "flipV"), Some("1" | "true")),
    )
}

pub(crate) fn style_refs(
    style: Option<&XmlNode>,
    theme: &Theme,
    cmap: &ColorMap,
) -> (Option<Fill>, Option<Stroke>) {
    let Some(style) = style else {
        return (None, None);
    };
    let fill_ref = named(style, A, "fillRef").or_else(|| named(style, AS, "fillRef"));
    let line_ref = named(style, A, "lnRef").or_else(|| named(style, AS, "lnRef"));
    let pick = |n: Option<&XmlNode>| {
        let n = n?;
        let idx = integer(attr(n, "idx")).max(0) as u32;
        let color = n
            .children
            .first()
            .and_then(|c| color(c, theme, cmap, None))
            .map(|(c, _)| c);
        Some((idx, color))
    };
    let fill =
        pick(fill_ref).and_then(|(idx, color)| (idx != 0).then(|| theme.fill_ref(idx, color)));
    let line = pick(line_ref).and_then(|(idx, color)| theme.line_ref(idx, color));
    (fill, line)
}
