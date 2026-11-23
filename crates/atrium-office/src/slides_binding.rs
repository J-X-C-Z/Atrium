//! PowerPoint OPC binding for the GPUI-free upstream model.
use crate::package::Package;
use crate::xml::{XmlNode, XmlPart};
use crate::{
    Align, Deck, Inline, Marks, NodeId, OpenError, Para, ParaStyle, SaveError, Shape, Slide,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ops::Range,
};

const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const PS: &str = "http://purl.oclc.org/ooxml/presentationml/main";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const AS: &str = "http://purl.oclc.org/ooxml/drawingml/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const RS: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships";

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
}

fn named<'n>(n: &'n XmlNode, ns: &str, name: &str) -> Option<&'n XmlNode> {
    n.children.iter().find(|n| n.is(ns, name))
}
fn attr<'n>(n: &'n XmlNode, key: &str) -> Option<&'n str> {
    n.attr(None, key)
}
fn damaged(why: &str) -> OpenError {
    OpenError::Damaged(why.into())
}
fn points(n: Option<&str>) -> i32 {
    n.and_then(|n| n.parse::<i64>().ok()).map_or(0, |x| {
        (x / 12700).clamp(i32::MIN as i64, i32::MAX as i64) as i32
    })
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
        let size = named(root, p, "sldSz").map_or((720, 540), |n| {
            (points(attr(n, "cx")), points(attr(n, "cy")))
        });
        let rels: HashMap<_, _> = package
            .relationships(&main)?
            .into_iter()
            .map(|r| (r.id.clone(), r))
            .collect();
        let mut bind = Self {
            parts: Vec::new(),
            binding: HashMap::new(),
        };
        let mut slides = Vec::new();
        let mut next = 1;
        let mut seen = HashSet::new();
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
                let root = xml.root();
                if !root.is(p, "sld") {
                    return Err(damaged("invalid slide root"));
                }
                let hidden = matches!(attr(root, "show"), Some("0" | "false"));
                let mut shapes = Vec::new();
                bind.parts.push(rel.target.clone());
                if let Some(tree) = named(root, p, "cSld").and_then(|n| named(n, p, "spTree")) {
                    bind.collect(tree, p, a, xml.bytes(), &mut next, &mut shapes);
                }
                slides.push(Slide {
                    hidden,
                    size,
                    shapes,
                    background: None,
                });
            }
        }
        Ok((Deck::from_slides(slides), bind))
    }

    fn collect(
        &mut self,
        tree: &XmlNode,
        p: &str,
        a: &str,
        raw: &[u8],
        next: &mut NodeId,
        out: &mut Vec<Shape>,
    ) {
        for n in &tree.children {
            if n.is(p, "grpSp") {
                self.collect(n, p, a, raw, next, out);
                continue;
            }
            if n.is(p, "sp") {
                if let Some(b) = named(n, p, "txBody") {
                    let x = named(n, p, "spPr").and_then(|p| named(p, a, "xfrm"));
                    out.push(self.shape(b, frame(x, a), a, raw, next));
                }
            } else if n.is(p, "graphicFrame") {
                let f = frame(named(n, p, "xfrm"), a);
                if let Some(table) = named(n, a, "graphic")
                    .and_then(|x| named(x, a, "graphicData"))
                    .and_then(|x| named(x, a, "tbl"))
                {
                    for row in table.children.iter().filter(|x| x.is(a, "tr")) {
                        for cell in row.children.iter().filter(|x| x.is(a, "tc")) {
                            if let Some(b) = named(cell, a, "txBody") {
                                out.push(self.shape(b, f, a, raw, next));
                            }
                        }
                    }
                }
            }
        }
    }
    fn shape(
        &mut self,
        body: &XmlNode,
        frame: [i32; 4],
        a: &str,
        raw: &[u8],
        next: &mut NodeId,
    ) -> Shape {
        let id = *next;
        *next += 1;
        let mut paras = Vec::new();
        for p in body.children.iter().filter(|x| x.is(a, "p")) {
            let id = *next;
            *next += 1;
            let mut pieces = Vec::new();
            let mut locked = false;
            let mut seen_text = false;
            let mut end_run = false;
            for c in &p.children {
                if c.is(a, "pPr") {
                    if seen_text || end_run {
                        locked = true;
                    }
                    continue;
                }
                if c.is(a, "endParaRPr") {
                    end_run = true;
                    continue;
                }
                if end_run {
                    locked = true;
                }
                seen_text = true;
                if c.is(a, "r") || c.is(a, "fld") {
                    if c.is(a, "fld") || c.children.iter().any(|n| !n.is(a, "rPr") && !n.is(a, "t"))
                    {
                        locked = true;
                    }
                    if let Some(t) = named(c, a, "t") {
                        let bounds = text_range(raw, t);
                        if bounds.is_none() {
                            locked = true;
                        }
                        let marks = read_marks(named(c, a, "rPr"));
                        pieces.push(Piece {
                            view: Inline::Text {
                                text: t.text.clone(),
                                marks,
                            },
                            raw: c.range.clone(),
                            text: bounds,
                            props: named(c, a, "rPr").map(|n| n.range.clone()),
                        });
                    } else {
                        locked = true;
                    }
                } else if c.is(a, "br") {
                    pieces.push(Piece {
                        view: Inline::Break,
                        raw: c.range.clone(),
                        text: None,
                        props: None,
                    });
                } else {
                    locked = true;
                }
            }
            let align = match named(p, a, "pPr").and_then(|p| attr(p, "algn")) {
                Some("ctr") => Align::Center,
                Some("r") => Align::Right,
                Some("just") => Align::Justify,
                _ => Align::Left,
            };
            let has_gaps = pieces.windows(2).any(|w| {
                raw[w[0].raw.end..w[1].raw.start]
                    .iter()
                    .any(|b| !b.is_ascii_whitespace())
            });
            locked |= has_gaps;
            let open = &raw[p.range.clone()];
            let prefix = qualified_prefix(open);
            let inlines = pieces.iter().map(|r| r.view.clone()).collect();
            let range = pieces
                .first()
                .zip(pieces.last())
                .map(|(l, r)| l.raw.start..r.raw.end)
                .or_else(|| {
                    named(p, a, "endParaRPr")
                        .map(|end| end.range.start..end.range.start)
                        .or_else(|| {
                            let close = open.windows(2).rposition(|b| b == b"</")? + p.range.start;
                            Some(close..close)
                        })
                });
            locked |= range.is_none();
            if let Some(range) = range {
                self.binding.insert(
                    id,
                    Binding {
                        part: self.parts.len() - 1,
                        range,
                        pieces,
                        prefix,
                    },
                );
            }
            paras.push(Para {
                id,
                style: ParaStyle::Normal,
                align,
                inlines,
                locked,
                look: Default::default(),
            });
        }
        Shape {
            id,
            frame,
            paras,
            look: Default::default(),
            kind: Default::default(),
        }
    }

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
            .flat_map(|s| &s.paras)
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
            XmlPart::parse(raw.clone()).map_err(|e| SaveError::Io(e.to_string()))?;
            changes.push((name.clone(), raw));
        }
        Ok(changes)
    }
}

