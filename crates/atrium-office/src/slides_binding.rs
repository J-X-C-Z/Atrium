//! PowerPoint OPC binding for the GPUI-free upstream model.

use crate::look::{Borders, CellLook, Fill, ShapeKind, ShapeLook, TableLook, VAlign};
use crate::package::Package;
use crate::relationships::Rel;
use crate::slides_chart;
use crate::slides_style::{self as style, StyleState};
use crate::slides_theme::{self as theme, A, AS, ColorMap, Theme, attr, emu_twips, integer, named};
use crate::xml::{XmlNode, XmlPart};
use crate::{
    Block, Deck, Inline, Marks, NodeId, OpenError, Para, ParaStyle, SaveError, Shape, Slide, Table,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
    path::PathBuf,
};

const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const PS: &str = "http://purl.oclc.org/ooxml/presentationml/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const RS: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships";
const DGM: &str = "http://schemas.openxmlformats.org/drawingml/2006/diagram";
const DGMS: &str = "http://purl.oclc.org/ooxml/drawingml/diagram";

struct Piece {
    view: Inline,
    raw: Range<usize>,
    text: Option<Range<usize>>,
    props: Option<Range<usize>>,
}

struct Binding {
    part: usize,
    range: Range<usize>,
    pieces: Vec<Piece>,
    prefix: String,
}

pub(crate) struct SlidesBinding {
    parts: Vec<String>,
    binding: HashMap<NodeId, Binding>,
    media: Option<tempfile::TempDir>,
}

fn damaged(why: &str) -> OpenError {
    OpenError::Damaged(why.into())
}

fn points(n: Option<&str>) -> i32 {
    n.and_then(|n| n.parse::<i64>().ok()).map_or(0, |x| {
        (x / 12700).clamp(i32::MIN as i64, i32::MAX as i64) as i32
    })
}

fn emu(n: Option<&str>) -> i32 {
    n.and_then(|n| n.parse::<i64>().ok()).map_or(0, emu_twips)
}

#[derive(Clone, Copy, Debug)]
struct GroupMap {
    ox: f64,
    oy: f64,
    sx: f64,
    sy: f64,
    chx: f64,
    chy: f64,
}

impl GroupMap {
    fn map(self, frame: [i32; 4]) -> [i32; 4] {
        [
            (self.ox + (frame[0] as f64 - self.chx) * self.sx).round() as i32,
            (self.oy + (frame[1] as f64 - self.chy) * self.sy).round() as i32,
            (frame[2] as f64 * self.sx).round() as i32,
            (frame[3] as f64 * self.sy).round() as i32,
        ]
    }
}

struct Part<'a> {
    name: &'a str,
    xml: &'a XmlPart,
    rels: HashMap<String, Rel>,
    index: usize,
}

impl<'a> Part<'a> {
    fn new(package: &Package, name: &'a str, xml: &'a XmlPart, index: usize) -> Self {
        let rels = package
            .relationships(name)
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();
        Self {
            name,
            xml,
            rels,
            index,
        }
    }

    fn rel(&self, kind: &str) -> Option<&Rel> {
        self.rels.values().find(|r| !r.external && r.kind(kind))
    }
}

struct Loader<'a> {
    package: &'a Package,
    theme: Theme,
    cmap: ColorMap,
    a: &'a str,
    r: &'a str,
    styles: TableStyles,
}

impl Loader<'_> {
    fn frame(&self, n: Option<&XmlNode>) -> [i32; 4] {
        let off = n.and_then(|n| named(n, self.a, "off"));
        let ext = n.and_then(|n| named(n, self.a, "ext"));
        [
            points(off.and_then(|n| attr(n, "x"))),
            points(off.and_then(|n| attr(n, "y"))),
            points(ext.and_then(|n| attr(n, "cx"))),
            points(ext.and_then(|n| attr(n, "cy"))),
        ]
    }

    fn fill(&self, n: &XmlNode, part: &Part<'_>, media: &mut Media) -> Fill {
        let package = self.package;
        style::fill(n, &self.theme, &self.cmap, &mut |id, crop| {
            media.path(package, part, id, crop)
        })
    }
}

struct Media {
    dir: Option<tempfile::TempDir>,
    paths: HashMap<(String, String), PathBuf>,
}

impl Media {
    fn new() -> Self {
        Self {
            dir: None,
            paths: HashMap::new(),
        }
    }

    fn path(
        &mut self,
        package: &Package,
        part: &Part<'_>,
        id: &str,
        _crop: [i32; 4],
    ) -> Option<PathBuf> {
        let key = (part.name.to_owned(), id.to_owned());
        if let Some(path) = self.paths.get(&key) {
            return Some(path.clone());
        }
        let rel = part.rels.get(id)?;
        if rel.external || !rel.kind("image") {
            return None;
        }
        let bytes = package.read_part(&rel.target).ok()?;
        let ext = if safe_png(&bytes) {
            "png"
        } else if safe_jpeg(&bytes) {
            "jpg"
        } else {
            return None;
        };
        if self.dir.is_none() {
            self.dir = tempfile::tempdir().ok();
        }
        let dir = self.dir.as_ref()?;
        let path = dir.path().join(format!("image-{}.{ext}", self.paths.len()));
        std::fs::write(&path, bytes).ok()?;
        self.paths.insert(key, path.clone());
        Some(path)
    }
}

fn safe_png(bytes: &[u8]) -> bool {
    if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR") {
        return false;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap_or([0; 4]));
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap_or([0; 4]));
    !bytes.windows(4).any(|w| w == b"acTL")
        && width > 0
        && height > 0
        && width <= 8192
        && height <= 8192
        && u64::from(width) * u64::from(height) <= 16_000_000
}

fn safe_jpeg(bytes: &[u8]) -> bool {
    bytes.len() > 4 && bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Ph {
    idx: Option<u32>,
    ty: String,
}

fn placeholder(n: &XmlNode, p: &str) -> Option<Ph> {
    let ph = named(n, p, "nvSpPr")
        .or_else(|| named(n, p, "nvPicPr"))
        .or_else(|| named(n, p, "nvGraphicFramePr"))
        .and_then(|nv| named(nv, p, "nvPr"))
        .and_then(|nv| named(nv, p, "ph"))?;
    Some(Ph {
        idx: attr(ph, "idx").and_then(|v| v.parse().ok()),
        ty: attr(ph, "type").unwrap_or("obj").to_owned(),
    })
}

fn ph_nodes<'n>(root: &'n XmlNode, p: &str) -> HashMap<(Option<u32>, String), &'n XmlNode> {
    let mut out = HashMap::new();
    if let Some(tree) = named(root, p, "cSld").and_then(|n| named(n, p, "spTree")) {
        collect_ph(tree, p, &mut out);
    }
    out
}

fn collect_ph<'n>(
    parent: &'n XmlNode,
    p: &str,
    out: &mut HashMap<(Option<u32>, String), &'n XmlNode>,
) {
    for n in &parent.children {
        if let Some(ph) = placeholder(n, p) {
            out.entry((ph.idx, ph.ty)).or_insert(n);
        }
        if n.is(p, "grpSp") {
            collect_ph(n, p, out);
        }
    }
}

fn find_ph<'n>(map: &HashMap<(Option<u32>, String), &'n XmlNode>, ph: &Ph) -> Option<&'n XmlNode> {
    if let Some(idx) = ph.idx
        && let Some(n) = map.get(&(Some(idx), ph.ty.clone()))
    {
        return Some(n);
    }
    map.get(&(ph.idx, ph.ty.clone())).copied().or_else(|| {
        map.iter()
            .find(|((idx, _), _)| *idx == ph.idx)
            .map(|(_, n)| *n)
    })
}

fn lst<'a>(n: &'a XmlNode, p: &str) -> Option<&'a XmlNode> {
    named(n, p, "txBody").and_then(|b| named(b, A, "lstStyle").or_else(|| named(b, AS, "lstStyle")))
}

fn shape_body<'n>(n: &'n XmlNode, p: &str) -> Option<&'n XmlNode> {
    named(n, p, "txBody")
}

fn master_txstyles<'n>(master: Option<&'n XmlNode>, p: &str, ph: &Ph) -> Option<&'n XmlNode> {
    let styles = named(master?, p, "txStyles")?;
    let name = if matches!(ph.ty.as_str(), "title" | "ctrTitle") {
        "titleStyle"
    } else if matches!(
        ph.ty.as_str(),
        "body" | "subTitle" | "obj" | "content" | "pic" | "chart" | "tbl"
    ) {
        "bodyStyle"
    } else {
        "otherStyle"
    };
    named(styles, p, name)
}

#[derive(Clone, Copy)]
struct TextStyle<'a> {
    tx: Option<&'a XmlNode>,
    master_lst: Option<&'a XmlNode>,
    layout_lst: Option<&'a XmlNode>,
    major: bool,
}

impl TextStyle<'_> {
    fn state(&self, loader: &Loader<'_>, level: u8, list_id: u32) -> StyleState {
        let mut state = StyleState::new(style::theme_marks(&loader.theme, self.major));
        for list in [self.tx, self.master_lst, self.layout_lst]
            .into_iter()
            .flatten()
        {
            state.apply_lst(list, level, &loader.theme, &loader.cmap, list_id);
        }
        state
    }
}

