//! Effective Word looks: the document defaults, style chains, numbering and
//! theme, resolved into the model's look types.
//!
//! The loader reads the values here; the layout engine never sees a style.

use std::collections::{HashMap, HashSet};

use crate::Marks;
use crate::look::{
    Baseline, Border, Borders, Fonts, LabelSuffix, LineSpacing, ListLook, NumFormat, ParaLook, Rgb,
    TabKind, TabStop, Twips,
};
use crate::xml::XmlNode;

const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const AS: &str = "http://purl.oclc.org/ooxml/drawingml/main";

#[derive(Debug)]
pub(crate) struct Looks<'a> {
    ns: &'a str,
    styles: HashMap<&'a str, &'a XmlNode>,
    abstracts: HashMap<&'a str, &'a XmlNode>,
    numbers: HashMap<&'a str, &'a XmlNode>,
    theme: Option<&'a XmlNode>,
    defaults: Marks,
    default_para: ParaLook,
}

fn child<'a>(node: &'a XmlNode, ns: &str, name: &str) -> Option<&'a XmlNode> {
    node.children.iter().find(|n| n.is(ns, name))
}

fn val<'a>(node: &'a XmlNode, ns: &str, name: &str) -> Option<&'a str> {
    child(node, ns, name)?.attr(Some(ns), "val")
}

fn attr<'a>(node: &'a XmlNode, ns: &str, name: &str) -> Option<&'a str> {
    node.attr(Some(ns), name)
}

fn num(s: Option<&str>) -> Option<i64> {
    s?.trim().parse().ok()
}