fn frame(n: Option<&XmlNode>, a: &str) -> [i32; 4] {
    let off = n.and_then(|n| named(n, a, "off"));
    let ext = n.and_then(|n| named(n, a, "ext"));
    [
        points(off.and_then(|n| attr(n, "x"))),
        points(off.and_then(|n| attr(n, "y"))),
        points(ext.and_then(|n| attr(n, "cx"))),
        points(ext.and_then(|n| attr(n, "cy"))),
    ]
}

fn read_marks(r: Option<&XmlNode>) -> Marks {
    let field = |k| r.and_then(|n| attr(n, k));
    Marks {
        bold: matches!(field("b"), Some("1" | "true")),
        italic: matches!(field("i"), Some("1" | "true")),
        underline: field("u").is_some_and(|s| s != "none"),
        strike: matches!(field("strike"), Some("sngStrike" | "dblStrike")),
        link: None,
        ..Marks::default()
    }
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
            // Match unchanged local runs first. A bounded search prevents expensive
            // scans of adversarially long paragraphs.
            let end = (i + 16).min(old.len() - suffix);
            let exact = (i..end).find(|&k| &old[k].view == new);
            let at = exact
                .or_else(|| {
                    let Inline::Text { marks, .. } = new else {
                        return None;
                    };
                    (i..end).find(
                    |&k| matches!(&old[k].view,Inline::Text{marks:previous,..} if marks==previous),
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
                Inline::Text { text, marks } => {
                    if marks.link.is_some() {
                        return Err(SaveError::Unsupported);
                    }
                    // Do not synthesize a restyled run when that would discard
                    // the template's font, language, color, hyperlinks or extensions.
                    if old
                        .get(i)
                        .is_some_and(|p| matches!(p.view, Inline::Text { .. }))
                    {
                        return Err(SaveError::Unsupported);
                    }
                    let (b, i, u, s) = (
                        u8::from(marks.bold),
                        u8::from(marks.italic),
                        if marks.underline { "sng" } else { "none" },
                        if marks.strike {
                            "sngStrike"
                        } else {
                            "noStrike"
                        },
                    );
                    out.extend_from_slice(format!("<{q}r><{q}rPr b=\"{b}\" i=\"{i}\" u=\"{u}\" strike=\"{s}\"/><{q}t>{}</{q}t></{q}r>",escape_text(text)?).as_bytes());
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

impl SlidesBinding {
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
                if left.frame != right.frame || left.paras.len() != right.paras.len() {
                    return Err(damaged("slide shape changed on save"));
                }
                for (left, right) in left.paras.iter().zip(&right.paras) {
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

fn mark_patch(raw: &[u8], r: &Piece, marks: &Marks) -> Result<(Range<usize>, Vec<u8>), SaveError> {
    let values = [
        ("b", if marks.bold { "1" } else { "0" }),
        ("i", if marks.italic { "1" } else { "0" }),
        ("u", if marks.underline { "sng" } else { "none" }),
        (
            "strike",
            if marks.strike {
                "sngStrike"
            } else {
                "noStrike"
            },
        ),
    ];
    let mut attrs = String::new();
    for (k, v) in values {
        attrs.push_str(&format!(" {k}=\"{v}\""));
    }
    if let Some(rp) = &r.props {
        let src = &raw[rp.clone()];
        let end = open_end(src)?;
        // Retain the exact bytes of unmodeled attributes.
        let mut output = src[..1 + name(src).len()].to_vec();
        let mut i = output.len();
        while i < end {
            let start = i;
            while i < end && src[i].is_ascii_whitespace() {
                i += 1;
            }
            if i == end || src[i] == b'/' {
                output.extend_from_slice(attrs.as_bytes());
                output.extend_from_slice(&src[start..=end]);
                break;
            }
            let k = i;
            while i < end && !src[i].is_ascii_whitespace() && src[i] != b'=' {
                i += 1;
            }
            let key = &src[k..i];
            while i < end && (src[i].is_ascii_whitespace() || src[i] == b'=') {
                i += 1;
            }
            let q = *src
                .get(i)
                .filter(|&&b| matches!(b, b'\'' | b'"'))
                .ok_or(SaveError::Unsupported)?;
            i += 1;
            while i < end && src[i] != q {
                i += 1;
            }
            if i == end {
                return Err(SaveError::Unsupported);
            }
            i += 1;
            if !values.iter().any(|(k, _)| k.as_bytes() == key) {
                output.extend_from_slice(&src[start..i]);
            }
        }
        if i == end && output.last() != Some(&b'>') {
            output.extend_from_slice(attrs.as_bytes());
            output.push(b'>');
        }
        return Ok((rp.start..rp.start + end + 1, output));
    }
    let src = &raw[r.raw.clone()];
    let end = open_end(src)?;
    let q = name(src);
    let prefix = q
        .iter()
        .rposition(|&b| b == b':')
        .map(|i| &q[..=i])
        .unwrap_or_default();
    let mut output = b"<".to_vec();
    output.extend_from_slice(prefix);
    output.extend_from_slice(b"rPr");
    output.extend_from_slice(attrs.as_bytes());
    output.extend_from_slice(b"/>");
    Ok((r.raw.start + end + 1..r.raw.start + end + 1, output))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn changes_preserve_unmodeled_runs_and_rebase_ids() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("initial.pptx");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&input).unwrap());
        let rel = "http://schemas.openxmlformats.org/package/2006/relationships";
        let slide = format!(
            r#"<p:sld xmlns:p="{P}" xmlns:a="{A}" show="0"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:rPr b="1" lang="zh-CN"><a:latin typeface="Font"/></a:rPr><a:t>原文</a:t></a:r><a:r><a:rPr lang="en-US"/><a:t> untouched &amp; tail </a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
        );
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
            ("ppt/slides/s.xml", slide.clone()),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
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
}