impl SlidesBinding {
    pub(crate) fn load(package: &Package) -> Result<(Deck, Self), OpenError> {
        let main = package
            .related_part("", "officeDocument")?
            .unwrap_or_else(|| "ppt/presentation.xml".into());
        let doc = package.part(&main)?;
        let root = doc.root();
        let (p, a, r) = if root.is(P, "presentation") {
            (P, A, R)
        } else if root.is(PS, "presentation") {
            (PS, AS, RS)
        } else {
            return Err(damaged("invalid presentation root"));
        };
        let size = named(root, p, "sldSz")
            .map_or((720, 540), |n| (emu(attr(n, "cx")), emu(attr(n, "cy"))));
        let rels: HashMap<_, _> = package
            .relationships(&main)?
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();
        let mut bind = Self {
            parts: Vec::new(),
            binding: HashMap::new(),
            media: None,
        };
        let mut slides = Vec::new();
        let mut next = 1;
        let mut seen = HashSet::new();
        let table_styles_part = rels
            .values()
            .find(|r| !r.external && r.kind("tableStyles"))
            .map(|r| r.target.clone());
        if let Some(ids) = named(root, p, "sldIdLst") {
            for id in ids.children.iter().filter(|child| child.is(p, "sldId")) {
                let rel = id
                    .attr(Some(r), "id")
                    .and_then(|id| rels.get(id))
                    .filter(|r| !r.external && r.kind("slide"))
                    .ok_or_else(|| damaged("invalid slide relationship"))?;
                if !seen.insert(&rel.target) {
                    return Err(damaged("duplicate slide relationship target"));
                }
                let xml = package.part(&rel.target)?;
                let slide_root = xml.root();
                if !slide_root.is(p, "sld") {
                    return Err(damaged("invalid slide root"));
                }
                let hidden = matches!(attr(slide_root, "show"), Some("0" | "false"));
                bind.parts.push(rel.target.clone());
                let index = bind.parts.len() - 1;
                let part = Part::new(package, &rel.target, &xml, index);
                let layout_part = part.rel("slideLayout").map(|r| r.target.clone());
                let layout_xml = layout_part.as_deref().and_then(|n| package.part(n).ok());
                let layout_root = layout_xml.as_ref().map(|x| x.root());
                let master_part = layout_part
                    .as_deref()
                    .and_then(|n| package.relationships(n).ok())
                    .and_then(|rels| {
                        rels.into_iter()
                            .find(|r| !r.external && r.kind("slideMaster"))
                            .map(|r| r.target)
                    });
                let master_xml = master_part.as_deref().and_then(|n| package.part(n).ok());
                let master_root = master_xml.as_ref().map(|x| x.root());
                let (th, cmap) = theme_for(package, master_part.as_deref(), master_root, a);
                let styles = table_styles_part
                    .as_deref()
                    .and_then(|n| package.part(n).ok())
                    .map(|part| TableStyles::parse(part.root(), a, &th, &cmap))
                    .unwrap_or_default();
                let loader = Loader {
                    package,
                    theme: th,
                    cmap,
                    a,
                    r,
                    styles,
                };
                let mut media = Media::new();
                let mut shapes = Vec::new();
                let mut background = None;
                let slide_bg = read_background(slide_root, p, &loader, &part, &mut media);
                let layout_bg = match (layout_root, layout_xml.as_ref(), layout_part.as_deref()) {
                    (Some(root), Some(xml), Some(name)) => {
                        let part = Part::new(package, name, xml, index);
                        read_background(root, p, &loader, &part, &mut media)
                    }
                    _ => None,
                };
                let master_bg = match (master_root, master_xml.as_ref(), master_part.as_deref()) {
                    (Some(root), Some(xml), Some(name)) => {
                        let part = Part::new(package, name, xml, index);
                        read_background(root, p, &loader, &part, &mut media)
                    }
                    _ => None,
                };
                let bg = slide_bg.or(layout_bg).or(master_bg);
                if let Some(fill) = bg {
                    background = background_color(&fill);
                    if matches!(fill, Fill::Gradient { .. } | Fill::Picture { .. }) {
                        let kind = bg_kind(&fill).unwrap_or(ShapeKind::Text);
                        let id = next;
                        next += 1;
                        shapes.push(Shape {
                            id,
                            frame: [0, 0, size.0, size.1],
                            paras: Vec::new(),
                            look: ShapeLook {
                                fill,
                                ..ShapeLook::default()
                            },
                            kind,
                        });
                    }
                }
                let master_ph = master_root.map(|r| ph_nodes(r, p)).unwrap_or_default();
                let layout_ph = layout_root.map(|r| ph_nodes(r, p)).unwrap_or_default();
                if let (Some(root), Some(xml), Some(name)) =
                    (master_root, master_xml.as_ref(), master_part.as_deref())
                {
                    let part = Part::new(package, name, xml, index);
                    if let Some(tree) = named(root, p, "cSld").and_then(|n| named(n, p, "spTree")) {
                        collect(
                            tree,
                            p,
                            &loader,
                            &part,
                            &mut media,
                            &mut next,
                            &mut bind.binding,
                            None,
                            true,
                            &master_ph,
                            &mut shapes,
                        );
                    }
                }
                if let (Some(root), Some(xml), Some(name)) =
                    (layout_root, layout_xml.as_ref(), layout_part.as_deref())
                {
                    let part = Part::new(package, name, xml, index);
                    if let Some(tree) = named(root, p, "cSld").and_then(|n| named(n, p, "spTree")) {
                        collect(
                            tree,
                            p,
                            &loader,
                            &part,
                            &mut media,
                            &mut next,
                            &mut bind.binding,
                            None,
                            true,
                            &layout_ph,
                            &mut shapes,
                        );
                    }
                }
                if let Some(tree) = named(slide_root, p, "cSld").and_then(|n| named(n, p, "spTree"))
                {
                    collect_slide(
                        tree,
                        p,
                        &loader,
                        &part,
                        &mut media,
                        &mut next,
                        &mut bind.binding,
                        None,
                        &layout_ph,
                        &master_ph,
                        master_root,
                        &mut shapes,
                    );
                }
                if media.dir.is_some() {
                    bind.media = media.dir.take().or(bind.media.take());
                }
                slides.push(Slide {
                    hidden,
                    size,
                    shapes,
                    background,
                });
            }
        }
        Ok((Deck::from_slides(slides), bind))
    }
}

fn theme_for(
    package: &Package,
    master: Option<&str>,
    master_root: Option<&XmlNode>,
    a: &str,
) -> (Theme, ColorMap) {
    let cmap = ColorMap::from_master(
        master_root.and_then(|r| named(r, P, "clrMap").or_else(|| named(r, PS, "clrMap"))),
    );
    let Some(master) = master else {
        return (Theme::default(), cmap);
    };
    let Some(target) = package.related_part(master, "theme").ok().flatten() else {
        return (Theme::default(), cmap);
    };
    let Ok(part) = package.part(&target) else {
        return (Theme::default(), cmap);
    };
    (Theme::parse(part.root(), a), cmap)
}

fn bg_kind(fill: &Fill) -> Option<ShapeKind> {
    if matches!(fill, Fill::Picture { .. }) {
        Some(ShapeKind::Picture)
    } else {
        None
    }
}

fn background_color(fill: &Fill) -> Option<crate::look::Rgb> {
    match fill {
        Fill::Solid(c, _) => Some(*c),
        Fill::Gradient { stops, .. } => stops.first().map(|(_, c, _)| *c),
        Fill::Picture { .. } => Some(crate::look::Rgb::WHITE),
        Fill::None => None,
    }
}

fn read_background(
    root: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
) -> Option<Fill> {
    let bg = named(root, p, "cSld").and_then(|n| named(n, p, "bg"))?;
    if let Some(pr) = named(bg, p, "bgPr") {
        let fill = pr.children.iter().find(|n| {
            matches!(
                n.local_name.as_str(),
                "noFill" | "solidFill" | "gradFill" | "blipFill"
            )
        })?;
        return Some(loader.fill(fill, part, media));
    }
    let reference = named(bg, p, "bgRef")?;
    let idx = integer(attr(reference, "idx")).clamp(0, u32::MAX as i64) as u32;
    let color = reference
        .children
        .first()
        .and_then(|c| theme::color(c, &loader.theme, &loader.cmap, None))
        .map(|(c, _)| c);
    Some(loader.theme.background_ref(idx, color))
}