fn twips(s: Option<&str>) -> Option<Twips> {
    num(s).map(|n| n.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
}

fn on(node: Option<&XmlNode>, ns: &str) -> bool {
    node.is_some_and(|n| !matches!(attr(n, ns, "val"), Some("0" | "false" | "off")))
}

fn off(node: Option<&XmlNode>, ns: &str) -> bool {
    node.is_some_and(|n| matches!(attr(n, ns, "val"), Some("0" | "false" | "off")))
}

fn hex_rgb(s: &str) -> Option<Rgb> {
    let s = s.trim();
    (s.len() == 6)
        .then(|| u32::from_str_radix(s, 16).ok())
        .flatten()
        .map(Rgb)
}

fn apply_tint(color: Rgb, tint: Option<&str>, shade: Option<&str>) -> Rgb {
    let channel = |c: u32| c as f32;
    let tint = tint
        .and_then(|s| u8::from_str_radix(s, 16).ok())
        .map(|v| v as f32 / 255.0);
    let shade = shade
        .and_then(|s| u8::from_str_radix(s, 16).ok())
        .map(|v| v as f32 / 255.0);
    let map = |c: u32| {
        let mut v = channel(c);
        if let Some(t) = tint {
            v = v + (255.0 - v) * t;
        }
        if let Some(s) = shade {
            v *= s;
        }
        v.round().clamp(0.0, 255.0) as u32
    };
    let c = color.0;
    Rgb((map((c >> 16) & 0xFF) << 16) | (map((c >> 8) & 0xFF) << 8) | map(c & 0xFF))
}

impl<'a> Looks<'a> {
    pub(crate) fn new(
        ns: &'a str,
        styles: Option<&'a XmlNode>,
        numbering: Option<&'a XmlNode>,
        theme: Option<&'a XmlNode>,
    ) -> Self {
        let index = |parent: Option<&'a XmlNode>, tag, attr| {
            parent
                .into_iter()
                .flat_map(|n| &n.children)
                .filter(|n| n.is(ns, tag))
                .filter_map(|n| Some((n.attr(Some(ns), attr)?, n)))
                .collect()
        };
        let styles_map = index(styles.filter(|n| n.is(ns, "styles")), "style", "styleId");
        let mut looks = Self {
            ns,
            styles: styles_map,
            abstracts: index(
                numbering.filter(|n| n.is(ns, "numbering")),
                "abstractNum",
                "abstractNumId",
            ),
            numbers: index(numbering.filter(|n| n.is(ns, "numbering")), "num", "numId"),
            theme,
            defaults: Marks::default(),
            default_para: ParaLook::default(),
        };
        let doc_defaults = styles
            .filter(|n| n.is(ns, "styles"))
            .and_then(|n| child(n, ns, "docDefaults"));
        if let Some(rpr) = doc_defaults
            .and_then(|n| child(n, ns, "rPrDefault"))
            .and_then(|n| child(n, ns, "rPr"))
        {
            looks.defaults = looks.run_marks(Some(rpr), &[], &[]);
        }
        if let Some(ppr) = doc_defaults
            .and_then(|n| child(n, ns, "pPrDefault"))
            .and_then(|n| child(n, ns, "pPr"))
        {
            looks.default_para = looks.para_look(Some(ppr), &[]);
        }
        looks
    }

    pub(crate) fn defaults(&self) -> Marks {
        self.defaults.clone()
    }

    pub(crate) fn style_chain(&self, id: &str) -> Vec<&'a XmlNode> {
        let mut next = Some(id);
        let mut seen = HashSet::new();
        let mut result = Vec::new();
        while let Some(id) = next {
            if !seen.insert(id) || result.len() == 256 {
                break;
            }
            let Some(&style) = self.styles.get(id) else {
                break;
            };
            result.push(style);
            next = val(style, self.ns, "basedOn");
        }
        result
    }

    fn property<'n>(
        &self,
        tag: &'n str,
        direct: Option<&'n XmlNode>,
        chain: &[&'n XmlNode],
    ) -> Option<&'n XmlNode> {
        direct.and_then(|p| child(p, self.ns, tag)).or_else(|| {
            chain
                .iter()
                .find_map(|style| child(child(style, self.ns, "pPr")?, self.ns, tag))
        })
    }

    fn p_attr<'n>(
        &self,
        direct: Option<&'n XmlNode>,
        chain: &[&'n XmlNode],
        tag: &str,
        attr_name: &str,
    ) -> Option<&'n str> {
        std::iter::once(direct)
            .chain(chain.iter().map(|style| child(style, self.ns, "pPr")))
            .flatten()
            .filter_map(|p| child(p, self.ns, tag))
            .find_map(|n| n.attr(Some(self.ns), attr_name))
    }

    fn run_property<'n>(
        &self,
        direct: Option<&'n XmlNode>,
        char_chain: &[&'n XmlNode],
        para_chain: &[&'n XmlNode],
        tag: &'n str,
    ) -> Option<&'n XmlNode> {
        direct.and_then(|p| child(p, self.ns, tag)).or_else(|| {
            char_chain
                .iter()
                .chain(para_chain)
                .find_map(|n| child(child(n, self.ns, "rPr")?, self.ns, tag))
        })
    }

    fn color(&self, node: Option<&XmlNode>) -> Option<Rgb> {
        let n = node?;
        let ns = self.ns;
        if let Some(value) = attr(n, ns, "val").and_then(hex_rgb) {
            return Some(value);
        }
        let theme = attr(n, ns, "themeColor")?;
        let base = self.theme_color(theme)?;
        Some(apply_tint(
            base,
            attr(n, ns, "themeTint"),
            attr(n, ns, "themeShade"),
        ))
    }

    fn theme_color(&self, name: &str) -> Option<Rgb> {
        let scheme = self
            .theme
            .and_then(|n| {
                n.children
                    .iter()
                    .find(|n| n.is(A, "themeElements") || n.is(AS, "themeElements"))
            })
            .and_then(|n| {
                n.children
                    .iter()
                    .find(|n| n.is(A, "clrScheme") || n.is(AS, "clrScheme"))
            })?;
        let slot = match name {
            "dark1" | "dk1" | "text1" => "dk1",
            "light1" | "lt1" | "background1" => "lt1",
            "dark2" | "dk2" | "text2" => "dk2",
            "light2" | "lt2" | "background2" => "lt2",
            "accent1" => "accent1",
            "accent2" => "accent2",
            "accent3" => "accent3",
            "accent4" => "accent4",
            "accent5" => "accent5",
            "accent6" => "accent6",
            "hyperlink" | "hlink" => "hlink",
            "followedHyperlink" | "folHlink" => "folHlink",
            _ => return None,
        };
        let node = scheme.children.iter().find(|n| n.local_name == slot)?;
        node.children.iter().find_map(|c| {
            if c.local_name == "srgbClr" {
                c.attr(None, "val").and_then(hex_rgb)
            } else if c.local_name == "sysClr" {
                c.attr(None, "lastClr").and_then(hex_rgb)
            } else {
                None
            }
        })
    }

    fn theme_font(&self, slot: &str, major: bool) -> Option<String> {
        let scheme = self
            .theme
            .and_then(|n| {
                n.children
                    .iter()
                    .find(|n| n.is(A, "themeElements") || n.is(AS, "themeElements"))
            })
            .and_then(|n| {
                n.children
                    .iter()
                    .find(|n| n.is(A, "fontScheme") || n.is(AS, "fontScheme"))
            })?;
        let font = scheme.children.iter().find(|n| {
            (major && (n.is(A, "majorFont") || n.is(AS, "majorFont")))
                || (!major && (n.is(A, "minorFont") || n.is(AS, "minorFont")))
        })?;
        let node = font.children.iter().find(|n| {
            (slot == "ea" && (n.is(A, "ea") || n.is(AS, "ea")))
                || (slot == "latin" && (n.is(A, "latin") || n.is(AS, "latin")))
        })?;
        let name = node.attr(None, "typeface")?.trim();
        (!name.is_empty()).then(|| name.to_string())
    }

    fn fonts(&self, node: Option<&XmlNode>) -> Fonts {
        let mut fonts = Fonts::default();
        let Some(n) = node else {
            return fonts;
        };
        let ns = self.ns;
        if let Some(v) = attr(n, ns, "ascii").or_else(|| attr(n, ns, "hAnsi")) {
            fonts.latin = v.to_string();
        }
        if let Some(v) = attr(n, ns, "eastAsia") {
            fonts.east_asia = v.to_string();
        }
        if fonts.latin.is_empty() {
            if let Some(slot) = attr(n, ns, "asciiTheme").or_else(|| attr(n, ns, "hAnsiTheme")) {
                let major = slot.starts_with("major");
                fonts.latin = self.theme_font("latin", major).unwrap_or_default();
            }
        }
        if fonts.east_asia.is_empty()
            && let Some(slot) = attr(n, ns, "eastAsiaTheme")
        {
            let major = slot.starts_with("major");
            fonts.east_asia = self.theme_font("ea", major).unwrap_or_default();
        }
        fonts
    }

    pub(crate) fn run_marks(
        &self,
        direct: Option<&XmlNode>,
        char_chain: &[&XmlNode],
        para_chain: &[&XmlNode],
    ) -> Marks {
        let ns = self.ns;
        let get = |tag| self.run_property(direct, char_chain, para_chain, tag);
        let mut marks = self.defaults.clone();
        marks.bold = on(get("b"), ns);
        marks.italic = on(get("i"), ns);
        marks.underline = on(get("u"), ns);
        marks.strike = on(get("strike"), ns);
        marks.fonts = self.fonts(get("rFonts"));
        if let Some(size) = get("sz")
            .and_then(|n| attr(n, ns, "val"))
            .and_then(|s| num(Some(s)))
        {
            marks.size = Some((size * 10).clamp(i32::MIN as i64, i32::MAX as i64) as i32);
        }
        marks.color = self.color(get("color")).or(marks.color);
        marks.highlight = get("highlight")
            .and_then(|n| attr(n, ns, "val"))
            .and_then(highlight);
        marks.baseline = match get("vertAlign").and_then(|n| attr(n, ns, "val")) {
            Some("superscript") => Baseline::Superscript,
            Some("subscript") => Baseline::Subscript,
            _ => Baseline::Normal,
        };
        marks
    }

    pub(crate) fn para_look(&self, direct: Option<&XmlNode>, chain: &[&XmlNode]) -> ParaLook {
        let ns = self.ns;
        let get = |tag| self.property(tag, direct, chain);
        let mut look = self.default_para.clone();
        if let Some(before) = twips(self.p_attr(direct, chain, "spacing", "before")) {
            look.before = before;
        }
        if let Some(after) = twips(self.p_attr(direct, chain, "spacing", "after")) {
            look.after = after;
        }
        if let Some(line) = num(self.p_attr(direct, chain, "spacing", "line")) {
            look.line = match self.p_attr(direct, chain, "spacing", "lineRule") {
                Some("exact") => {
                    LineSpacing::Exact(line.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
                }
                Some("atLeast") => {
                    LineSpacing::AtLeast(line.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
                }
                _ => LineSpacing::Auto(line.clamp(0, u16::MAX as i64) as u16),
            };
        }
        if let Some(v) = twips(self.p_attr(direct, chain, "ind", "left"))
            .or_else(|| twips(self.p_attr(direct, chain, "ind", "start")))
        {
            look.left = v;
        }
        if let Some(v) = twips(self.p_attr(direct, chain, "ind", "right"))
            .or_else(|| twips(self.p_attr(direct, chain, "ind", "end")))
        {
            look.right = v;
        }
        if let Some(v) = twips(self.p_attr(direct, chain, "ind", "firstLine")).filter(|v| *v != 0) {
            look.first_line = v;
        } else if let Some(v) = twips(self.p_attr(direct, chain, "ind", "hanging")) {
            look.first_line = -v;
        } else if let Some(v) = num(self.p_attr(direct, chain, "ind", "firstLineChars")) {
            look.first_line = char_indent(v, look.mark.size);
        } else if let Some(v) = num(self.p_attr(direct, chain, "ind", "hangingChars")) {
            look.first_line = -char_indent(v, look.mark.size);
        }
        look.keep_next = on(get("keepNext"), ns);
        look.keep_lines = on(get("keepLines"), ns);
        look.page_break_before = on(get("pageBreakBefore"), ns);
        look.widow_control = !off(get("widowControl"), ns);
        look.contextual = on(get("contextualSpacing"), ns);
        look.snap_to_grid = on(get("snapToGrid"), ns);
        look.tabs = get("tabs")
            .map(|n| {
                n.children
                    .iter()
                    .filter(|d| d.is(ns, "tab"))
                    .filter_map(|d| {
                        let at = twips(attr(d, ns, "pos"))?;
                        let kind = match attr(d, ns, "val") {
                            Some("center") => TabKind::Center,
                            Some("right" | "end") => TabKind::Right,
                            Some("decimal") => TabKind::Decimal,
                            _ => TabKind::Left,
                        };
                        let leader = match attr(d, ns, "leader") {
                            Some("dot") => Some('.'),
                            Some("hyphen") => Some('-'),
                            Some("underscore") => Some('_'),
                            _ => None,
                        };
                        Some(TabStop { at, kind, leader })
                    })
                    .collect()
            })
            .unwrap_or_default();
        look.shading = get("shd")
            .and_then(|n| attr(n, ns, "fill"))
            .and_then(hex_rgb)
            .filter(|c| *c != Rgb(0xFFFFFF));
        look.borders = borders(get("pBdr"), ns, "top", "left", "bottom", "right");
        look.list = direct.and_then(|direct| self.list(direct, chain));
        if let Some(rpr) = get("rPr") {
            look.mark = self.run_marks(Some(rpr), &[], chain);
        }
        look
    }

    fn list(&self, direct: &XmlNode, chain: &[&XmlNode]) -> Option<ListLook> {
        let ns = self.ns;
        let number_property = |name| {
            std::iter::once(Some(direct))
                .chain(chain.iter().map(|style| child(style, ns, "pPr")))
                .flatten()
                .find_map(|p| val(child(p, ns, "numPr")?, ns, name))
        };
        let list = number_property("numId")?.parse::<u32>().ok()?;
        if list == 0 {
            return None;
        }
        let level = number_property("ilvl")
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(0)
            .min(8);
        let number = self.numbers.get(list.to_string().as_str())?;
        let overridden = number.children.iter().find(|n| {
            n.is(ns, "lvlOverride")
                && attr(n, ns, "ilvl").and_then(|x| x.parse::<u8>().ok()) == Some(level)
        });
        let abstract_id = val(number, ns, "abstractNumId")?;
        let abstract_num = self.abstracts.get(abstract_id)?;
        let level_node = |id: u8| {
            abstract_num.children.iter().find(|n| {
                n.is(ns, "lvl")
                    && attr(n, ns, "ilvl").and_then(|x| x.parse::<u8>().ok()) == Some(id)
            })
        };
        let effective = overridden
            .and_then(|n| child(n, ns, "lvl"))
            .or_else(|| level_node(level))?;
        let mut formats = Vec::new();
        let mut starts = Vec::new();
        for id in 0..=level {
            let n = level_node(id).unwrap_or(effective);
            formats.push(num_format(val(n, ns, "numFmt").unwrap_or("decimal")));
            starts.push(num(val(n, ns, "start")).unwrap_or(1).max(0) as u32);
        }
        let pattern = val(effective, ns, "lvlText").unwrap_or("%1.").to_string();
        let suffix = match val(effective, ns, "suff") {
            Some("space") => LabelSuffix::Space,
            Some("nothing") => LabelSuffix::Nothing,
            _ => LabelSuffix::Tab,
        };
        let marks =
            child(effective, ns, "rPr").map(|rpr| Box::new(self.run_marks(Some(rpr), &[], chain)));
        Some(ListLook {
            list,
            level,
            pattern,
            formats,
            starts,
            marks,
            suffix,
        })
    }

    pub(crate) fn table_look(
        &self,
        table: &XmlNode,
        style_chain: &[&XmlNode],
    ) -> crate::look::TableLook {
        let ns = self.ns;
        let mut look = crate::look::TableLook::default();
        if let Some(grid) = child(table, ns, "tblGrid") {
            look.columns = grid
                .children
                .iter()
                .filter(|n| n.is(ns, "gridCol"))
                .filter_map(|n| twips(attr(n, ns, "w")))
                .collect();
        }
        let props = child(table, ns, "tblPr");
        let mut table_borders = Borders::default();
        let mut inside_h = None;
        let mut inside_v = None;
        if let Some(pr) = props {
            if let Some(b) = child(pr, ns, "tblBorders") {
                table_borders = borders(Some(b), ns, "top", "left", "bottom", "right");
                inside_h = border(child(b, ns, "insideH"), ns);
                inside_v = border(child(b, ns, "insideV"), ns);
            }
            if let Some(m) = child(pr, ns, "tblCellMar") {
                look.padding = [
                    margin(m, ns, "top").unwrap_or(0),
                    margin(m, ns, "right").unwrap_or(108),
                    margin(m, ns, "bottom").unwrap_or(0),
                    margin(m, ns, "left").unwrap_or(108),
                ];
            }
            look.align = match val(pr, ns, "jc") {
                Some("center") => crate::Align::Center,
                Some("right" | "end") => crate::Align::Right,
                _ => crate::Align::Left,
            };
            look.indent = twips(val(pr, ns, "tblInd")).unwrap_or(0);
        }
        for style in style_chain {
            if let Some(pr) = child(style, ns, "tblPr") {
                if table_borders == Borders::default()
                    && let Some(b) = child(pr, ns, "tblBorders")
                {
                    table_borders = borders(Some(b), ns, "top", "left", "bottom", "right");
                    inside_h = border(child(b, ns, "insideH"), ns);
                    inside_v = border(child(b, ns, "insideV"), ns);
                }
                if look.padding == [0; 4]
                    && let Some(m) = child(pr, ns, "tblCellMar")
                {
                    look.padding = [
                        margin(m, ns, "top").unwrap_or(0),
                        margin(m, ns, "right").unwrap_or(108),
                        margin(m, ns, "bottom").unwrap_or(0),
                        margin(m, ns, "left").unwrap_or(108),
                    ];
                }
            }
        }
        look.borders = table_borders;
        look.inside_h = inside_h;
        look.inside_v = inside_v;
        look.header_rows = table
            .children
            .iter()
            .filter(|n| n.is(ns, "tr"))
            .take_while(|tr| {
                child(tr, ns, "trPr")
                    .and_then(|p| child(p, ns, "tblHeader"))
                    .is_some()
            })
            .count();
        look.rows = table
            .children
            .iter()
            .filter(|n| n.is(ns, "tr"))
            .map(|tr| {
                let pr = child(tr, ns, "trPr");
                let height = pr
                    .and_then(|p| child(p, ns, "trHeight"))
                    .and_then(|n| twips(attr(n, ns, "val")));
                height.map(|h| {
                    let exact = pr
                        .and_then(|p| child(p, ns, "trHeight"))
                        .and_then(|n| attr(n, ns, "hRule"))
                        == Some("exact");
                    (h, exact)
                })
            })
            .collect();
        look.cells = table
            .children
            .iter()
            .filter(|n| n.is(ns, "tr"))
            .map(|tr| {
                tr.children
                    .iter()
                    .filter(|n| n.is(ns, "tc"))
                    .map(|tc| {
                        let pr = child(tc, ns, "tcPr");
                        crate::look::CellLook {
                            shading: pr
                                .and_then(|p| child(p, ns, "shd"))
                                .and_then(|n| attr(n, ns, "fill"))
                                .and_then(hex_rgb),
                            borders: pr
                                .and_then(|p| child(p, ns, "tcBorders"))
                                .map(|b| borders(Some(b), ns, "top", "left", "bottom", "right")),
                            v_align: match pr.and_then(|p| val(p, ns, "vAlign")) {
                                Some("center") => crate::look::VAlign::Center,
                                Some("bottom") => crate::look::VAlign::Bottom,
                                _ => crate::look::VAlign::Top,
                            },
                            merged_up: pr.and_then(|p| child(p, ns, "vMerge")).is_some_and(|n| {
                                matches!(attr(n, ns, "val"), None | Some("continue"))
                            }),
                        }
                    })
                    .collect()
            })
            .collect();
        look
    }
}

fn char_indent(hundredths: i64, size: Option<Twips>) -> Twips {
    let size = size.unwrap_or(210) as f32;
    (hundredths as f32 / 100.0 * size).round() as i32
}

fn highlight(name: &str) -> Option<Rgb> {
    Some(Rgb(match name {
        "black" => 0x000000,
        "blue" => 0x0000FF,
        "cyan" => 0x00FFFF,
        "green" => 0x00FF00,
        "magenta" => 0xFF00FF,
        "red" => 0xFF0000,
        "yellow" => 0xFFFF00,
        "white" => 0xFFFFFF,
        "darkBlue" => 0x000080,
        "darkCyan" => 0x008080,
        "darkGreen" => 0x008000,
        "darkMagenta" => 0x800080,
        "darkRed" => 0x800000,
        "darkYellow" => 0x808000,
        "darkGray" => 0x808080,
        "lightGray" => 0xC0C0C0,
        _ => return None,
    }))
}

fn num_format(name: &str) -> NumFormat {
    match name {
        "bullet" => NumFormat::Bullet(String::new()),
        "none" => NumFormat::None,
        "decimalZero" => NumFormat::DecimalZero,
        "lowerLetter" => NumFormat::LowerLetter,
        "upperLetter" => NumFormat::UpperLetter,
        "lowerRoman" => NumFormat::LowerRoman,
        "upperRoman" => NumFormat::UpperRoman,
        "chineseCounting" | "chineseCountingThousand" => NumFormat::Chinese,
        "chineseLegalSimplified" => NumFormat::ChineseLegal,
        "decimalEnclosedCircle" | "decimalEnclosedCircleChinese" => NumFormat::Circled,
        _ => NumFormat::Decimal,
    }
}

fn border(node: Option<&XmlNode>, ns: &str) -> Option<Border> {
    let n = node?;
    if matches!(attr(n, ns, "val"), Some("nil" | "none")) {
        return None;
    }
    Some(Border {
        width: twips(attr(n, ns, "sz")).unwrap_or(0),
        color: attr(n, ns, "color").and_then(hex_rgb).unwrap_or(Rgb::BLACK),
        space: twips(attr(n, ns, "space")).unwrap_or(0),
    })
}

fn margin(node: &XmlNode, ns: &str, name: &str) -> Option<Twips> {
    let n = child(node, ns, name)?;
    twips(n.attr(Some(ns), "w")).or_else(|| twips(n.attr(Some(ns), "val")))
}

fn borders(
    node: Option<&XmlNode>,
    ns: &str,
    top: &str,
    left: &str,
    bottom: &str,
    right: &str,
) -> Borders {
    let Some(n) = node else {
        return Borders::default();
    };
    Borders {
        top: border(child(n, ns, top), ns),
        bottom: border(child(n, ns, bottom), ns),
        left: border(child(n, ns, left), ns),
        right: border(child(n, ns, right), ns),
    }
}