fn group_map(xfrm: &XmlNode, parent: Option<GroupMap>, loader: &Loader<'_>) -> Option<GroupMap> {
    let frame = loader.frame(Some(xfrm));
    let off = named(xfrm, loader.a, "chOff")?;
    let ext = named(xfrm, loader.a, "chExt")?;
    let chx = points(attr(off, "x")) as f64;
    let chy = points(attr(off, "y")) as f64;
    let chw = points(attr(ext, "cx")).max(1) as f64;
    let chh = points(attr(ext, "cy")).max(1) as f64;
    let frame = parent.map_or(frame, |p| p.map(frame));
    Some(GroupMap {
        ox: frame[0] as f64,
        oy: frame[1] as f64,
        sx: frame[2].max(1) as f64 / chw,
        sy: frame[3].max(1) as f64 / chh,
        chx,
        chy,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect(
    tree: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    next: &mut NodeId,
    binding: &mut HashMap<NodeId, Binding>,
    group: Option<GroupMap>,
    locked: bool,
    placeholders: &HashMap<(Option<u32>, String), &XmlNode>,
    out: &mut Vec<Shape>,
) {
    for n in &tree.children {
        if n.local_name == "AlternateContent" {
            if let Some(branch) = n
                .children
                .iter()
                .find(|c| c.local_name == "Choice")
                .or_else(|| n.children.iter().find(|c| c.local_name == "Fallback"))
            {
                let mut children = Vec::new();
                children.extend(branch.children.iter().map(clone_node));
                let holder = XmlNode {
                    namespace: tree.namespace.clone(),
                    local_name: "spTree".into(),
                    range: tree.range.clone(),
                    children,
                    attributes: Vec::new(),
                    text: String::new(),
                };
                collect(
                    &holder,
                    p,
                    loader,
                    part,
                    media,
                    next,
                    binding,
                    group,
                    locked,
                    placeholders,
                    out,
                );
            }
            continue;
        }
        if n.is(p, "grpSp") {
            let xfrm = named(n, p, "grpSpPr").and_then(|s| named(s, loader.a, "xfrm"));
            let map = xfrm.and_then(|x| group_map(x, group, loader)).or(group);
            collect(
                n,
                p,
                loader,
                part,
                media,
                next,
                binding,
                map,
                locked,
                placeholders,
                out,
            );
        } else if n.is(p, "sp") {
            if locked && placeholder(n, p).is_some() {
                continue;
            }
            if let Some(shape) = read_sp(n, p, loader, part, media, next, binding, group, locked) {
                out.push(shape);
            }
        } else if n.is(p, "pic") {
            if let Some(shape) = read_pic(n, p, loader, part, media, next, group) {
                out.push(shape);
            }
        } else if n.is(p, "cxnSp") {
            if let Some(shape) = read_cxn(n, p, loader, part, media, next, group) {
                out.push(shape);
            }
        } else if n.is(p, "graphicFrame") {
            out.extend(read_graphic(
                n, p, loader, part, media, next, binding, group,
            ));
        }
    }
}

fn clone_node(n: &XmlNode) -> XmlNode {
    XmlNode {
        namespace: n.namespace.clone(),
        local_name: n.local_name.clone(),
        range: n.range.clone(),
        children: n.children.iter().map(clone_node).collect(),
        attributes: n
            .attributes
            .iter()
            .map(|a| crate::xml::XmlAttribute {
                namespace: a.namespace.clone(),
                local_name: a.local_name.clone(),
                value: a.value.clone(),
            })
            .collect(),
        text: n.text.clone(),
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_slide(
    tree: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    next: &mut NodeId,
    binding: &mut HashMap<NodeId, Binding>,
    group: Option<GroupMap>,
    layout_ph: &HashMap<(Option<u32>, String), &XmlNode>,
    master_ph: &HashMap<(Option<u32>, String), &XmlNode>,
    master_root: Option<&XmlNode>,
    out: &mut Vec<Shape>,
) {
    for n in &tree.children {
        if n.is(p, "grpSp") {
            let xfrm = named(n, p, "grpSpPr").and_then(|s| named(s, loader.a, "xfrm"));
            let map = xfrm.and_then(|x| group_map(x, group, loader)).or(group);
            collect_slide(
                n,
                p,
                loader,
                part,
                media,
                next,
                binding,
                map,
                layout_ph,
                master_ph,
                master_root,
                out,
            );
            continue;
        }
        if n.is(p, "sp") {
            let ph = placeholder(n, p);
            let mut frame = frame_of(n, p, loader);
            if frame[2] <= 0
                || frame[3] <= 0
                || named(n, p, "spPr")
                    .and_then(|s| named(s, loader.a, "xfrm"))
                    .is_none()
            {
                if let Some(found) = ph
                    .as_ref()
                    .and_then(|ph| find_ph(layout_ph, ph).or_else(|| find_ph(master_ph, ph)))
                {
                    frame = frame_of(found, p, loader);
                }
            }
            let text_style = ph.as_ref().map_or(
                TextStyle {
                    tx: None,
                    master_lst: None,
                    layout_lst: None,
                    major: false,
                },
                |ph| TextStyle {
                    tx: master_txstyles(master_root, p, ph),
                    master_lst: find_ph(master_ph, ph).and_then(|n| lst(n, p)),
                    layout_lst: find_ph(layout_ph, ph).and_then(|n| lst(n, p)),
                    major: matches!(ph.ty.as_str(), "title" | "ctrTitle"),
                },
            );
            if let Some(mut shape) = read_sp_with_style(
                n, p, loader, part, media, next, binding, group, false, text_style,
            ) {
                if frame[2] > 0 && frame[3] > 0 {
                    shape.frame = frame;
                }
                out.push(shape);
            }
        } else if n.is(p, "pic") {
            if let Some(shape) = read_pic(n, p, loader, part, media, next, group) {
                out.push(shape);
            }
        } else if n.is(p, "cxnSp") {
            if let Some(shape) = read_cxn(n, p, loader, part, media, next, group) {
                out.push(shape);
            }
        } else if n.is(p, "graphicFrame") {
            out.extend(read_graphic(
                n, p, loader, part, media, next, binding, group,
            ));
        } else if n.local_name == "AlternateContent" {
            if let Some(branch) = n
                .children
                .iter()
                .find(|c| c.local_name == "Choice")
                .or_else(|| n.children.iter().find(|c| c.local_name == "Fallback"))
            {
                let mut children = Vec::new();
                children.extend(branch.children.iter().map(clone_node));
                let holder = XmlNode {
                    namespace: tree.namespace.clone(),
                    local_name: "spTree".into(),
                    range: tree.range.clone(),
                    children,
                    attributes: Vec::new(),
                    text: String::new(),
                };
                collect_slide(
                    &holder,
                    p,
                    loader,
                    part,
                    media,
                    next,
                    binding,
                    group,
                    layout_ph,
                    master_ph,
                    master_root,
                    out,
                );
            }
        }
    }
}

fn frame_of(n: &XmlNode, p: &str, loader: &Loader<'_>) -> [i32; 4] {
    let xfrm = named(n, p, "spPr").and_then(|s| named(s, loader.a, "xfrm"));
    loader.frame(xfrm)
}

#[allow(clippy::too_many_arguments)]
fn read_sp(
    n: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    next: &mut NodeId,
    binding: &mut HashMap<NodeId, Binding>,
    group: Option<GroupMap>,
    locked: bool,
) -> Option<Shape> {
    read_sp_with_style(
        n,
        p,
        loader,
        part,
        media,
        next,
        binding,
        group,
        locked,
        TextStyle {
            tx: None,
            master_lst: None,
            layout_lst: None,
            major: false,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn read_sp_with_style(
    n: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    next: &mut NodeId,
    binding: &mut HashMap<NodeId, Binding>,
    group: Option<GroupMap>,
    locked: bool,
    text_style: TextStyle<'_>,
) -> Option<Shape> {
    let id = *next;
    *next += 1;
    let sp_pr = named(n, p, "spPr");
    let xfrm = sp_pr.and_then(|s| named(s, loader.a, "xfrm"));
    let mut frame = loader.frame(xfrm);
    if let Some(map) = group {
        frame = map.map(frame);
    }
    let look = read_look(n, p, loader, part, media, xfrm, sp_pr);
    let body = shape_body(n, p);
    let paras = body.map_or_else(Vec::new, |body| {
        read_body(
            body,
            p,
            loader,
            part,
            next,
            binding,
            &text_style,
            id as u32,
            locked,
        )
    });
    Some(Shape {
        id,
        frame,
        paras,
        look,
        kind: ShapeKind::Text,
    })
}

fn read_look(
    n: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    xfrm: Option<&XmlNode>,
    sp_pr: Option<&XmlNode>,
) -> ShapeLook {
    let mut look = ShapeLook::default();
    if let Some(sp) = sp_pr {
        let fill_node = sp.children.iter().find(|c| {
            matches!(
                c.local_name.as_str(),
                "noFill" | "solidFill" | "gradFill" | "blipFill"
            )
        });
        if let Some(fill) = fill_node {
            look.fill = loader.fill(fill, part, media);
        }
        let ln = named(sp, loader.a, "ln");
        if let Some(ln) = ln {
            look.line = style::stroke(ln, &loader.theme, &loader.cmap);
        }
        if fill_node.is_none() || ln.is_none() {
            let (style_fill, style_line) =
                style::style_refs(named(n, p, "style"), &loader.theme, &loader.cmap);
            if fill_node.is_none()
                && let Some(fill) = style_fill
            {
                look.fill = fill;
            }
            if ln.is_none()
                && let Some(line) = style_line
            {
                look.line = Some(line);
            }
        }
        let frame = loader.frame(xfrm);
        look.path = style::path(
            sp.children
                .iter()
                .find(|c| c.local_name == "prstGeom" || c.local_name == "custGeom"),
            frame[2],
            frame[3],
        );
        let (_, flip_h, flip_v) = style::rotation(xfrm);
        if flip_h || flip_v {
            look.path = look
                .path
                .into_iter()
                .map(|cmd| flip_path(cmd, frame[2], frame[3], flip_h, flip_v))
                .collect();
        }
        look.shadow = style::shadow(sp, &loader.theme, &loader.cmap);
    } else {
        let (style_fill, style_line) =
            style::style_refs(named(n, p, "style"), &loader.theme, &loader.cmap);
        look.fill = style_fill.unwrap_or(Fill::None);
        look.line = style_line;
    }
    let (rot, flip_h, flip_v) = style::rotation(xfrm);
    look.rotation = rot;
    look.flip = (flip_h, flip_v);
    let body = shape_body(n, p).and_then(|b| named(b, loader.a, "bodyPr"));
    let body = style::body(body);
    look.insets = body.insets;
    look.v_align = body.v_align;
    look.font_scale = body.font_scale;
    look.vert = body.vert;
    look.auto_grow = body.auto_grow;
    look
}

fn read_pic(
    n: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    next: &mut NodeId,
    group: Option<GroupMap>,
) -> Option<Shape> {
    let id = *next;
    *next += 1;
    let sp_pr = named(n, p, "spPr");
    let xfrm = sp_pr.and_then(|s| named(s, loader.a, "xfrm"));
    let mut frame = loader.frame(xfrm);
    if let Some(map) = group {
        frame = map.map(frame);
    }
    let fill = named(n, p, "blipFill")
        .map(|b| loader.fill(b, part, media))
        .unwrap_or(Fill::None);
    let mut look = ShapeLook {
        fill,
        ..ShapeLook::default()
    };
    let (rot, flip_h, flip_v) = style::rotation(xfrm);
    look.rotation = rot;
    look.flip = (flip_h, flip_v);
    Some(Shape {
        id,
        frame,
        paras: Vec::new(),
        look,
        kind: ShapeKind::Picture,
    })
}

fn flip_path(
    cmd: crate::look::PathCmd,
    w: i32,
    h: i32,
    flip_h: bool,
    flip_v: bool,
) -> crate::look::PathCmd {
    use crate::look::PathCmd;
    let x = |v: i32| if flip_h { w - v } else { v };
    let y = |v: i32| if flip_v { h - v } else { v };
    match cmd {
        PathCmd::Move(a, b) => PathCmd::Move(x(a), y(b)),
        PathCmd::Line(a, b) => PathCmd::Line(x(a), y(b)),
        PathCmd::Quad(a, b, c, d) => PathCmd::Quad(x(a), y(b), x(c), y(d)),
        PathCmd::Cubic(a, b, c, d, e, f) => PathCmd::Cubic(x(a), y(b), x(c), y(d), x(e), y(f)),
        PathCmd::Close => PathCmd::Close,
    }
}

fn read_cxn(
    n: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    next: &mut NodeId,
    group: Option<GroupMap>,
) -> Option<Shape> {
    let mut empty = HashMap::new();
    read_sp(n, p, loader, part, media, next, &mut empty, group, false)
}

#[allow(clippy::too_many_arguments)]
fn read_graphic(
    n: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    next: &mut NodeId,
    binding: &mut HashMap<NodeId, Binding>,
    group: Option<GroupMap>,
) -> Vec<Shape> {
    let id = *next;
    *next += 1;
    let mut frame = loader.frame(named(n, p, "xfrm"));
    if let Some(map) = group {
        frame = map.map(frame);
    }
    let data = named(n, loader.a, "graphic")
        .and_then(|g| named(g, loader.a, "graphicData"))
        .or_else(|| named(n, loader.a, "graphic").and_then(|g| named(g, AS, "graphicData")));
    let Some(data) = data else {
        return vec![fallback(id, frame)];
    };
    if let Some(table) = named(data, loader.a, "tbl") {
        let table = read_table(table, id, p, loader, part, next, binding);
        return vec![Shape {
            id,
            frame,
            paras: Vec::new(),
            look: ShapeLook::default(),
            kind: ShapeKind::Table(Box::new(table)),
        }];
    }
    if let Some(chart) = named(
        data,
        "http://schemas.openxmlformats.org/drawingml/2006/chart",
        "chart",
    )
    .or_else(|| named(data, "http://purl.oclc.org/ooxml/drawingml/chart", "chart"))
    {
        let rid = chart
            .attr(Some(loader.r), "id")
            .or_else(|| chart.attr(Some(RS), "id"));
        let target = rid
            .and_then(|id| part.rels.get(id))
            .filter(|r| !r.external && r.kind("chart"))
            .map(|r| r.target.clone());
        let chart = target
            .as_deref()
            .and_then(|target| loader.package.part(target).ok())
            .map(|part| slides_chart::read(&part, &loader.theme, &loader.cmap))
            .unwrap_or_default();
        return vec![Shape {
            id,
            frame,
            paras: Vec::new(),
            look: ShapeLook::default(),
            kind: ShapeKind::Chart(Box::new(chart)),
        }];
    }
    if named(data, DGM, "relIds")
        .or_else(|| named(data, DGMS, "relIds"))
        .is_some()
    {
        return read_diagram(data, p, loader, part, media, next)
            .unwrap_or_else(|| vec![fallback(id, frame)]);
    }
    if let Some(pic) = find_pic(data) {
        if let Some(mut shape) = read_pic(pic, p, loader, part, media, next, group) {
            let path = match &shape.look.fill {
                Fill::Picture { path, .. } => Some(path.clone()),
                _ => None,
            };
            shape.kind = ShapeKind::Fallback(path);
            return vec![shape];
        }
    }
    vec![fallback(id, frame)]
}

fn fallback(id: NodeId, frame: [i32; 4]) -> Shape {
    Shape {
        id,
        frame,
        paras: Vec::new(),
        look: ShapeLook::default(),
        kind: ShapeKind::Fallback(None),
    }
}

fn find_pic<'n>(n: &'n XmlNode) -> Option<&'n XmlNode> {
    if n.local_name == "pic" {
        return Some(n);
    }
    n.children.iter().find_map(find_pic)
}

fn read_diagram(
    data: &XmlNode,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    media: &mut Media,
    next: &mut NodeId,
) -> Option<Vec<Shape>> {
    let rel_ids = named(data, DGM, "relIds").or_else(|| named(data, DGMS, "relIds"))?;
    let dm = rel_ids
        .attr(Some(loader.r), "dm")
        .or_else(|| rel_ids.attr(Some(RS), "dm"));
    let data_part = dm
        .and_then(|id| part.rels.get(id))
        .map(|r| r.target.clone())?;
    let drawing = loader
        .package
        .relationships(&data_part)
        .ok()?
        .into_iter()
        .find(|r| !r.external && r.kind("diagramDrawing"))
        .map(|r| r.target)?;
    let xml = loader.package.part(&drawing).ok()?;
    let root = xml.root();
    let dsp = "http://schemas.openxmlformats.org/drawingml/2006/diagram";
    let mut out = Vec::new();
    for sp in root
        .children
        .iter()
        .flat_map(|n| n.children.iter())
        .filter(|n| n.local_name == "sp")
    {
        let sp_pr = named(sp, dsp, "spPr");
        let frame = loader.frame(sp_pr.and_then(|s| named(s, loader.a, "xfrm")));
        let look = sp_pr.map_or_else(ShapeLook::default, |sp| ShapeLook {
            fill: sp
                .children
                .iter()
                .find(|c| matches!(c.local_name.as_str(), "solidFill" | "gradFill" | "noFill"))
                .map(|c| loader.fill(c, part, media))
                .unwrap_or(Fill::None),
            ..ShapeLook::default()
        });
        let body = named(sp, dsp, "txBody").or_else(|| named(sp, p, "txBody"));
        let chain = TextStyle {
            tx: None,
            master_lst: None,
            layout_lst: None,
            major: false,
        };
        let mut empty = HashMap::new();
        let paras = body.map_or_else(Vec::new, |body| {
            read_body(
                body,
                p,
                loader,
                part,
                next,
                &mut empty,
                &chain,
                *next as u32,
                true,
            )
        });
        out.push(Shape {
            id: *next,
            frame,
            paras,
            look,
            kind: ShapeKind::Text,
        });
        *next += 1;
    }
    Some(out)
}

fn read_table(
    table: &XmlNode,
    table_id: NodeId,
    p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    next: &mut NodeId,
    binding: &mut HashMap<NodeId, Binding>,
) -> Table {
    let mut columns = Vec::new();
    if let Some(grid) = named(table, loader.a, "tblGrid") {
        for col in grid.children.iter().filter(|n| n.local_name == "gridCol") {
            columns.push(emu(attr(col, "w")));
        }
    }
    let mut rows = Vec::new();
    let mut looks = Vec::new();
    let mut merged = Vec::new();
    let mut padding = [0; 4];
    let first_row = named(table, loader.a, "tblPr")
        .and_then(|pr| attr(pr, "firstRow"))
        .is_none_or(|v| matches!(v, "1" | "true"));
    let band_row = named(table, loader.a, "tblPr")
        .and_then(|pr| attr(pr, "bandRow"))
        .is_none_or(|v| matches!(v, "1" | "true"));
    for (r, row) in table
        .children
        .iter()
        .filter(|n| n.local_name == "tr")
        .enumerate()
    {
        let mut cells = Vec::new();
        let mut row_looks = Vec::new();
        let mut column = 0usize;
        for cell in row.children.iter().filter(|n| n.local_name == "tc") {
            let mut cell_look = CellLook::default();
            if let Some(pr) = named(cell, loader.a, "tcPr") {
                for side in ["lnL", "lnR", "lnT", "lnB"] {
                    if let Some(line) = named(pr, loader.a, side)
                        && let Some(stroke) = style::stroke(line, &loader.theme, &loader.cmap)
                    {
                        let border = crate::look::Border {
                            width: stroke.width,
                            color: stroke.color,
                            space: 0,
                        };
                        let borders = cell_look.borders.get_or_insert_with(Borders::default);
                        match side {
                            "lnL" => borders.left = Some(border),
                            "lnR" => borders.right = Some(border),
                            "lnT" => borders.top = Some(border),
                            _ => borders.bottom = Some(border),
                        }
                    }
                }
                if let Some(fill) = pr
                    .children
                    .iter()
                    .find(|c| matches!(c.local_name.as_str(), "solidFill" | "gradFill" | "noFill"))
                    && let Fill::Solid(c, _) = loader.fill(fill, part, &mut Media::new())
                {
                    cell_look.shading = Some(c);
                }
                for (i, key) in ["marT", "marR", "marB", "marL"].into_iter().enumerate() {
                    if let Some(value) = attr(pr, key) {
                        cell_look.padding[i] = emu(Some(value));
                    }
                }
                cell_look.v_align = match attr(pr, "anchor") {
                    Some("ctr") => VAlign::Center,
                    Some("b") => VAlign::Bottom,
                    _ => VAlign::Top,
                };
                if cell
                    .attr(None, "vMerge")
                    .is_some_and(|v| matches!(v, "1" | "true" | "cont"))
                    || cell
                        .attr(None, "rowSpan")
                        .is_some_and(|v| v.parse::<usize>().is_ok_and(|n| n > 1))
                {
                    cell_look.merged_up = true;
                }
            }
            if padding == [0; 4] {
                padding = cell_look.padding;
            }
            let span = cell
                .attr(None, "gridSpan")
                .or_else(|| cell.attr(None, "colSpan"))
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n > 1)
                .unwrap_or(1);
            for k in 1..span {
                merged.push((r, column + k));
            }
            let blocks = named(cell, loader.a, "txBody").map_or_else(Vec::new, |body| {
                let chain = TextStyle {
                    tx: None,
                    master_lst: None,
                    layout_lst: None,
                    major: false,
                };
                read_body(
                    body,
                    p,
                    loader,
                    part,
                    next,
                    binding,
                    &chain,
                    cell_seed(table_id, r, column),
                    false,
                )
                .into_iter()
                .map(Block::Paragraph)
                .collect()
            });
            cells.push(blocks);
            row_looks.push(cell_look);
            column += span;
        }
        if cells.is_empty() {
            continue;
        }
        rows.push(cells);
        looks.push(row_looks);
    }
    let mut look = TableLook {
        columns,
        cells: looks,
        rows: table
            .children
            .iter()
            .filter(|n| n.local_name == "tr")
            .map(|row| Some((emu(attr(row, "h")), false)))
            .collect(),
        header_rows: usize::from(first_row),
        padding,
        ..TableLook::default()
    };
    if let Some(style_id) = named(table, loader.a, "tblPr")
        .and_then(|pr| named(pr, loader.a, "tableStyleId"))
        .map(|n| n.text.trim().to_owned())
    {
        loader.styles.apply(
            &style_id,
            &mut rows,
            &mut look,
            first_row,
            band_row,
            &loader.theme,
        );
    }
    let table = Table {
        id: table_id,
        rows,
        merged,
        look,
    };
    sync_table_bindings(&table, binding);
    table
}

fn cell_seed(table: NodeId, row: usize, col: usize) -> u32 {
    (table as u32).wrapping_mul(131) ^ ((row as u32) << 8) ^ col as u32
}

fn sync_table_bindings(table: &Table, binding: &mut HashMap<NodeId, Binding>) {
    for p in table
        .rows
        .iter()
        .flatten()
        .flatten()
        .filter_map(|b| match b {
            Block::Paragraph(p) => Some(p),
            _ => None,
        })
    {
        let Some(binding) = binding.get_mut(&p.id) else {
            continue;
        };
        if binding.pieces.len() != p.inlines.len() {
            continue;
        }
        for (piece, view) in binding.pieces.iter_mut().zip(&p.inlines) {
            if matches!(
                (&piece.view, view),
                (Inline::Text { .. }, Inline::Text { .. })
            ) {
                piece.view = view.clone();
            }
        }
    }
}

fn read_body(
    body: &XmlNode,
    _p: &str,
    loader: &Loader<'_>,
    part: &Part<'_>,
    next: &mut NodeId,
    binding: &mut HashMap<NodeId, Binding>,
    chain: &TextStyle<'_>,
    shape_id: u32,
    locked_shape: bool,
) -> Vec<Para> {
    let own_lst = named(body, loader.a, "lstStyle");
    let mut out = Vec::new();
    for pnode in body.children.iter().filter(|n| n.local_name == "p") {
        let id = *next;
        *next += 1;
        let ppr = named(pnode, loader.a, "pPr");
        let level = ppr
            .and_then(|pr| attr(pr, "lvl"))
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(0);
        let list_id = shape_id.max(1) | 0x4000_0000;
        let mut state = chain.state(loader, level, list_id);
        if let Some(list) = own_lst {
            state.apply_lst(list, level, &loader.theme, &loader.cmap, list_id);
        }
        if let Some(pr) = ppr {
            state.apply_para(pr, &loader.theme, &loader.cmap, list_id, level);
        }
        let mut pieces = Vec::new();
        let mut locked = locked_shape;
        let mut seen_text = false;
        let mut end_run = false;
        for c in &pnode.children {
            if c.local_name == "pPr" {
                if seen_text || end_run {
                    locked = true;
                }
                continue;
            }
            if c.local_name == "endParaRPr" {
                end_run = true;
                continue;
            }
            if end_run {
                locked = true;
            }
            seen_text = true;
            if c.local_name == "r" || c.local_name == "fld" {
                if c.local_name == "fld"
                    || c.children
                        .iter()
                        .any(|n| n.local_name != "rPr" && n.local_name != "t")
                {
                    locked = true;
                }
                let rpr = named(c, loader.a, "rPr");
                let marks = style::direct_marks(rpr, &state.marks, &loader.theme, &loader.cmap);
                if let Some(t) = named(c, loader.a, "t") {
                    let bounds = text_range(part.xml.bytes(), t);
                    if bounds.is_none() {
                        locked = true;
                    }
                    pieces.push(Piece {
                        view: Inline::Text {
                            text: t.text.clone(),
                            marks,
                        },
                        raw: c.range.clone(),
                        text: bounds,
                        props: rpr.map(|n| n.range.clone()),
                    });
                } else {
                    locked = true;
                }
            } else if c.local_name == "br" {
                pieces.push(Piece {
                    view: Inline::Break,
                    raw: c.range.clone(),
                    text: None,
                    props: None,
                });
            } else if c.local_name == "tab" {
                pieces.push(Piece {
                    view: Inline::Tab,
                    raw: c.range.clone(),
                    text: None,
                    props: None,
                });
            } else {
                locked = true;
            }
        }
        let has_gaps = pieces.windows(2).any(|w| {
            part.xml.bytes()[w[0].raw.end..w[1].raw.start]
                .iter()
                .any(|b| !b.is_ascii_whitespace())
        });
        locked |= has_gaps;
        let open = &part.xml.bytes()[pnode.range.clone()];
        let inlines = pieces.iter().map(|r| r.view.clone()).collect();
        let range = pieces
            .first()
            .zip(pieces.last())
            .map(|(l, r)| l.raw.start..r.raw.end)
            .or_else(|| {
                named(pnode, loader.a, "endParaRPr")
                    .map(|end| end.range.start..end.range.start)
                    .or_else(|| {
                        let close = open.windows(2).rposition(|b| b == b"</")? + pnode.range.start;
                        Some(close..close)
                    })
            });
        locked |= range.is_none();
        if let Some(range) = range {
            binding.insert(
                id,
                Binding {
                    part: part.index,
                    range,
                    pieces,
                    prefix: qualified_prefix(open),
                },
            );
        }
        let mut look = state.para.clone();
        look.mark = named(pnode, loader.a, "endParaRPr")
            .map(|n| style::marks(n, &state.marks, &loader.theme, &loader.cmap))
            .unwrap_or_else(|| state.marks.clone());
        out.push(Para {
            id,
            style: ParaStyle::Normal,
            align: state.align,
            inlines,
            locked,
            look,
        });
    }
    out
}

fn qualified_prefix(raw: &[u8]) -> String {
    let q = raw[1..]
        .split(|b| b.is_ascii_whitespace() || matches!(b, b'>' | b'/'))
        .next()
        .unwrap_or_default();
    q.iter()
        .rposition(|b| *b == b':')
        .map_or_else(String::new, |i| {
            String::from_utf8_lossy(&q[..=i]).into_owned()
        })
}

fn text_range(raw: &[u8], node: &XmlNode) -> Option<Range<usize>> {
    let raw = &raw[node.range.clone()];
    let mut quote = None;
    let start = raw.iter().position(|&b| {
        if let Some(q) = quote {
            if q == b {
                quote = None;
            }
            false
        } else if matches!(b, b'\'' | b'"') {
            quote = Some(b);
            false
        } else {
            b == b'>'
        }
    })? + 1;
    let end = raw.windows(2).rposition(|b| b == b"</")?;
    if end < start || raw[start..end].contains(&b'<') {
        return None;
    }
    Some(node.range.start + start..node.range.start + end)
}

impl SlidesBinding {
    pub(crate) fn changes(
        &self,
        package: &Package,
        deck: &Deck,
    ) -> Result<Vec<(String, Vec<u8>)>, SaveError> {
        let mut staged: BTreeMap<usize, Vec<(&Binding, &[Inline])>> = BTreeMap::new();
        for p in deck
            .slides()
            .iter()
            .flat_map(|s| &s.shapes)
            .flat_map(Shape::text_paras)
        {
            let Some(b) = self.binding.get(&p.id) else {
                if p.locked {
                    continue;
                }
                return Err(SaveError::Unsupported);
            };
            if p.inlines.iter().eq(b.pieces.iter().map(|r| &r.view)) {
                continue;
            }
            if p.locked {
                return Err(SaveError::Unsupported);
            }
            staged.entry(b.part).or_default().push((b, &p.inlines));
        }
        let mut changes = Vec::new();
        for (part, mut edits) in staged {
            let name = &self.parts[part];
            let mut raw = package
                .read_part(name)
                .map_err(|e| SaveError::Io(e.to_string()))?;
            edits.sort_unstable_by_key(|(b, _)| std::cmp::Reverse(b.range.start));
            for (b, inlines) in edits {
                let bytes = serialize(b, inlines, &raw)?;
                raw.splice(b.range.clone(), bytes);
            }
            if let Err(e) = XmlPart::parse(raw.clone()) {
                eprintln!("{}", String::from_utf8_lossy(&raw));
                return Err(SaveError::Io(e.to_string()));
            }
            changes.push((name.clone(), raw));
        }
        Ok(changes)
    }

    pub(crate) fn rebase(package: &Package, deck: &Deck) -> Result<Self, OpenError> {
        let (parsed, mut new) = Self::load(package)?;
        if parsed.slides().len() != deck.slides().len() {
            return Err(damaged("slide count changed on save"));
        }
        let mut bindings = HashMap::new();
        for (left, right) in deck.slides().iter().zip(parsed.slides()) {
            if left.hidden != right.hidden
                || left.size != right.size
                || left.shapes.len() != right.shapes.len()
            {
                return Err(damaged("slide structure changed on save"));
            }
            for (left, right) in left.shapes.iter().zip(&right.shapes) {
                if left.frame != right.frame || !same_kind(&left.kind, &right.kind) {
                    return Err(damaged("slide shape changed on save"));
                }
                let left_paras: Vec<_> = left.text_paras().collect();
                let right_paras: Vec<_> = right.text_paras().collect();
                if left_paras.len() != right_paras.len() {
                    return Err(damaged("slide shape changed on save"));
                }
                for (left, right) in left_paras.iter().zip(&right_paras) {
                    if left.inlines != right.inlines
                        || left.locked != right.locked
                        || left.align != right.align
                        || left.style != right.style
                    {
                        return Err(damaged("slide paragraph changed on save"));
                    }
                    if let Some(binding) = new.binding.remove(&right.id)
                        && bindings.insert(left.id, binding).is_some()
                    {
                        return Err(damaged("duplicate slide paragraph id"));
                    }
                }
            }
        }
        new.binding = bindings;
        Ok(new)
    }
}

fn same_kind(left: &ShapeKind, right: &ShapeKind) -> bool {
    match (left, right) {
        (ShapeKind::Table(a), ShapeKind::Table(b)) => {
            a.rows.len() == b.rows.len()
                && a.rows.iter().zip(&b.rows).all(|(a, b)| a.len() == b.len())
        }
        (ShapeKind::Chart(a), ShapeKind::Chart(b)) => a.kind == b.kind,
        (ShapeKind::Picture, ShapeKind::Picture) => true,
        (ShapeKind::Fallback(_), ShapeKind::Fallback(_)) => true,
        (ShapeKind::Text, ShapeKind::Text) => true,
        _ => false,
    }
}

fn serialize(b: &Binding, inlines: &[Inline], raw: &[u8]) -> Result<Vec<u8>, SaveError> {
    let old = &b.pieces;
    let prefix = inlines
        .iter()
        .zip(old)
        .take_while(|(new, old)| *new == &old.view)
        .count();
    let suffix = inlines[prefix..]
        .iter()
        .rev()
        .zip(old[prefix..].iter().rev())
        .take_while(|(new, old)| *new == &old.view)
        .count();
    let mut out = Vec::new();
    let mut last = None;
    let mut i = prefix;
    for (j, new) in inlines.iter().enumerate() {
        let k = if j < prefix {
            Some(j)
        } else if j >= inlines.len() - suffix {
            Some(old.len() - (inlines.len() - j))
        } else {
            let end = (i + 16).min(old.len() - suffix);
            let exact = (i..end).find(|&k| &old[k].view == new);
            let at = exact
                .or_else(|| {
                    let Inline::Text { marks, .. } = new else {
                        return None;
                    };
                    (i..end).find(
                        |&k| matches!(&old[k].view, Inline::Text{marks:previous,..} if marks==previous),
                    )
                })
                .or_else(|| {
                    (i < end
                        && matches!(new, Inline::Text { .. })
                        && matches!(old[i].view, Inline::Text { .. }))
                    .then_some(i)
                });
            if let Some(k) = at {
                i = k + 1;
            }
            at
        };
        if let Some(k) = k {
            if last == k.checked_sub(1)
                && let Some(previous) = last
            {
                out.extend_from_slice(&raw[old[previous].raw.end..old[k].raw.start]);
            }
            let piece = &old[k];
            if new == &piece.view {
                out.extend_from_slice(&raw[piece.raw.clone()]);
            } else if let Inline::Text { text, marks } = new {
                let range = piece.text.clone().ok_or(SaveError::Unsupported)?;
                let escaped = escape_text(text)?;
                if matches!(&piece.view,Inline::Text{marks:previous,..} if previous==marks) {
                    out.extend_from_slice(&raw[piece.raw.start..range.start]);
                } else {
                    if marks.link.is_some() {
                        return Err(SaveError::Unsupported);
                    }
                    let (patched, bytes) = mark_patch(raw, piece, marks)?;
                    if patched.end > range.start {
                        return Err(SaveError::Unsupported);
                    }
                    out.extend_from_slice(&raw[piece.raw.start..patched.start]);
                    out.extend_from_slice(&bytes);
                    out.extend_from_slice(&raw[patched.end..range.start]);
                }
                out.extend_from_slice(escaped.as_bytes());
                out.extend_from_slice(&raw[range.end..piece.raw.end]);
            } else {
                return Err(SaveError::Unsupported);
            }
        } else {
            let q = &b.prefix;
            match new {
                Inline::Break => out.extend_from_slice(format!("<{q}br/>").as_bytes()),
                Inline::Tab => out.extend_from_slice(format!("<{q}tab/>").as_bytes()),
                Inline::Text { text, marks } => {
                    if marks.link.is_some() {
                        return Err(SaveError::Unsupported);
                    }
                    if old
                        .get(i)
                        .is_some_and(|p| matches!(p.view, Inline::Text { .. }))
                    {
                        return Err(SaveError::Unsupported);
                    }
                    let rpr = new_rpr(marks, q)?;
                    out.extend_from_slice(
                        format!("<{q}r>{rpr}<{q}t>{}</{q}t></{q}r>", escape_text(text)?).as_bytes(),
                    );
                }
                _ => return Err(SaveError::Unsupported),
            }
        }
        last = k;
    }
    Ok(out)
}

fn escape_text(text: &str) -> Result<String, SaveError> {
    if text.chars().any(|c|!matches!(c,'\t'|'\r'|'\n'|'\u{20}'..='\u{d7ff}'|'\u{e000}'..='\u{fffd}'|'\u{10000}'..='\u{10ffff}')){
        return Err(SaveError::Unsupported);
    }
    Ok(quick_xml::escape::escape(text).replace('\r', "&#13;"))
}

fn new_rpr(marks: &Marks, q: &str) -> Result<String, SaveError> {
    let mut attrs = String::new();
    let b = u8::from(marks.bold);
    let i = u8::from(marks.italic);
    let u = if marks.underline { "sng" } else { "none" };
    let s = if marks.strike {
        "sngStrike"
    } else {
        "noStrike"
    };
    attrs.push_str(&format!(" b=\"{b}\" i=\"{i}\" u=\"{u}\" strike=\"{s}\""));
    if let Some(size) = marks.size {
        attrs.push_str(&format!(" sz=\"{}\"", size * 100 / 20));
    }
    let mut body = String::new();
    if let Some(color) = marks.color {
        body.push_str(&format!(
            "<{q}solidFill><{q}srgbClr val=\"{:06X}\"/></{q}solidFill>",
            color.0
        ));
    }
    if !marks.fonts.latin.is_empty() {
        body.push_str(&format!(
            "<{q}latin typeface=\"{}\"/>",
            quick_xml::escape::escape(&marks.fonts.latin)
        ));
    }
    if !marks.fonts.east_asia.is_empty() {
        body.push_str(&format!(
            "<{q}ea typeface=\"{}\"/>",
            quick_xml::escape::escape(&marks.fonts.east_asia)
        ));
    }
    Ok(format!("<{q}rPr{attrs}>{body}</{q}rPr>"))
}

fn mark_patch(raw: &[u8], r: &Piece, marks: &Marks) -> Result<(Range<usize>, Vec<u8>), SaveError> {
    let mut values = vec![
        ("b", if marks.bold { "1" } else { "0" }.to_owned()),
        ("i", if marks.italic { "1" } else { "0" }.to_owned()),
        ("u", if marks.underline { "sng" } else { "none" }.to_owned()),
        (
            "strike",
            if marks.strike {
                "sngStrike"
            } else {
                "noStrike"
            }
            .to_owned(),
        ),
    ];
    if let Some(size) = marks.size {
        values.push(("sz", (size * 100 / 20).to_string()));
    }
    if let Some(rp) = &r.props {
        let src = &raw[rp.clone()];
        let end = open_end(src)?;
        let q = {
            let name = name(src);
            name.iter()
                .rposition(|&b| b == b':')
                .map(|i| String::from_utf8_lossy(&name[..=i]).into_owned())
                .unwrap_or_default()
        };
        let close = src.windows(2).rposition(|b| b == b"</");
        let self_closing = src[..=end].ends_with(b"/>");
        let inner = if self_closing {
            &[][..]
        } else if let Some(close) = close {
            &src[end + 1..close]
        } else {
            &[][..]
        };
        let mut output = patch_attributes(&src[..=end], &values)?;
        let keep = keep_children(inner, &["solidFill", "latin", "ea"])?;
        let mut body = Vec::new();
        if let Some(color) = marks.color {
            body.extend_from_slice(
                format!(
                    "<{q}solidFill><{q}srgbClr val=\"{:06X}\"/></{q}solidFill>",
                    color.0
                )
                .as_bytes(),
            );
        }
        if !marks.fonts.latin.is_empty() {
            body.extend_from_slice(
                format!(
                    "<{q}latin typeface=\"{}\"/>",
                    quick_xml::escape::escape(&marks.fonts.latin)
                )
                .as_bytes(),
            );
        }
        if !marks.fonts.east_asia.is_empty() {
            body.extend_from_slice(
                format!(
                    "<{q}ea typeface=\"{}\"/>",
                    quick_xml::escape::escape(&marks.fonts.east_asia)
                )
                .as_bytes(),
            );
        }
        for range in keep {
            body.extend_from_slice(&inner[range]);
        }
        if output.ends_with(b"/>") {
            output.truncate(output.len() - 2);
            output.push(b'>');
        }
        output.extend_from_slice(&body);
        output.extend_from_slice(format!("</{q}rPr>").as_bytes());
        Ok((rp.start..rp.start + src.len(), output))
    } else {
        let src = &raw[r.raw.clone()];
        let end = open_end(src)?;
        let q = {
            let name = name(src);
            name.iter()
                .rposition(|&b| b == b':')
                .map(|i| String::from_utf8_lossy(&name[..=i]).into_owned())
                .unwrap_or_default()
        };
        Ok((
            r.raw.start + end + 1..r.raw.start + end + 1,
            new_rpr(marks, &q)?.into_bytes(),
        ))
    }
}

fn patch_attributes(open: &[u8], values: &[(&str, String)]) -> Result<Vec<u8>, SaveError> {
    let end = open_end(open)?;
    let mut output = open[..1 + name(open).len()].to_vec();
    let mut i = output.len();
    let mut wrote = vec![false; values.len()];
    while i < end {
        let start = i;
        while i < end && open[i].is_ascii_whitespace() {
            i += 1;
        }
        if i == end || open[i] == b'/' {
            for (k, (key, value)) in values.iter().enumerate() {
                if !wrote[k] {
                    output.extend_from_slice(format!(" {key}=\"{value}\"").as_bytes());
                }
            }
            output.extend_from_slice(&open[start..=end]);
            return Ok(output);
        }
        let k = i;
        while i < end && !open[i].is_ascii_whitespace() && open[i] != b'=' {
            i += 1;
        }
        let key = &open[k..i];
        while i < end && (open[i].is_ascii_whitespace() || open[i] == b'=') {
            i += 1;
        }
        let quote = *open
            .get(i)
            .filter(|&&b| matches!(b, b'\'' | b'"'))
            .ok_or(SaveError::Unsupported)?;
        i += 1;
        while i < end && open[i] != quote {
            i += 1;
        }
        if i == end {
            return Err(SaveError::Unsupported);
        }
        i += 1;
        if let Some((index, (_, value))) = values
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name.as_bytes() == key)
        {
            output.extend_from_slice(
                format!(
                    " {}={}{}{}",
                    values[index].0, quote as char, value, quote as char
                )
                .as_bytes(),
            );
            wrote[index] = true;
        } else {
            output.extend_from_slice(&open[start..i]);
        }
    }
    for (k, (key, value)) in values.iter().enumerate() {
        if !wrote[k] {
            output.extend_from_slice(format!(" {key}=\"{value}\"").as_bytes());
        }
    }
    output.extend_from_slice(b">");
    Ok(output)
}

fn keep_children(inner: &[u8], remove: &[&str]) -> Result<Vec<Range<usize>>, SaveError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < inner.len() {
        let Some(start) = inner[i..].iter().position(|&b| b == b'<').map(|p| i + p) else {
            break;
        };
        if inner[start..].starts_with(b"<!--") {
            let Some(end) = inner[start..]
                .windows(3)
                .position(|w| w == b"-->")
                .map(|p| start + p + 3)
            else {
                break;
            };
            i = end;
            continue;
        }
        if inner[start..].starts_with(b"</") {
            i = inner[start..]
                .iter()
                .position(|&b| b == b'>')
                .map_or(inner.len(), |p| start + p + 1);
            continue;
        }
        let end = open_end(&inner[start..])? + start + 1;
        let self_closing = inner[start..end].ends_with(b"/>");
        let name = child_name(&inner[start..end]);
        if !self_closing {
            let mut depth = 1;
            let mut j = end;
            while j < inner.len() && depth > 0 {
                let Some(next) = inner[j..].iter().position(|&b| b == b'<').map(|p| j + p) else {
                    return Err(SaveError::Unsupported);
                };
                if inner[next..].starts_with(b"</") {
                    let stop = inner[next..]
                        .iter()
                        .position(|&b| b == b'>')
                        .map_or(inner.len(), |p| next + p + 1);
                    depth -= 1;
                    j = stop;
                } else {
                    let stop = open_end(&inner[next..])? + next + 1;
                    if !inner[next..stop].ends_with(b"/>") {
                        depth += 1;
                    }
                    j = stop;
                }
            }
            if !remove.contains(&name.as_str()) {
                out.push(start..j);
            }
            i = j;
        } else {
            if !remove.contains(&name.as_str()) {
                out.push(start..end);
            }
            i = end;
        }
    }
    Ok(out)
}

fn child_name(open: &[u8]) -> String {
    let name = name(open);
    let local = name
        .iter()
        .rposition(|&b| b == b':')
        .map_or(name, |i| &name[i + 1..]);
    String::from_utf8_lossy(local).into_owned()
}

fn open_end(raw: &[u8]) -> Result<usize, SaveError> {
    let mut quote = None;
    raw.iter()
        .position(|&b| {
            if let Some(q) = quote {
                if q == b {
                    quote = None;
                }
                false
            } else if matches!(b, b'\'' | b'"') {
                quote = Some(b);
                false
            } else {
                b == b'>'
            }
        })
        .ok_or(SaveError::Unsupported)
}

fn name(raw: &[u8]) -> &[u8] {
    let end = raw
        .iter()
        .position(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
        .unwrap_or(raw.len());
    &raw[1..end]
}

#[derive(Clone, Debug, Default)]
struct CellSpec {
    fill: Option<crate::look::Rgb>,
    color: Option<crate::look::Rgb>,
    bold: Option<bool>,
}

#[derive(Clone, Debug, Default)]
struct TableStyleSpec {
    whole: CellSpec,
    band1: CellSpec,
    band2: CellSpec,
    first_row: CellSpec,
    last_row: CellSpec,
    first_col: CellSpec,
    last_col: CellSpec,
}

#[derive(Clone, Debug, Default)]
struct TableStyles {
    styles: HashMap<String, TableStyleSpec>,
}

impl TableStyles {
    fn parse(root: &XmlNode, a: &str, theme: &Theme, cmap: &ColorMap) -> Self {
        let mut out = Self::default();
        let Some(list) = named(root, a, "tblStyleLst").or_else(|| named(root, AS, "tblStyleLst"))
        else {
            return out;
        };
        for style in list.children.iter().filter(|n| n.local_name == "tblStyle") {
            let Some(id) = attr(style, "styleId") else {
                continue;
            };
            let mut spec = TableStyleSpec::default();
            for (name, target) in [
                ("wholeTbl", &mut spec.whole),
                ("band1H", &mut spec.band1),
                ("band2H", &mut spec.band2),
                ("firstRow", &mut spec.first_row),
                ("lastRow", &mut spec.last_row),
                ("firstCol", &mut spec.first_col),
                ("lastCol", &mut spec.last_col),
            ] {
                if let Some(n) = named(style, a, name) {
                    cell_spec(n, a, theme, cmap, target);
                }
            }
            out.styles.insert(id.to_owned(), spec);
        }
        out
    }

    fn apply(
        &self,
        id: &str,
        rows: &mut [Vec<Vec<Block>>],
        look: &mut TableLook,
        first_row: bool,
        band_row: bool,
        theme: &Theme,
    ) {
        let spec = self
            .styles
            .get(id)
            .cloned()
            .unwrap_or_else(|| builtin_style(id, theme));
        let row_count = rows.len();
        for (r, row) in rows.iter_mut().enumerate() {
            let cell_count = row.len();
            for (c, cell) in row.iter_mut().enumerate() {
                let cell_look = look.cells.get_mut(r).and_then(|row| row.get_mut(c));
                let mut app = spec.whole.clone();
                if band_row {
                    let band = if r % 2 == usize::from(first_row) {
                        &spec.band1
                    } else {
                        &spec.band2
                    };
                    merge_spec(&mut app, band);
                }
                if first_row && r == 0 {
                    merge_spec(&mut app, &spec.first_row);
                }
                if first_row && r + 1 == row_count {
                    merge_spec(&mut app, &spec.last_row);
                }
                if c == 0 {
                    merge_spec(&mut app, &spec.first_col);
                }
                if c + 1 == cell_count {
                    merge_spec(&mut app, &spec.last_col);
                }
                if let Some(cell_look) = cell_look
                    && cell_look.shading.is_none()
                {
                    cell_look.shading = app.fill;
                }
                for block in cell {
                    if let Block::Paragraph(p) = block {
                        for inline in &mut p.inlines {
                            if let Inline::Text { marks, .. } = inline {
                                if marks.color.is_none() {
                                    marks.color = app.color;
                                }
                                if let Some(bold) = app.bold {
                                    marks.bold |= bold;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn merge_spec(target: &mut CellSpec, source: &CellSpec) {
    if source.fill.is_some() {
        target.fill = source.fill;
    }
    if source.color.is_some() {
        target.color = source.color;
    }
    if source.bold.is_some() {
        target.bold = source.bold;
    }
}

fn cell_spec(n: &XmlNode, a: &str, theme: &Theme, cmap: &ColorMap, out: &mut CellSpec) {
    if let Some(text) = named(n, a, "tcTxStyle") {
        if let Some(b) = attr(text, "b") {
            out.bold = Some(matches!(b, "1" | "true" | "on"));
        }
        if let Some(fill) = named(text, a, "solidFill")
            && let Some(color) = fill
                .children
                .first()
                .and_then(|c| theme::color(c, theme, cmap, None))
        {
            out.color = Some(color.0);
        }
    }
    if let Some(cell) = named(n, a, "tcStyle")
        && let Some(fill) = cell
            .children
            .iter()
            .find(|c| matches!(c.local_name.as_str(), "solidFill" | "gradFill" | "noFill"))
        && let Fill::Solid(color, _) = style::fill(fill, theme, cmap, &mut |_, _| None)
    {
        out.fill = Some(color);
    }
}

fn builtin_style(id: &str, theme: &Theme) -> TableStyleSpec {
    let accent = *theme
        .palette
        .get("accent1")
        .unwrap_or(&crate::look::Rgb(0x4F81BD));
    let light = mix(accent, crate::look::Rgb::WHITE, 0.8);
    match id {
        "{5C22544A-7EE6-4342-B048-85BDC9FD1C3A}" => TableStyleSpec {
            first_row: CellSpec {
                fill: Some(accent),
                color: Some(crate::look::Rgb::WHITE),
                bold: Some(true),
            },
            band1: CellSpec {
                fill: Some(light),
                ..CellSpec::default()
            },
            ..TableStyleSpec::default()
        },
        _ => TableStyleSpec {
            whole: CellSpec {
                fill: Some(crate::look::Rgb::WHITE),
                ..CellSpec::default()
            },
            first_row: CellSpec {
                fill: Some(light),
                bold: Some(true),
                ..CellSpec::default()
            },
            ..TableStyleSpec::default()
        },
    }
}

fn mix(a: crate::look::Rgb, b: crate::look::Rgb, amount: f32) -> crate::look::Rgb {
    let lerp = |x: u32, y: u32| {
        (x as f32 + (y as f32 - x as f32) * amount)
            .round()
            .clamp(0.0, 255.0) as u32
    };
    crate::look::Rgb(
        lerp((a.0 >> 16) & 255, (b.0 >> 16) & 255) << 16
            | lerp((a.0 >> 8) & 255, (b.0 >> 8) & 255) << 8
            | lerp(a.0 & 255, b.0 & 255),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    fn build(path: &Path, slide: &str) {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let rel = "http://schemas.openxmlformats.org/package/2006/relationships";
        for (name, xml) in [
            ("[Content_Types].xml", "<Types/>".into()),
            (
                "ppt/presentation.xml",
                format!(
                    r#"<p:presentation xmlns:p="{P}" xmlns:r="{R}"><p:sldIdLst><p:sldId r:id="s"/></p:sldIdLst></p:presentation>"#
                ),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                format!(
                    r#"<Relationships xmlns="{rel}"><Relationship Id="s" Type="{R}/slide" Target="slides/s.xml"/></Relationships>"#
                ),
            ),
            ("ppt/slides/s.xml", slide.to_owned()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn changes_preserve_unmodeled_runs_and_rebase_ids() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("initial.pptx");
        let slide = format!(
            r#"<p:sld xmlns:p="{P}" xmlns:a="{A}" show="0"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:rPr b="1" lang="zh-CN"><a:latin typeface="Font"/></a:rPr><a:t>原文</a:t></a:r><a:r><a:rPr lang="en-US"/><a:t> untouched &amp; tail </a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
        );
        build(&input, &slide);
        let pkg = Package::open(&input).unwrap();
        let (mut deck, bind) = SlidesBinding::load(&pkg).unwrap();
        assert!(deck.slides()[0].hidden);
        let para = &deck.slides()[0].shapes[0].paras[0];
        let id = para.id;
        let mut edited = para.inlines.clone();
        if let Inline::Text { text, .. } = &mut edited[0] {
            *text = "新字 <".into();
        }
        let _inverse = deck
            .apply(crate::SlideOp::SetContent {
                para: id,
                inlines: edited.clone(),
            })
            .unwrap();
        let changes = bind.changes(&pkg, &deck).unwrap();
        assert_eq!(
            changes[0].1,
            slide.replace("原文", "新字 &lt;").into_bytes()
        );
        let output = temp.path().join("out.pptx");
        pkg.save_replaced(&output, &changes).unwrap();
        let next = Package::open(&output).unwrap();
        let rebased = SlidesBinding::rebase(&next, &deck).unwrap();
        assert!(rebased.changes(&next, &deck).unwrap().is_empty());
        if let Inline::Text { text, marks } = &mut edited[0] {
            *text = "第二次".into();
            marks.italic = true;
        }
        deck.apply(crate::SlideOp::SetContent {
            para: id,
            inlines: edited,
        })
        .unwrap();
        let new = rebased.changes(&next, &deck).unwrap();
        let xml = std::str::from_utf8(&new[0].1).unwrap();
        assert!(
            xml.contains("第二次") && xml.contains("i=\"1\"") && xml.contains("typeface=\"Font\"")
        );
        assert!(
            xml.contains(r#"<a:r><a:rPr lang="en-US"/><a:t> untouched &amp; tail </a:t></a:r>"#)
        );
    }

    #[test]
    fn fixture_noop_has_no_changes() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/writer-corpus/python/atrium-slides.pptx");
        let pkg = Package::open(&path).unwrap();
        let (deck, bind) = SlidesBinding::load(&pkg).unwrap();
        eprintln!(
            "shapes {:?}",
            deck.slides()[0]
                .shapes
                .iter()
                .map(|s| (&s.kind, s.frame))
                .collect::<Vec<_>>()
        );
        let changes = bind.changes(&pkg, &deck).unwrap();
        eprintln!("changes {}", changes.len());
        assert!(changes.is_empty());
    }
}
