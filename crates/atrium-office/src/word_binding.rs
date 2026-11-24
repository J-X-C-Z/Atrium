//! Read-only WordprocessingML projection. The original package remains the
//! authority for formatting, unknown content, and any later save.
use crate::word_looks::Looks;
use crate::{
    Align, Atom, AtomKind, Block, Footnote, Inline, Marks, NodeId, OpenError, Package, Para,
    ParaStyle, Table, WordDoc,
    xml::{XmlNode, XmlPart},
};
use std::collections::{HashMap, HashSet};

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const WS: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const RS: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships";
const OPC_REL: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const OPC_REL_STRICT: &str = "http://purl.oclc.org/ooxml/package/relationships";
const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
const MS: &str = "http://purl.oclc.org/ooxml/officeDocument/math";

#[allow(clippy::too_many_arguments)]
fn project(
    document: &XmlPart,
    styles: Option<&XmlPart>,
    numbering: Option<&XmlPart>,
    theme: Option<&XmlPart>,
    relationships: Option<&XmlPart>,
    images: &HashMap<String, std::path::PathBuf>,
    footnotes: Option<&XmlPart>,
) -> Result<WordDoc, OpenError> {
    let root = document.root();
    let ns = root
        .namespace
        .as_deref()
        .filter(|ns| *ns == W || *ns == WS)
        .ok_or_else(|| {
            OpenError::Damaged("Word document has no WordprocessingML namespace".into())
        })?;
    if !root.is(ns, "document") {
        return Err(OpenError::Damaged("missing Word document root".into()));
    }
    let body = child(root, ns, "body")
        .ok_or_else(|| OpenError::Damaged("missing Word document body".into()))?;
    let styles = styles.map(XmlPart::root);
    let numbering = numbering.map(XmlPart::root);
    let mut cx = Context::new(ns, styles, numbering, theme);
    cx.images = Some(images);
    if let Some(rels) = relationships {
        let root = rels.root();
        let ns = rel_namespace(root)
            .ok_or_else(|| OpenError::Damaged("invalid Word relationships root".into()))?;
        let mut seen = HashSet::new();
        for rel in root.children.iter().filter(|n| n.is(ns, "Relationship")) {
            let id = rel
                .attr(None, "Id")
                .filter(|id| !id.is_empty())
                .ok_or_else(|| OpenError::Damaged("missing relationship ID".into()))?;
            if !seen.insert(id) {
                return Err(OpenError::Damaged("duplicate relationship ID".into()));
            }
            let hyperlink = rel.attr(None, "Type").is_some_and(|ty| {
                ty.strip_suffix("/hyperlink")
                    .is_some_and(|ns| ns == R || ns == RS)
            });
            if hyperlink
                && rel.attr(None, "TargetMode") == Some("External")
                && let Some(target) = rel.attr(None, "Target").filter(|url| allowed_link(url))
            {
                cx.links.insert(id, target);
            }
        }
    }
    let defaults = cx.looks.defaults();
    let mut blocks = Vec::new();
    let mut sections = Vec::new();
    for node in &body.children {
        if node.is(ns, "sectPr") {
            sections.push(section(node, ns, None));
            continue;
        }
        let block = cx.block(node);
        if node.is(ns, "p")
            && let Some(sect) = child(node, ns, "pPr").and_then(|p| child(p, ns, "sectPr"))
        {
            sections.push(section(sect, ns, Some(block.id())));
        }
        blocks.push(block);
    }
    let footnotes = footnotes
        .map(|part| load_footnotes(part, ns, styles, numbering, theme, body))
        .unwrap_or_default();
    Ok(WordDoc::from_blocks(blocks)
        .with_look(sections, defaults)
        .with_footnotes(footnotes))
}

/// The footnote body is joined to the reference atom by the same byte-offset
/// id scheme the rest of the document uses. `WordDoc::footnotes()` keeps the
/// blocks keyed by the atom that refers to them.
fn load_footnotes(
    part: &XmlPart,
    ns: &str,
    styles: Option<&XmlNode>,
    numbering: Option<&XmlNode>,
    theme: Option<&XmlPart>,
    body: &XmlNode,
) -> Vec<Footnote> {
    let root = part.root();
    if !root.is(ns, "footnotes") && !root.is(ns, "endnotes") {
        return Vec::new();
    }
    let mut references = Vec::new();
    collect_note_references(body, ns, &mut references);
    if references.is_empty() {
        return Vec::new();
    }
    // The layout numbers notes in document order, not by their part id.
    references.sort_by_key(|(atom, _)| *atom);
    let mut labels = HashMap::new();
    for (index, (_, note)) in references.iter().enumerate() {
        labels.entry(note.clone()).or_insert(index as u32 + 1);
    }
    let wanted: HashSet<&str> = references.iter().map(|(_, note)| note.as_str()).collect();
    let mut bodies = HashMap::new();
    for note in &root.children {
        if !(note.is(ns, "footnote") || note.is(ns, "endnote")) {
            continue;
        }
        let Some(id) = note.attr(Some(ns), "id") else {
            continue;
        };
        if id.starts_with('-') || !wanted.contains(id) {
            continue;
        }
        let mut cx = Context::new(ns, styles, numbering, theme);
        cx.id_base = (1u64 << 40) + note.range.start as u64;
        let mut blocks: Vec<Block> = note
            .children
            .iter()
            .filter(|child| child.is(ns, "p") || child.is(ns, "tbl"))
            .map(|child| cx.block(child))
            .collect();
        for block in &mut blocks {
            strip_note_marks(block);
        }
        let label = labels.get(id).copied().unwrap_or(1);
        prepend_note_label(&mut blocks, label);
        bodies.insert(id.to_owned(), blocks);
    }
    let mut out = Vec::new();
    for (atom, note) in references {
        if let Some(blocks) = bodies.get(&note) {
            out.push(Footnote {
                id: atom,
                blocks: blocks.clone(),
            });
        }
    }
    out
}

fn collect_note_references(node: &XmlNode, ns: &str, out: &mut Vec<(NodeId, String)>) {
    if node.is(ns, "footnoteReference") || node.is(ns, "endnoteReference") {
        if let Some(id) = node.attr(Some(ns), "id") {
            out.push((node.range.start as NodeId, id.to_owned()));
        }
    }
    for child in &node.children {
        collect_note_references(child, ns, out);
    }
}

fn strip_note_marks(block: &mut Block) {
    match block {
        Block::Paragraph(para) => para.inlines.retain(|inline| {
            !matches!(inline, Inline::Atom(atom) if matches!(atom.kind, AtomKind::Footnote))
        }),
        Block::Table(table) => {
            for row in &mut table.rows {
                for cell in row {
                    for block in cell {
                        strip_note_marks(block);
                    }
                }
            }
        }
        Block::Opaque { .. } => {}
    }
}

fn prepend_note_label(blocks: &mut [Block], label: u32) {
    let Some(Block::Paragraph(para)) = blocks.first_mut() else {
        return;
    };
    para.inlines.insert(
        0,
        Inline::Text {
            text: format!("{label} "),
            marks: Marks {
                baseline: crate::look::Baseline::Superscript,
                ..Marks::default()
            },
        },
    );
}

type WordParts = (String, Option<String>, Option<String>);

fn package_parts(package: &Package) -> Result<WordParts, OpenError> {
    let document = package
        .related_part("", "officeDocument")?
        .unwrap_or_else(|| "word/document.xml".into());
    let optional = |kind, conventional: &str| -> Result<Option<String>, OpenError> {
        if let Some(target) = package.related_part(&document, kind)? {
            if !package.has_part(&target) {
                return Err(OpenError::Damaged(format!(
                    "missing {kind} relationship target"
                )));
            }
            return Ok(Some(target));
        }
        Ok(package
            .has_part(conventional)
            .then(|| conventional.to_owned()))
    };
    let styles = optional("styles", "word/styles.xml")?;
    let numbering = optional("numbering", "word/numbering.xml")?;
    Ok((document, styles, numbering))
}

fn relationship_part_name(document: &str) -> String {
    if let Some((dir, file)) = document.rsplit_once('/') {
        format!("{dir}/_rels/{file}.rels")
    } else {
        format!("_rels/{document}.rels")
    }
}

#[allow(clippy::too_many_arguments)]
fn attach_headers(
    package: &Package,
    document_name: &str,
    document: &XmlPart,
    styles: Option<&XmlPart>,
    numbering: Option<&XmlPart>,
    theme: Option<&XmlPart>,
    relationships: Option<&XmlPart>,
    model: &mut WordDoc,
) {
    let Some(relationships) = relationships else {
        return;
    };
    let Some(rels_ns) = rel_namespace(relationships.root()) else {
        return;
    };
    let targets = relationships
        .root()
        .children
        .iter()
        .filter(|n| n.is(rels_ns, "Relationship"))
        .filter_map(|n| {
            Some((
                n.attr(None, "Id")?,
                (n.attr(None, "Target")?, n.attr(None, "Type")?),
            ))
        })
        .collect::<HashMap<_, _>>();
    let root = document.root();
    let Some(ns) = root.namespace.as_deref() else {
        return;
    };
    let Some(body) = child(root, ns, "body") else {
        return;
    };
    let mut sections = Vec::new();
    for node in &body.children {
        if node.is(ns, "sectPr") {
            sections.push(node);
        } else if node.is(ns, "p")
            && let Some(sect) = child(node, ns, "pPr").and_then(|p| child(p, ns, "sectPr"))
        {
            sections.push(sect);
        }
    }
    if sections.len() != model.sections.len() {
        return;
    }
    for (index, (sect, section)) in sections.into_iter().zip(&mut model.sections).enumerate() {
        let load = |kind: &str, first: bool| -> Option<Vec<Block>> {
            let reference = sect.children.iter().find(|n| {
                n.is(ns, kind)
                    && n.attr(Some(ns), "type") == Some(if first { "first" } else { "default" })
            })?;
            let id = reference
                .attr(Some(R), "id")
                .or_else(|| reference.attr(Some(RS), "id"))?;
            let (target, ty) = targets.get(id)?;
            if !(ty.ends_with("/header") || ty.ends_with("/footer")) {
                return None;
            }
            let target = if let Some(absolute) = target.strip_prefix('/') {
                absolute.to_owned()
            } else {
                document_name
                    .rsplit_once('/')
                    .map(|(dir, _)| format!("{dir}/{target}"))
                    .unwrap_or_else(|| (*target).to_owned())
            };
            let part = package.part(&target).ok()?;
            let part_root = part.root();
            if !(part_root.is(ns, "hdr") || part_root.is(ns, "ftr")) {
                return None;
            }
            let mut cx = Context::new(
                ns,
                styles.map(XmlPart::root),
                numbering.map(XmlPart::root),
                theme,
            );
            cx.id_base = (1u64 << 48) + (index as u64 + 1) * 1_000_000;
            Some(
                part_root
                    .children
                    .iter()
                    .map(|node| cx.block(node))
                    .collect(),
            )
        };
        section.header = load("headerReference", false);
        section.first_header = load("headerReference", true);
        section.footer = load("footerReference", false);
        section.first_footer = load("footerReference", true);
    }
}

fn rel_namespace(root: &XmlNode) -> Option<&str> {
    [OPC_REL, OPC_REL_STRICT]
        .into_iter()
        .find(|ns| root.is(ns, "Relationships"))
}

fn allowed_link(url: &str) -> bool {
    if url.len() > 4096
        || url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '\\' | '<' | '>'))
    {
        return false;
    }
    let Some((scheme, tail)) = url.split_once(':') else {
        return false;
    };
    if scheme.eq_ignore_ascii_case("mailto") {
        return !tail.is_empty() && !tail.starts_with('/');
    }
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return false;
    }
    tail.strip_prefix("//")
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .is_some_and(|host| !host.is_empty() && !host.contains('@'))
}

fn child<'a>(node: &'a XmlNode, ns: &str, name: &str) -> Option<&'a XmlNode> {
    node.children.iter().find(|n| n.is(ns, name))
}

fn val<'a>(node: &'a XmlNode, ns: &str, name: &str) -> Option<&'a str> {
    child(node, ns, name)?.attr(Some(ns), "val")
}

struct Context<'a> {
    ns: &'a str,
    id_base: u64,
    links: HashMap<&'a str, &'a str>,
    images: Option<&'a HashMap<String, std::path::PathBuf>>,
    styles: HashMap<&'a str, &'a XmlNode>,
    abstracts: HashMap<&'a str, &'a XmlNode>,
    numbers: HashMap<&'a str, &'a XmlNode>,
    looks: Looks<'a>,
}

impl<'a> Context<'a> {
    fn new(
        ns: &'a str,
        styles: Option<&'a XmlNode>,
        numbering: Option<&'a XmlNode>,
        theme: Option<&'a XmlPart>,
    ) -> Self {
        let index = |parent: Option<&'a XmlNode>, tag, attr| {
            parent
                .into_iter()
                .flat_map(|n| &n.children)
                .filter(|n| n.is(ns, tag))
                .filter_map(|n| Some((n.attr(Some(ns), attr)?, n)))
                .collect()
        };
        Self {
            ns,
            id_base: 0,
            links: HashMap::new(),
            images: None,
            styles: index(styles.filter(|n| n.is(ns, "styles")), "style", "styleId"),
            abstracts: index(
                numbering.filter(|n| n.is(ns, "numbering")),
                "abstractNum",
                "abstractNumId",
            ),
            numbers: index(numbering.filter(|n| n.is(ns, "numbering")), "num", "numId"),
            looks: Looks::new(ns, styles, numbering, theme.map(XmlPart::root)),
        }
    }

    fn id(&self, node: &XmlNode) -> NodeId {
        self.id_base + node.range.start as NodeId
    }

    fn style_chain(&self, id: &str) -> Vec<&'a XmlNode> {
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

    fn list_kind(&self, id: &str, level: u8) -> Option<bool> {
        let ns = self.ns;
        let num = self.numbers.get(id)?;
        let find_level = |n: &'a XmlNode, tag| {
            n.children.iter().find(|n| {
                n.is(ns, tag)
                    && n.attr(Some(ns), "ilvl").and_then(|x| x.parse::<u8>().ok()) == Some(level)
            })
        };
        let overridden = find_level(num, "lvlOverride").and_then(|n| child(n, ns, "lvl"));
        let original = val(num, ns, "abstractNumId")
            .and_then(|id| self.abstracts.get(id))
            .and_then(|n| find_level(n, "lvl"));
        let effective = overridden.or(original)?;
        match val(effective, ns, "numFmt").unwrap_or("decimal") {
            "none" => None,
            "bullet" => Some(false),
            _ => Some(true),
        }
    }

    fn block(&self, node: &XmlNode) -> Block {
        let ns = self.ns;
        if node.is(ns, "p") {
            Block::Paragraph(self.para(node))
        } else if node.is(ns, "tbl") {
            let style = child(node, ns, "tblPr")
                .and_then(|p| val(p, ns, "tblStyle"))
                .map(|id| self.style_chain(id))
                .unwrap_or_default();
            let mut merged = Vec::new();
            let rows = node
                .children
                .iter()
                .filter(|n| n.is(ns, "tr"))
                .enumerate()
                .map(|(row, tr)| {
                    tr.children
                        .iter()
                        .filter(|n| n.is(ns, "tc"))
                        .enumerate()
                        .map(|(col, tc)| {
                            if child(tc, ns, "tcPr").is_some_and(|n| {
                                child(n, ns, "vMerge").is_some()
                                    || child(n, ns, "hMerge").is_some()
                                    || val(n, ns, "gridSpan")
                                        .and_then(|v| v.parse::<u32>().ok())
                                        .is_some_and(|span| span > 1)
                            }) {
                                merged.push((row, col));
                            }
                            tc.children
                                .iter()
                                .filter(|n| !n.is(ns, "tcPr"))
                                .map(|n| self.block(n))
                                .collect()
                        })
                        .collect()
                })
                .collect();
            Block::Table(Table {
                id: self.id(node),
                rows,
                merged,
                look: self.looks.table_look(node, &style),
            })
        } else {
            Block::Opaque {
                id: self.id(node),
                label: node.local_name.clone(),
            }
        }
    }

    fn para(&self, node: &XmlNode) -> Para {
        let ns = self.ns;
        let direct = child(node, ns, "pPr");
        let id = direct
            .and_then(|n| val(n, ns, "pStyle"))
            .unwrap_or("Normal");
        let chain = self.style_chain(id);
        let property = |name| {
            direct.and_then(|n| child(n, ns, name)).or_else(|| {
                chain
                    .iter()
                    .find_map(|n| child(child(n, ns, "pPr")?, ns, name))
            })
        };
        let outline = property("outlineLvl")
            .and_then(|n| n.attr(Some(ns), "val"))
            .and_then(|v| v.parse::<u8>().ok());
        let number_property = |name| {
            std::iter::once(direct)
                .chain(chain.iter().map(|style| child(style, ns, "pPr")))
                .flatten()
                .find_map(|p| val(child(p, ns, "numPr")?, ns, name))
        };
        let num = number_property("numId").and_then(|num| {
            // A direct zero is a real override, not a missing ID to inherit.
            if num.parse::<u32>().ok()? == 0 {
                return None;
            }
            let level = number_property("ilvl")
                .and_then(|v| v.parse::<u8>().ok())
                .unwrap_or(0);
            Some(ParaStyle::ListItem {
                level,
                ordered: self.list_kind(num, level).unwrap_or(false),
            })
        });
        let style = match outline {
            Some(level @ 0..=5) => ParaStyle::Heading(level + 1),
            level => num.unwrap_or_else(|| match id {
                "Normal" => ParaStyle::Normal,
                "Title" => ParaStyle::Title,
                "Quote" | "IntenseQuote" => ParaStyle::Quote,
                id => id
                    .strip_prefix("Heading")
                    .and_then(|v| v.parse::<u8>().ok())
                    .filter(|n| level.is_none() && (1..=6).contains(n))
                    .map(ParaStyle::Heading)
                    .unwrap_or_else(|| ParaStyle::Other(id.to_owned())),
            }),
        };
        let align = match property("jc").and_then(|n| n.attr(Some(ns), "val")) {
            Some("center") => Align::Center,
            Some("right" | "end") => Align::Right,
            Some("both" | "distribute") => Align::Justify,
            _ => Align::Left,
        };
        let mut inlines = Vec::new();
        let mut locked = false;
        for n in &node.children {
            self.inline(n, &chain, None, &mut inlines, &mut locked);
        }
        let look = self.looks.para_look(direct, &chain);
        Para {
            id: self.id(node),
            style,
            align,
            inlines,
            locked,
            look,
        }
    }

    fn inline(
        &self,
        n: &XmlNode,
        para: &[&XmlNode],
        link: Option<&str>,
        out: &mut Vec<Inline>,
        locked: &mut bool,
    ) {
        let ns = self.ns;
        if [M, MS]
            .iter()
            .any(|ns| n.is(ns, "oMath") || n.is(ns, "oMathPara"))
        {
            *locked = true;
            out.push(self.atom(n, AtomKind::Other("math".into())));
            return;
        }
        if n.namespace.as_deref() != Some(ns) {
            return;
        }
        match n.local_name.as_str() {
            "r" => {
                let direct = child(n, ns, "rPr");
                let char_id = direct.and_then(|p| val(p, ns, "rStyle"));
                let char_chain = char_id.map(|id| self.style_chain(id)).unwrap_or_default();
                let mut marks = self.looks.run_marks(direct, &char_chain, para);
                marks.link = link.map(str::to_owned);
                for n in &n.children {
                    if n.is(ns, "t") || n.is(ns, "delText") {
                        out.push(Inline::Text {
                            text: n.text.clone(),
                            marks: marks.clone(),
                        });
                    } else {
                        self.inline(n, para, link, out, locked);
                    }
                }
            }
            "hyperlink" => {
                // Parse links as metadata only; nothing is fetched or launched.
                let target = n
                    .attr(Some(if self.ns == WS { RS } else { R }), "id")
                    .and_then(|id| self.links.get(id).copied());
                let anchor = n.attr(Some(ns), "anchor").map(|s| format!("#{s}"));
                for n in &n.children {
                    self.inline(n, para, target.or(anchor.as_deref()), out, locked);
                }
            }
            "ins" | "del" | "moveFrom" | "moveTo" | "sdt" | "sdtContent" => {
                *locked = true;
                for n in &n.children {
                    self.inline(n, para, link, out, locked);
                }
            }
            "commentRangeStart" | "commentRangeEnd" | "commentReference" | "fldChar"
            | "instrText" => {
                *locked = true;
                if n.is(ns, "fldChar") {
                    out.push(self.atom(n, AtomKind::Field(String::new())));
                }
            }
            "fldSimple" => {
                let instruction = n.attr(Some(ns), "instr").unwrap_or_default().to_string();
                out.push(self.atom(n, AtomKind::Field(instruction)));
            }
            "bookmarkStart" | "bookmarkEnd" => {
                out.push(self.atom(n, AtomKind::Other("bookmark".into())))
            }
            "footnoteReference" | "endnoteReference" => out.push(self.atom(n, AtomKind::Footnote)),
            "drawing" | "pict" => {
                let display = image_ids(n).find_map(|id| self.images?.get(id).cloned());
                out.push(self.atom(n, AtomKind::Image(display)));
            }
            "object" | "sym" => out.push(self.atom(n, AtomKind::Other(n.local_name.clone()))),
            "br" | "cr" => out.push(Inline::Break),
            "tab" => out.push(Inline::Tab),
            _ => {}
        }
    }

    fn atom(&self, node: &XmlNode, kind: AtomKind) -> Inline {
        let look = match &kind {
            AtomKind::Image(_) => drawing_look(node),
            AtomKind::Field(_) => crate::look::AtomLook {
                shown: field_result(node),
                ..Default::default()
            },
            _ => crate::look::AtomLook::default(),
        };
        Inline::Atom(Atom {
            id: self.id(node),
            kind,
            look,
        })
    }
}

const DRAWING: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const DRAWING_STRICT: &str = "http://purl.oclc.org/ooxml/drawingml/main";
const DRAWING_WORD: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
const DRAWING_WORD_STRICT: &str = "http://purl.oclc.org/ooxml/drawingml/wordprocessingDrawing";
const VML: &str = "urn:schemas-microsoft-com:vml";

fn image_ids(node: &XmlNode) -> impl Iterator<Item = &str> {
    let mut pending = vec![node];
    std::iter::from_fn(move || {
        while let Some(n) = pending.pop() {
            pending.extend(n.children.iter().rev());
            let rs = if n.is(DRAWING, "blip") || n.is(DRAWING_STRICT, "blip") {
                "embed"
            } else if n.is(VML, "imagedata") {
                "id"
            } else {
                continue;
            };
            if let Some(id) = n.attr(Some(R), rs).or_else(|| n.attr(Some(RS), rs)) {
                return Some(id);
            }
        }
        None
    })
}

fn safe_png(bytes: &[u8]) -> bool {
    if bytes.len() < 33 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR") {
        return false;
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("IHDR width"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("IHDR height"));
    !bytes.windows(4).any(|w| w == b"acTL")
        && width > 0
        && height > 0
        && width <= 8192
        && height <= 8192
        && u64::from(width) * u64::from(height) <= 16_000_000
}

fn extract_images(
    package: &Package,
    document: &XmlPart,
) -> (
    Option<tempfile::TempDir>,
    HashMap<String, std::path::PathBuf>,
) {
    let referenced = image_ids(document.root()).collect::<HashSet<_>>();
    let mut paths = HashMap::new();
    if referenced.is_empty() {
        return (None, paths);
    }
    let Ok(available) = crate::media::embedded_word_images(package) else {
        return (None, paths);
    };
    let mut dir = None;
    let mut remaining: usize = 32 * 1024 * 1024;
    for reference in available
        .into_iter()
        .filter(|r| referenced.contains(r.relationship_id()))
        .take(64)
    {
        if remaining == 0 {
            break;
        }
        if !reference
            .part_name()
            .rsplit_once('.')
            .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("png"))
        {
            continue;
        }
        let Ok(bytes) = crate::media::read_image(package, &reference) else {
            continue;
        };
        // Charge all reads, even unsupported or corrupt referenced images.
        let Some(next) = remaining.checked_sub(bytes.len()) else {
            break;
        };
        remaining = next;
        if !safe_png(&bytes) {
            continue;
        }
        if dir.is_none() {
            dir = tempfile::tempdir().ok();
        }
        let Some(folder) = dir.as_ref() else {
            break;
        };
        let path = folder.path().join(format!("image-{}.png", paths.len()));
        if std::fs::write(&path, bytes).is_ok() {
            paths.insert(reference.relationship_id().to_owned(), path);
        }
    }
    (dir, paths)
}

fn section(node: &XmlNode, ns: &str, last_block: Option<NodeId>) -> crate::look::Section {
    let mut page = crate::look::PageLook::default();
    if let Some(size) = child(node, ns, "pgSz") {
        if let Some(v) = size.attr(Some(ns), "w").and_then(|v| v.parse::<i32>().ok()) {
            page.width = v;
        }
        if let Some(v) = size.attr(Some(ns), "h").and_then(|v| v.parse::<i32>().ok()) {
            page.height = v;
        }
    }
    if let Some(margin) = child(node, ns, "pgMar") {
        let read = |name| {
            margin
                .attr(Some(ns), name)
                .and_then(|v| v.parse::<i32>().ok())
        };
        if let Some(v) = read("top") {
            page.top = v;
        }
        if let Some(v) = read("bottom") {
            page.bottom = v;
        }
        if let Some(v) = read("left") {
            page.left = v;
        }
        if let Some(v) = read("right") {
            page.right = v;
        }
        if let Some(v) = read("header") {
            page.header = v;
        }
        if let Some(v) = read("footer") {
            page.footer = v;
        }
        if let Some(v) = read("gutter") {
            page.gutter = v;
        }
    }
    if let Some(cols) = child(node, ns, "cols") {
        page.columns = cols
            .attr(Some(ns), "num")
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(1)
            .max(1);
        if let Some(v) = cols
            .attr(Some(ns), "space")
            .and_then(|v| v.parse::<i32>().ok())
        {
            page.column_gap = v;
        }
    }
    if let Some(grid) = child(node, ns, "docGrid")
        && let Some(v) = grid
            .attr(Some(ns), "linePitch")
            .and_then(|v| v.parse::<i32>().ok())
    {
        page.line_pitch = Some(v);
    }
    let new_page =
        child(node, ns, "type").and_then(|n| n.attr(Some(ns), "val")) != Some("continuous");
    let title_page = child(node, ns, "titlePg").is_some();
    let page_start = child(node, ns, "pgNumType")
        .and_then(|n| n.attr(Some(ns), "start"))
        .and_then(|v| v.parse::<u32>().ok());
    crate::look::Section {
        last_block,
        page,
        title_page,
        page_start,
        new_page,
        ..Default::default()
    }
}

fn drawing_look(node: &XmlNode) -> crate::look::AtomLook {
    let mut look = crate::look::AtomLook::default();
    let mut pending = vec![node];
    while let Some(n) = pending.pop() {
        pending.extend(n.children.iter().rev());
        // `wp:extent` is in the wordprocessingDrawing namespace, not the
        // DrawingML one. Looking only for `a:extent` missed every picture
        // size Word writes, so pictures were laid out 3 lines tall.
        if (n.is(DRAWING, "extent")
            || n.is(DRAWING_STRICT, "extent")
            || n.is(DRAWING_WORD, "extent")
            || n.is(DRAWING_WORD_STRICT, "extent"))
            && look.size.is_none()
        {
            let cx = n.attr(None, "cx").and_then(|v| v.parse::<i64>().ok());
            let cy = n.attr(None, "cy").and_then(|v| v.parse::<i64>().ok());
            if let (Some(cx), Some(cy)) = (cx, cy) {
                look.size = Some((
                    (cx / 635).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                    (cy / 635).clamp(i32::MIN as i64, i32::MAX as i64) as i32,
                ));
            }
        }
        if (n.local_name == "anchor" && n.namespace.as_deref().is_some()) && look.float.is_none() {
            let mut float = crate::look::Float {
                x: 0,
                from_x: crate::look::Origin::Page,
                y: 0,
                from_y: crate::look::Origin::Page,
                wrap: crate::look::Wrap::TopAndBottom,
                clear: [0; 4],
            };
            // The clearance distances are attributes of `wp:anchor` (and
            // some producers repeat them on `wp:wrapSquare`), not child
            // elements.
            read_clears(&n.attributes, &mut float.clear);
            for c in &n.children {
                let origin = |s: Option<&str>| match s {
                    Some("page") => crate::look::Origin::Page,
                    Some("margin") => crate::look::Origin::Margin,
                    Some("paragraph") => crate::look::Origin::Paragraph,
                    Some("column") => crate::look::Origin::Column,
                    _ => crate::look::Origin::Page,
                };
                match c.local_name.as_str() {
                    "positionH" => {
                        float.from_x = origin(c.attr(None, "relativeFrom"));
                        float.x = c
                            .children
                            .iter()
                            .find(|x| x.local_name == "posOffset")
                            .and_then(|x| x.text.trim().parse::<i64>().ok())
                            .map_or(0, |v| {
                                (v / 635).clamp(i32::MIN as i64, i32::MAX as i64) as i32
                            });
                    }
                    "positionV" => {
                        float.from_y = origin(c.attr(None, "relativeFrom"));
                        float.y = c
                            .children
                            .iter()
                            .find(|x| x.local_name == "posOffset")
                            .and_then(|x| x.text.trim().parse::<i64>().ok())
                            .map_or(0, |v| {
                                (v / 635).clamp(i32::MIN as i64, i32::MAX as i64) as i32
                            });
                    }
                    "wrapSquare" => {
                        float.wrap = crate::look::Wrap::Square;
                        read_clears(&c.attributes, &mut float.clear);
                    }
                    "wrapTopAndBottom" => float.wrap = crate::look::Wrap::TopAndBottom,
                    "wrapNone" => {
                        float.wrap = if c.attr(None, "behindDoc") == Some("1") {
                            crate::look::Wrap::Behind
                        } else {
                            crate::look::Wrap::InFront
                        }
                    }
                    "distT" | "distR" | "distB" | "distL" => {
                        let slot = match c.local_name.as_str() {
                            "distT" => 0,
                            "distR" => 1,
                            "distB" => 2,
                            _ => 3,
                        };
                        float.clear[slot] = c
                            .attr(None, "val")
                            .and_then(|v| v.parse::<i64>().ok())
                            .map_or(0, |v| {
                                (v / 635).clamp(i32::MIN as i64, i32::MAX as i64) as i32
                            });
                    }
                    _ => {}
                }
            }
            look.float = Some(float);
        }
    }
    look
}

fn read_clears(attributes: &[crate::xml::XmlAttribute], clear: &mut [i32; 4]) {
    for attribute in attributes {
        let slot = match attribute.local_name.as_str() {
            "distT" => 0,
            "distR" => 1,
            "distB" => 2,
            "distL" => 3,
            _ => continue,
        };
        if let Ok(value) = attribute.value.trim().parse::<i64>() {
            clear[slot] = (value / 635).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        }
    }
}

fn field_result(node: &XmlNode) -> Option<String> {
    let mut text = String::new();
    let mut pending = vec![node];
    while let Some(n) = pending.pop() {
        pending.extend(n.children.iter().rev());
        if n.local_name == "t" {
            text.push_str(&n.text);
        }
    }
    (!text.is_empty()).then_some(text)
}
fn edit_para_properties(
    source: &[u8],
    para: &XmlNode,
    ns: &str,
    style: Option<&str>,
    number: Option<(&str, u8)>,
    outline: Option<&str>,
    look: Option<&crate::look::ParaLook>,
) -> Result<Vec<u8>, EditError> {
    let mut operations = Vec::<(usize, usize, Vec<u8>)>::new();
    let mut additions = Vec::<(&str, Vec<u8>)>::new();
    let props = child(para, ns, "pPr");
    let rank = |name: &str| {
        [
            "pStyle",
            "keepNext",
            "keepLines",
            "pageBreakBefore",
            "framePr",
            "widowControl",
            "numPr",
            "suppressLineNumbers",
            "pBdr",
            "shd",
            "tabs",
            "suppressAutoHyphens",
            "kinsoku",
            "wordWrap",
            "overflowPunct",
            "topLinePunct",
            "autoSpaceDE",
            "autoSpaceDN",
            "bidi",
            "adjustRightInd",
            "snapToGrid",
            "spacing",
            "ind",
            "contextualSpacing",
            "mirrorIndents",
            "suppressOverlap",
            "jc",
            "textDirection",
            "textAlignment",
            "textboxTightWrap",
            "outlineLvl",
            "divId",
            "cnfStyle",
            "rPr",
            "sectPr",
            "pPrChange",
        ]
        .iter()
        .position(|n| *n == name)
        .unwrap_or(usize::MAX)
    };
    let mut push = |name, markup: String| -> Result<(), EditError> {
        if let Some(old) = props.and_then(|p| child(p, ns, name)) {
            reject_unprojected_markup(&source[old.range.clone()])?;
            if name == "numPr" {
                if !old.attributes.is_empty()
                    || !old.text.trim().is_empty()
                    || old.children.iter().any(|n| {
                        !(n.is(ns, "numId") || n.is(ns, "ilvl"))
                            || !n.children.is_empty()
                            || !n.text.trim().is_empty()
                            || n.attributes.iter().any(|a| {
                                a.namespace.as_deref() != Some(ns) || a.local_name != "val"
                            })
                    })
                {
                    return Err(EditError::Locked);
                }
            } else if matches!(name, "spacing" | "ind" | "shd") {
                if !old.children.is_empty()
                    || !old.text.trim().is_empty()
                    || old
                        .attributes
                        .iter()
                        .any(|a| a.namespace.as_deref() != Some(ns))
                {
                    return Err(EditError::Locked);
                }
            } else if !old.children.is_empty()
                || !old.text.trim().is_empty()
                || old
                    .attributes
                    .iter()
                    .any(|a| a.namespace.as_deref() != Some(ns) || a.local_name != "val")
            {
                return Err(EditError::Locked);
            }
            operations.push((old.range.start, old.range.end, markup.into_bytes()));
        } else {
            additions.push((name, markup.into_bytes()));
        }
        Ok(())
    };
    if let Some(style) = style {
        let style = quick_xml::escape::escape(style);
        push(
            "pStyle",
            format!(r#"<w:pStyle xmlns:w="{ns}" w:val="{style}"/>"#),
        )?;
    }
    if let Some((id, level)) = number {
        push(
            "numPr",
            format!(
                r#"<w:numPr xmlns:w="{ns}"><w:ilvl w:val="{level}"/><w:numId w:val="{id}"/></w:numPr>"#
            ),
        )?;
    }
    if let Some(outline) = outline {
        push(
            "outlineLvl",
            format!(r#"<w:outlineLvl xmlns:w="{ns}" w:val="{outline}"/>"#),
        )?;
    }
    if let Some(look) = look {
        let mut spacing = String::new();
        if look.before != 0 {
            spacing.push_str(&format!(r#" w:before="{}""#, look.before));
        }
        if look.after != 0 {
            spacing.push_str(&format!(r#" w:after="{}""#, look.after));
        }
        match look.line {
            crate::look::LineSpacing::Auto(v) if v != 240 => {
                spacing.push_str(&format!(r#" w:line="{v}" w:lineRule="auto""#));
            }
            crate::look::LineSpacing::AtLeast(v) => {
                spacing.push_str(&format!(r#" w:line="{v}" w:lineRule="atLeast""#));
            }
            crate::look::LineSpacing::Exact(v) => {
                spacing.push_str(&format!(r#" w:line="{v}" w:lineRule="exact""#));
            }
            _ => {}
        }
        if !spacing.is_empty() {
            push(
                "spacing",
                format!(r#"<w:spacing xmlns:w="{ns}"{spacing}/>"#),
            )?;
        }
        let mut ind = String::new();
        if look.left != 0 {
            ind.push_str(&format!(r#" w:left="{}""#, look.left));
        }
        if look.right != 0 {
            ind.push_str(&format!(r#" w:right="{}""#, look.right));
        }
        if look.first_line > 0 {
            ind.push_str(&format!(r#" w:firstLine="{}""#, look.first_line));
        } else if look.first_line < 0 {
            ind.push_str(&format!(r#" w:hanging="{}""#, -look.first_line));
        }
        if !ind.is_empty() {
            push("ind", format!(r#"<w:ind xmlns:w="{ns}"{ind}/>"#))?;
        }
        for (name, value) in [
            ("keepNext", look.keep_next),
            ("keepLines", look.keep_lines),
            ("pageBreakBefore", look.page_break_before),
            ("widowControl", look.widow_control),
            ("contextualSpacing", look.contextual),
            ("snapToGrid", look.snap_to_grid),
        ] {
            push(
                name,
                format!(r#"<w:{name} xmlns:w="{ns}" w:val="{}"/>"#, u8::from(value)),
            )?;
        }
        if let Some(fill) = look.shading {
            push(
                "shd",
                format!(
                    r#"<w:shd xmlns:w="{ns}" w:val="clear" w:fill="{:06X}"/>"#,
                    fill.0
                ),
            )?;
        }
    }
    if let Some(props) = props {
        let raw = &source[props.range.clone()];
        if raw.ends_with(b"/>") {
            let children = additions
                .into_iter()
                .flat_map(|(_, xml)| xml)
                .collect::<Vec<_>>();
            operations.push((
                props.range.start,
                props.range.end,
                append_child(source, props, &children)?,
            ));
        } else {
            let close = raw
                .windows(2)
                .rposition(|v| v == b"</")
                .ok_or_else(invalid)?
                + props.range.start;
            for (name, xml) in additions {
                let offset = props
                    .children
                    .iter()
                    .find(|n| {
                        n.namespace.as_deref() == Some(ns) && rank(&n.local_name) > rank(name)
                    })
                    .map_or(close, |n| n.range.start);
                operations.push((offset, offset, xml));
            }
        }
    } else {
        let mut ppr = format!(r#"<w:pPr xmlns:w="{ns}">"#).into_bytes();
        for (_, xml) in additions {
            ppr.extend(xml);
        }
        ppr.extend_from_slice(b"</w:pPr>");
        let raw = &source[para.range.clone()];
        if raw.ends_with(b"/>") {
            return append_child(source, para, &ppr);
        }
        let at = para.range.start + tag_end(raw).ok_or_else(invalid)? + 1;
        operations.push((at, at, ppr));
    }
    operations.sort_by_key(|(start, end, _)| (*start, *end));
    let mut out = Vec::new();
    let mut pos = para.range.start;
    for (start, end, bytes) in operations {
        out.extend_from_slice(source.get(pos..start).ok_or_else(invalid)?);
        out.extend(bytes);
        pos = end;
    }
    out.extend_from_slice(&source[pos..para.range.end]);
    Ok(out)
}

fn reject_unprojected_markup(raw: &[u8]) -> Result<(), EditError> {
    if raw.windows(2).any(|w| w == b"<?") || raw.windows(4).any(|w| w == b"<!--") {
        return Err(EditError::Locked);
    }
    Ok(())
}

fn replace_children(source: &[u8], node: &XmlNode, contents: &[u8]) -> Result<Vec<u8>, EditError> {
    let raw = source.get(node.range.clone()).ok_or(invalid())?;
    let open_end = tag_end(raw).ok_or(invalid())?;
    if raw.get(open_end.saturating_sub(1)) == Some(&b'/') {
        return append_child(source, node, contents);
    }
    let end = raw.windows(2).rposition(|w| w == b"</").ok_or(invalid())?;
    let mut result = Vec::new();
    result.extend_from_slice(&raw[..=open_end]);
    result.extend_from_slice(contents);
    result.extend_from_slice(&raw[end..]);
    Ok(result)
}

fn tag_end(raw: &[u8]) -> Option<usize> {
    let mut quoted = None;
    for (i, &b) in raw.iter().enumerate() {
        if let Some(q) = quoted {
            if b == q {
                quoted = None;
            }
        } else if b == b'\'' || b == b'"' {
            quoted = Some(b);
        } else if b == b'>' {
            return Some(i);
        }
    }
    None
}

fn append_child(source: &[u8], node: &XmlNode, child: &[u8]) -> Result<Vec<u8>, EditError> {
    let raw = source.get(node.range.clone()).ok_or(invalid())?;
    let open_end = tag_end(raw).ok_or(invalid())?;
    let mut result = Vec::new();
    if raw.get(open_end.saturating_sub(1)) == Some(&b'/') {
        let end = raw[1..]
            .iter()
            .position(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
            .ok_or(invalid())?
            + 1;
        result.extend_from_slice(&raw[..open_end - 1]);
        result.push(b'>');
        result.extend_from_slice(child);
        result.extend_from_slice(b"</");
        result.extend_from_slice(&raw[1..end]);
        result.push(b'>');
    } else {
        let end = raw.windows(2).rposition(|b| b == b"</").ok_or(invalid())?;
        result.extend_from_slice(&raw[..end]);
        result.extend_from_slice(child);
        result.extend_from_slice(&raw[end..]);
    }
    Ok(result)
}

fn write_text(source: &[u8], node: &XmlNode, text: &str) -> Result<Vec<u8>, EditError> {
    // Text-only v1: tabs/breaks require explicit Word elements. XML
    // normalization would also erase the exact distinction for raw CR.
    if text.contains(['\r', '\n', '\t']) {
        return Err(invalid());
    }
    let raw = source.get(node.range.clone()).ok_or(invalid())?;
    let open_end = tag_end(raw).ok_or(invalid())?;
    let empty = raw.get(open_end.saturating_sub(1)) == Some(&b'/');
    let mut open = raw[..open_end - usize::from(empty)].to_vec();
    let edge_space = text.starts_with(' ') || text.ends_with(' ');
    if edge_space {
        match node.attr(Some("http://www.w3.org/XML/1998/namespace"), "space") {
            None => open.extend_from_slice(b" xml:space=\"preserve\""),
            Some("preserve") => {}
            // Do not overwrite an existing lexical attribute representation.
            Some(_) => return Err(invalid()),
        }
    }
    open.push(b'>');
    open.extend_from_slice(quick_xml::escape::escape(text).as_bytes());
    if empty {
        let end = raw[1..]
            .iter()
            .position(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
            .ok_or(invalid())?
            + 1;
        open.extend_from_slice(b"</");
        open.extend_from_slice(&raw[1..end]);
        open.push(b'>');
    } else {
        let end = raw.windows(2).rposition(|b| b == b"</").ok_or(invalid())?;
        // Comments or PIs inside a text element would otherwise be lost.
        if raw[open_end + 1..end].contains(&b'<') {
            return Err(EditError::Locked);
        }
        open.extend_from_slice(&raw[end..]);
    }
    Ok(open)
}

#[path = "word_numbering.rs"]
mod numbering_write;
use crate::{EditError, Place, WordOp};
use std::{collections::BTreeMap, sync::Arc};

#[derive(Debug, Clone)]
pub(crate) struct WordBinding {
    document_name: String,
    state: RawWord,
    saved: Arc<XmlPart>,
    styles: Option<Arc<XmlPart>>,
    numbering_name: String,
    auxiliary: Auxiliary,
    saved_auxiliary: Auxiliary,
    images: Arc<HashMap<String, std::path::PathBuf>>,
    _image_dir: Option<Arc<tempfile::TempDir>>,
    /// Most recent inverse plus exact pre-edit lexical state for stack undo.
    history: Vec<(WordOp, RawUndo)>,
    history_bytes: usize,
}

#[derive(Debug, Clone)]
struct RawWord {
    document: Arc<XmlPart>,
    model: WordDoc,
    ids: BTreeMap<usize, NodeId>,
    tombstones: BTreeMap<NodeId, Arc<[u8]>>,
}

#[derive(Debug, Clone, Default)]
struct Auxiliary {
    numbering: Option<Arc<XmlPart>>,
    manifest: Option<Arc<XmlPart>>,
    relationships: Option<Arc<XmlPart>>,
}

#[derive(Debug, Clone)]
struct RawUndo {
    start: usize,
    new_len: usize,
    previous: Arc<[u8]>,
    previous_ids: Vec<(usize, NodeId)>,
    previous_tombstone: Option<(NodeId, Option<Arc<[u8]>>)>,
    auxiliary: Option<Auxiliary>,
    cost: usize,
}

const HISTORY_BYTES: usize = 16 * 1024 * 1024;
const HISTORY_STEPS: usize = 256;

fn invalid() -> EditError {
    EditError::Invalid("unsupported Word XML edit".into())
}

impl WordBinding {
    pub(crate) fn load(package: &Package) -> Result<(Self, WordDoc), OpenError> {
        let (document_name, styles, numbering) = package_parts(package)?;
        let optional = |name: Option<&str>| {
            name.filter(|n| package.has_part(n))
                .map(|n| package.part(n).map(Arc::new))
                .transpose()
        };
        let document = Arc::new(package.part(&document_name)?);
        let styles = optional(styles.as_deref())?;
        let numbering_name = numbering.clone().unwrap_or_else(|| {
            document_name.rsplit_once('/').map_or_else(
                || "numbering.xml".into(),
                |(dir, _)| format!("{dir}/numbering.xml"),
            )
        });
        let numbering = optional(numbering.as_deref())?;
        let manifest = optional(Some("[Content_Types].xml"))?;
        let relations_name = relationship_part_name(&document_name);
        let relationships = optional(Some(&relations_name))?;
        let theme_name = package.related_part(&document_name, "theme")?;
        let theme = optional(theme_name.as_deref())?;
        let conventional_footnotes = document_name.rsplit_once('/').map_or_else(
            || "footnotes.xml".into(),
            |(dir, _)| format!("{dir}/footnotes.xml"),
        );
        let footnotes_name = package
            .related_part(&document_name, "footnotes")?
            .or_else(|| {
                package
                    .has_part(&conventional_footnotes)
                    .then_some(conventional_footnotes)
            });
        let footnotes = optional(footnotes_name.as_deref())?;
        let (image_dir, images) = extract_images(package, &document);
        let images = Arc::new(images);
        let mut model = project(
            &document,
            styles.as_deref(),
            numbering.as_deref(),
            theme.as_deref(),
            relationships.as_deref(),
            &images,
            footnotes.as_deref(),
        )?;
        attach_headers(
            package,
            &document_name,
            &document,
            styles.as_deref(),
            numbering.as_deref(),
            theme.as_deref(),
            relationships.as_deref(),
            &mut model,
        );
        let mut ids = BTreeMap::new();
        visit_blocks(&model.blocks, &mut |b| {
            ids.insert(b.id() as usize, b.id());
        });
        let auxiliary = Auxiliary {
            numbering,
            manifest,
            relationships,
        };
        let binding = Self {
            document_name,
            numbering_name,
            saved: Arc::clone(&document),
            state: RawWord {
                document,
                model: model.clone(),
                ids,
                tombstones: BTreeMap::new(),
            },
            styles,
            saved_auxiliary: auxiliary.clone(),
            auxiliary,
            images,
            _image_dir: image_dir.map(Arc::new),
            history: Vec::new(),
            history_bytes: 0,
        };
        Ok((binding, model))
    }

    pub(crate) fn changed_parts(&self) -> Vec<(String, Vec<u8>)> {
        let mut changes = Vec::new();
        if self.saved.bytes() != self.state.document.bytes() {
            changes.push((
                self.document_name.clone(),
                self.state.document.bytes().to_vec(),
            ));
        }
        for (name, current, saved) in [
            (
                relationship_part_name(&self.document_name),
                &self.auxiliary.relationships,
                &self.saved_auxiliary.relationships,
            ),
            (
                self.numbering_name.clone(),
                &self.auxiliary.numbering,
                &self.saved_auxiliary.numbering,
            ),
            (
                "[Content_Types].xml".into(),
                &self.auxiliary.manifest,
                &self.saved_auxiliary.manifest,
            ),
        ] {
            if let Some(current) = current
                && !saved.as_ref().is_some_and(|p| p.bytes() == current.bytes())
            {
                changes.push((name, current.bytes().to_vec()));
            }
        }
        changes
    }

    pub(crate) fn mark_saved(&mut self) {
        self.saved = Arc::clone(&self.state.document);
        self.saved_auxiliary = self.auxiliary.clone();
        // Already published new optional ZIP entries cannot be removed by the
        // replacement-only Package API. Unsaved inverse remains byte-exact.
        // After saving, keep immutable auxiliary definitions on lexical undo.
        for (_, undo) in &mut self.history {
            undo.auxiliary = None;
        }
    }

    pub(crate) fn apply(&mut self, op: &WordOp) -> Result<(), EditError> {
        let mut next_model = self.state.model.clone();
        let inverse = next_model.apply(op.clone())?;
        if let Some((_, undo)) = self.history.last().filter(|(last, _)| last == op) {
            let undo = undo.clone();
            let end = undo.start.checked_add(undo.new_len).ok_or_else(invalid)?;
            let range = undo.start..end;
            let document =
                replace_part(self.state.document.bytes(), range.clone(), &undo.previous)?;
            let mut ids = shift_ids(&self.state.ids, range, undo.previous.len());
            ids.extend(undo.previous_ids);
            if let Some((id, old)) = undo.previous_tombstone {
                if let Some(raw) = old {
                    self.state.tombstones.insert(id, raw);
                } else {
                    self.state.tombstones.remove(&id);
                }
            }
            self.state.document = document;
            self.state.ids = ids;
            self.state.model = next_model;
            if let Some(old) = undo.auxiliary {
                self.auxiliary = old;
            }
            self.history.pop();
            self.history_bytes -= undo.cost;
            return Ok(());
        }
        let original = &self.state.document;
        let source = original.bytes();
        let root = original.root();
        let ns = root.namespace.as_deref().ok_or_else(invalid)?;
        let body = child(root, ns, "body").ok_or_else(invalid)?;
        let resolve = |id| {
            self.state
                .ids
                .iter()
                .find_map(|(&at, &candidate)| (candidate == id).then_some(at))
                .ok_or(EditError::Missing)
        };
        let lookup = |id| find_node(body, ns, resolve(id)?).ok_or(EditError::Missing);
        let mut tombstones = self.state.tombstones.clone();
        let mut previous_tombstone = None;
        let mut auxiliary = self.auxiliary.clone();
        let mut assigned = None;
        let (range, rewritten) = match op {
            WordOp::SetContent { para, inlines } => {
                let (node, _) = lookup(*para)?;
                let before =
                    find_para(&self.state.model.blocks, *para).ok_or(EditError::Missing)?;
                let after = write_content(
                    source,
                    node,
                    ns,
                    &before.inlines,
                    inlines,
                    &mut auxiliary.relationships,
                )?;
                assigned = Some((node.range.start, *para));
                (node.range.clone(), after)
            }
            WordOp::SetStyle { para, style } => {
                let (p, _) = lookup(*para)?;
                let after = self.write_style(source, p, ns, style, &mut auxiliary)?;
                assigned = Some((p.range.start, *para));
                (p.range.clone(), after)
            }
            WordOp::SetAlign { para, align } => {
                let (p, _) = lookup(*para)?;
                let value = match align {
                    Align::Left => "left",
                    Align::Center => "center",
                    Align::Right => "right",
                    Align::Justify => "both",
                };
                let after = set_property(source, p, ns, "jc", value)?;
                assigned = Some((p.range.start, *para));
                (p.range.clone(), after)
            }
            // Written back in backlog item 14 (docs/layout-engine-implementation.md).
            WordOp::SetLook { para, look } => {
                let (p, _) = lookup(*para)?;
                let after = edit_para_properties(source, p, ns, None, None, None, Some(look))?;
                assigned = Some((p.range.start, *para));
                (p.range.clone(), after)
            }
            WordOp::DeletePara { para } => {
                let (p, container) = lookup(*para)?;
                if child(p, ns, "pPr")
                    .and_then(|n| child(n, ns, "sectPr"))
                    .is_some()
                {
                    return Err(EditError::Locked);
                }
                if container.is(ns, "tc")
                    && container.children.iter().filter(|n| n.is(ns, "p")).count() <= 1
                {
                    return Err(EditError::Locked);
                }
                // Managed deletion is intentionally destructive to its target;
                // exact XML is held in a tombstone for undo.
                if source[p.range.clone()].len() > HISTORY_BYTES {
                    return Err(EditError::Locked);
                }
                let mut total = source[p.range.clone()].len();
                for v in tombstones.values() {
                    total = total.saturating_add(v.len());
                }
                if total > HISTORY_BYTES || tombstones.len() >= HISTORY_STEPS {
                    return Err(EditError::Locked);
                }
                previous_tombstone = Some((
                    *para,
                    tombstones.insert(*para, Arc::from(&source[p.range.clone()])),
                ));
                (p.range.clone(), Vec::new())
            }
            WordOp::InsertPara {
                at,
                id,
                style,
                inlines,
            } => {
                let WordOp::DeletePara { para: id_allocated } = inverse else {
                    return Err(invalid());
                };
                if self.state.ids.values().any(|id| *id == id_allocated) {
                    return Err(invalid());
                }
                let (point, previous) = match at {
                    Place::After(node) => {
                        let (node, _) = lookup(*node)?;
                        if node.is(ns, "sectPr") {
                            return Err(EditError::Locked);
                        }
                        (node.range.end, node.is(ns, "p").then_some(node))
                    }
                    Place::BodyStart => (container_start(source, body, ns, "body")?, None),
                    Place::CellStart { table, row, col } => {
                        let (table, _) = lookup(*table)?;
                        if !table.is(ns, "tbl") {
                            return Err(EditError::Missing);
                        }
                        let cell = table
                            .children
                            .iter()
                            .filter(|n| n.is(ns, "tr"))
                            .nth(*row)
                            .and_then(|row| {
                                row.children.iter().filter(|n| n.is(ns, "tc")).nth(*col)
                            })
                            .ok_or(EditError::Missing)?;
                        let point = child(cell, ns, "tcPr").map_or_else(
                            || container_start(source, cell, ns, "tc"),
                            |p| Ok(p.range.end),
                        )?;
                        (point, None)
                    }
                };
                let found = id.and_then(|id| tombstones.remove(&id));
                let xml = if let Some(old) = found {
                    previous_tombstone = Some((id.ok_or_else(invalid)?, Some(Arc::clone(&old))));
                    old.to_vec()
                } else {
                    self.new_paragraph(source, ns, previous, style, inlines, &mut auxiliary)?
                };
                assigned = Some((point, id_allocated));
                (point..point, xml)
            }
        };
        if source[range.clone()] == rewritten {
            self.state.model = next_model;
            return Ok(());
        }
        let document = replace_part(source, range.clone(), &rewritten)?;
        let read = project(
            &document,
            self.styles.as_deref(),
            auxiliary.numbering.as_deref(),
            None,
            auxiliary.relationships.as_deref(),
            &self.images,
            None,
        )
        .map_err(|_| invalid())?;
        let mut ids = shift_ids(&self.state.ids, range.clone(), rewritten.len());
        if let Some((at, id)) = assigned {
            ids.insert(at, id);
        }
        let mut expected = Vec::new();
        let mut actual = Vec::new();
        visit_blocks(&next_model.blocks, &mut |b| expected.push(b.id()));
        let mut missing = false;
        visit_blocks(&read.blocks, &mut |b| match ids.get(&(b.id() as usize)) {
            Some(id) => actual.push(*id),
            None => missing = true,
        });
        if missing || actual != expected {
            return Err(invalid());
        }
        let previous = source[range.clone()].len();
        let auxiliaries_changed = [
            (&self.auxiliary.numbering, &auxiliary.numbering),
            (&self.auxiliary.relationships, &auxiliary.relationships),
            (&self.auxiliary.manifest, &auxiliary.manifest),
        ]
        .into_iter()
        .filter(|(old, new)| match (old, new) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        })
        .collect::<Vec<_>>();
        let cost = auxiliaries_changed
            .iter()
            .fold(previous, |count, (old, _)| {
                count.saturating_add(old.as_ref().map_or(0, |p| p.bytes().len()))
            });
        let undo = (cost <= HISTORY_BYTES).then(|| RawUndo {
            start: range.start,
            new_len: rewritten.len(),
            previous: Arc::from(&source[range.clone()]),
            previous_ids: self
                .state
                .ids
                .range(range)
                .map(|(&at, &id)| (at, id))
                .collect(),
            previous_tombstone,
            auxiliary: (!auxiliaries_changed.is_empty()).then(|| self.auxiliary.clone()),
            cost,
        });
        self.state = RawWord {
            document,
            model: next_model,
            ids,
            tombstones,
        };
        self.auxiliary = auxiliary;
        if let Some(undo) = undo {
            while self.history.len() >= HISTORY_STEPS || self.history_bytes + cost > HISTORY_BYTES {
                self.history_bytes -= self.history.remove(0).1.cost;
            }
            self.history_bytes += cost;
            self.history.push((inverse, undo));
        } else {
            self.history.clear();
            self.history_bytes = 0;
        }
        Ok(())
    }

    fn write_style(
        &self,
        source: &[u8],
        p: &XmlNode,
        ns: &str,
        style: &ParaStyle,
        auxiliary: &mut Auxiliary,
    ) -> Result<Vec<u8>, EditError> {
        if let ParaStyle::ListItem { level, ordered } = *style {
            let manifest = auxiliary.manifest.as_deref().ok_or_else(invalid)?;
            let (id, numbering, new_manifest) = numbering_write::with_list(
                ns,
                auxiliary.numbering.as_deref(),
                manifest,
                &self.numbering_name,
                level,
                ordered,
            )?;
            if let Some(num) = numbering {
                if auxiliary.numbering.is_none() {
                    add_relationship(
                        &mut auxiliary.relationships,
                        ns,
                        "numbering.xml",
                        "numbering",
                        false,
                    )?;
                }
                auxiliary.numbering = Some(Arc::new(num));
            }
            if let Some(manifest) = new_manifest {
                auxiliary.manifest = Some(Arc::new(manifest));
            }
            // Direct outline levels take precedence over list display. Body
            // outline is sufficient, without rebuilding unrelated properties.
            return edit_para_properties(source, p, ns, None, Some((&id, level)), Some("9"), None);
        }
        let id = match style {
            ParaStyle::Normal => "Normal".into(),
            ParaStyle::Title => "Title".into(),
            ParaStyle::Heading(level @ 1..=6) => {
                let context = Context::new(ns, self.styles.as_ref().map(|p| p.root()), None, None);
                context
                    .styles
                    .keys()
                    .filter(|id| {
                        context
                            .style_chain(id)
                            .into_iter()
                            .filter_map(|style| val(child(style, ns, "pPr")?, ns, "outlineLvl"))
                            .next()
                            .and_then(|v| v.parse::<u8>().ok())
                            == Some(level - 1)
                    })
                    .min()
                    .map(|id| (*id).to_owned())
                    .unwrap_or_else(|| format!("Heading{level}"))
            }
            ParaStyle::Quote => "Quote".into(),
            ParaStyle::Other(id)
                if self.styles.as_ref().is_some_and(|p| {
                    p.root()
                        .children
                        .iter()
                        .any(|n| n.is(ns, "style") && n.attr(Some(ns), "styleId") == Some(id))
                }) =>
            {
                id.clone()
            }
            _ => return Err(invalid()),
        };
        let context = Context::new(
            ns,
            self.styles.as_ref().map(|p| p.root()),
            auxiliary.numbering.as_ref().map(|p| p.root()),
            None,
        );
        let old = context.para(p);
        let chain = context.style_chain(&id);
        let inherited = chain
            .iter()
            .find_map(|s| child(child(s, ns, "pPr")?, ns, "numPr"))
            .is_some();
        let has_direct_number = child(p, ns, "pPr")
            .and_then(|pr| child(pr, ns, "numPr"))
            .is_some();
        let has_direct_outline = child(p, ns, "pPr")
            .and_then(|pr| child(pr, ns, "outlineLvl"))
            .is_some();
        let suppression =
            (inherited || has_direct_number || matches!(old.style, ParaStyle::ListItem { .. }))
                .then_some(("0", 0));
        let outline = if has_direct_outline {
            Some(match style {
                ParaStyle::Heading(1) => "0",
                ParaStyle::Heading(2) => "1",
                ParaStyle::Heading(3) => "2",
                ParaStyle::Heading(4) => "3",
                ParaStyle::Heading(5) => "4",
                ParaStyle::Heading(6) => "5",
                _ => "9",
            })
        } else {
            None
        };
        edit_para_properties(source, p, ns, Some(&id), suppression, outline, None)
    }

    fn new_paragraph(
        &self,
        source: &[u8],
        ns: &str,
        previous: Option<&XmlNode>,
        style: &ParaStyle,
        inlines: &[Inline],
        auxiliary: &mut Auxiliary,
    ) -> Result<Vec<u8>, EditError> {
        let mut p = format!(r#"<w:p xmlns:w="{ns}">"#).into_bytes();
        if let Some(prev) = previous
            && let Some(props) = child(prev, ns, "pPr")
        {
            if child(props, ns, "sectPr").is_some() || child(props, ns, "pPrChange").is_some() {
                return Err(EditError::Locked);
            }
            // The sibling shares ancestor namespaces. Do not change a locally
            // declared prefix when moving pPr into its new paragraph.
            let open = &source[prev.range.start
                ..prev.range.start + tag_end(&source[prev.range.clone()]).ok_or_else(invalid)?];
            if open.windows(5).any(|x| x == b"xmlns") {
                return Err(EditError::Locked);
            }
            p.extend_from_slice(&source[props.range.clone()]);
        }
        p.extend(render_tokens(
            ns,
            &tokens(inlines)?,
            &mut auxiliary.relationships,
        )?);
        p.extend_from_slice(b"</w:p>");
        // Parse with the document's visible root namespace bindings if needed;
        // fail closed when a copied property cannot stand alone.
        let parsed = XmlPart::parse(p.clone()).map_err(|_| EditError::Locked)?;
        self.write_style(&p, parsed.root(), ns, style, auxiliary)
    }
}

fn replace_part(
    source: &[u8],
    range: std::ops::Range<usize>,
    replacement: &[u8],
) -> Result<Arc<XmlPart>, EditError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(source.get(..range.start).ok_or_else(invalid)?);
    bytes.extend_from_slice(replacement);
    bytes.extend_from_slice(source.get(range.end..).ok_or_else(invalid)?);
    XmlPart::parse(bytes).map(Arc::new).map_err(|_| invalid())
}

fn shift_ids(
    ids: &BTreeMap<usize, NodeId>,
    range: std::ops::Range<usize>,
    new_len: usize,
) -> BTreeMap<usize, NodeId> {
    ids.iter()
        .filter_map(|(&at, &id)| {
            if at < range.start {
                Some((at, id))
            } else if at >= range.end {
                Some((at - range.len() + new_len, id))
            } else {
                None
            }
        })
        .collect()
}

fn visit_blocks(blocks: &[Block], f: &mut impl FnMut(&Block)) {
    for block in blocks {
        f(block);
        if let Block::Table(t) = block {
            for cell in t.rows.iter().flatten() {
                visit_blocks(cell, f);
            }
        }
    }
}

fn find_para(blocks: &[Block], id: NodeId) -> Option<&Para> {
    blocks.iter().find_map(|b| match b {
        Block::Paragraph(p) if p.id == id => Some(p),
        Block::Table(t) => t.rows.iter().flatten().find_map(|cell| find_para(cell, id)),
        _ => None,
    })
}

fn find_node<'a>(parent: &'a XmlNode, ns: &str, at: usize) -> Option<(&'a XmlNode, &'a XmlNode)> {
    let n = parent.children.iter().find(|n| n.range.contains(&at))?;
    if n.range.start == at {
        return Some((n, parent));
    }
    if ["body", "tbl", "tr", "tc"]
        .iter()
        .any(|name| n.is(ns, name))
    {
        find_node(n, ns, at)
    } else {
        None
    }
}

fn container_start(
    source: &[u8],
    container: &XmlNode,
    ns: &str,
    name: &str,
) -> Result<usize, EditError> {
    if !container.is(ns, name) {
        return Err(invalid());
    }
    let raw = &source[container.range.clone()];
    let end = tag_end(raw).ok_or_else(invalid)?;
    if end > 0 && raw[end - 1] == b'/' {
        return Err(EditError::Locked);
    }
    Ok(container.range.start + end + 1)
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Token<'a> {
    Text(char, &'a Marks),
    Break,
    Tab,
    Atom(&'a Atom),
}
fn tokens(inlines: &[Inline]) -> Result<Vec<Token<'_>>, EditError> {
    let mut result = Vec::new();
    for inline in inlines {
        match inline {
            Inline::Text { text, marks } => {
                if text.contains(['\r', '\n', '\t', '\u{fffc}'])
                    || text.len() > 512 * 1024
                    || marks.link.as_ref().is_some_and(|l| l.len() > 4096)
                {
                    return Err(invalid());
                }
                result.extend(text.chars().map(|c| Token::Text(c, marks)));
            }
            Inline::Break => result.push(Token::Break),
            Inline::Tab => result.push(Token::Tab),
            Inline::Atom(atom) => result.push(Token::Atom(atom)),
        }
        if result.len() > 131_072 {
            return Err(invalid());
        }
    }
    Ok(result)
}

fn render_tokens(
    ns: &str,
    tokens: &[Token<'_>],
    relationships: &mut Option<Arc<XmlPart>>,
) -> Result<Vec<u8>, EditError> {
    let mut output = Vec::new();
    let mut tokens = tokens;
    while let Some(first) = tokens.first() {
        match first {
            Token::Text(_, marks) => {
                let len = tokens
                    .iter()
                    .take_while(|token| matches!(token,Token::Text(_,m) if m==marks))
                    .count();
                let text: String = tokens[..len]
                    .iter()
                    .filter_map(|t| {
                        if let Token::Text(c, _) = t {
                            Some(*c)
                        } else {
                            None
                        }
                    })
                    .collect();
                let preserve = if text.starts_with(' ') || text.ends_with(' ') {
                    r#" xml:space="preserve""#
                } else {
                    ""
                };
                let underline = if marks.underline { "single" } else { "none" };
                let text = quick_xml::escape::escape(&text);
                let mut props = format!(
                    r#"<w:b w:val="{}"/><w:i w:val="{}"/><w:strike w:val="{}"/><w:u w:val="{underline}"/>"#,
                    u8::from(marks.bold),
                    u8::from(marks.italic),
                    u8::from(marks.strike)
                );
                if !marks.fonts.latin.is_empty() || !marks.fonts.east_asia.is_empty() {
                    let latin = quick_xml::escape::escape(&marks.fonts.latin);
                    let east = quick_xml::escape::escape(&marks.fonts.east_asia);
                    props.push_str(&format!(
                        r#"<w:rFonts w:ascii="{latin}" w:hAnsi="{latin}" w:eastAsia="{east}"/>"#
                    ));
                }
                if let Some(size) = marks.size {
                    props.push_str(&format!(
                        r#"<w:sz w:val="{}"/><w:szCs w:val="{}"/>"#,
                        size / 10,
                        size / 10
                    ));
                }
                if let Some(color) = marks.color {
                    props.push_str(&format!(r#"<w:color w:val="{:06X}"/>"#, color.0));
                }
                if let Some(highlight) = marks.highlight
                    && let Some(name) = highlight_name(highlight)
                {
                    props.push_str(&format!(r#"<w:highlight w:val="{name}"/>"#));
                }
                if matches!(marks.baseline, crate::look::Baseline::Superscript) {
                    props.push_str(r#"<w:vertAlign w:val="superscript"/>"#);
                } else if matches!(marks.baseline, crate::look::Baseline::Subscript) {
                    props.push_str(r#"<w:vertAlign w:val="subscript"/>"#);
                }
                let run = format!(
                    r#"<w:r xmlns:w="{ns}"><w:rPr>{props}</w:rPr><w:t{preserve}>{text}</w:t></w:r>"#
                )
                .into_bytes();
                output.extend(wrap_link(ns, run, marks.link.as_deref(), relationships)?);
                tokens = &tokens[len..];
            }
            Token::Break | Token::Tab => {
                let name = if *first == Token::Break { "br" } else { "tab" };
                output.extend(format!(r#"<w:r xmlns:w="{ns}"><w:{name}/></w:r>"#).bytes());
                tokens = &tokens[1..];
            }
            Token::Atom(_) => return Err(EditError::Locked),
        }
    }
    Ok(output)
}

fn write_content(
    source: &[u8],
    p: &XmlNode,
    ns: &str,
    old: &[Inline],
    new: &[Inline],
    relationships: &mut Option<Arc<XmlPart>>,
) -> Result<Vec<u8>, EditError> {
    let old = tokens(old)?;
    let new = tokens(new)?;
    if old == new {
        return Ok(source[p.range.clone()].to_vec());
    }
    let link_only = old.len() == new.len()
        && old.iter().zip(&new).all(|(old, new)| match (old, new) {
            (Token::Text(a, m), Token::Text(b, n)) => a == b && same_non_link_marks(m, n),
            _ => old == new,
        });
    if link_only {
        return write_link_only(source, p, ns, &old, &new, relationships);
    }
    let prefix = old
        .iter()
        .zip(&new)
        .take_while(|(a, b)| token_matches(a, b))
        .count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| token_matches(a, b))
        .count();
    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    let replacement = &new[prefix..new_end];
    let mut content = Vec::new();
    let mut retained_end = None;
    let mut inserted = false;
    let mut offset = 0;
    let xml = &source[p.range.clone()];
    reject_unprojected_markup(xml)?;
    // Ignore layout whitespace; meaningful direct text under w:p cannot be safely rebuilt.
    if !p.text.trim().is_empty() {
        return Err(EditError::Locked);
    }
    let cx = Context::new(ns, None, None, None);
    for node in &p.children {
        if node.is(ns, "pPr") {
            content.extend_from_slice(&source[node.range.clone()]);
            continue;
        }
        let mut inlines = Vec::new();
        let mut locked = false;
        cx.inline(node, &[], None, &mut inlines, &mut locked);
        let len = tokens(&inlines)?.len();
        let end = offset + len;
        if len == 0 {
            // Retain unknown zero-width content only if the selection does not
            // run through it, and preserve its relative placement.
            if offset > prefix && offset < old_end {
                return Err(EditError::Locked);
            }
            if offset > old_end && !inserted {
                content.extend(render_tokens(ns, replacement, relationships)?);
                inserted = true;
            }
            content.extend_from_slice(&source[node.range.clone()]);
        } else if end <= prefix {
            content.extend_from_slice(&source[node.range.clone()]);
        } else if offset >= old_end {
            if !inserted {
                content.extend(render_tokens(ns, replacement, relationships)?);
                inserted = true;
            }
            content.extend_from_slice(&source[node.range.clone()]);
        } else {
            if locked
                || old[offset..end]
                    .iter()
                    .any(|token| matches!(token, Token::Atom(_)))
            {
                return Err(EditError::Locked);
            }
            let (a, b) = (prefix.saturating_sub(offset), old_end.min(end) - offset);
            let run = linked_run(node, ns)?;
            let single = plain_text_run(run, ns)?;
            let mut chars = single.text.chars();
            let left = chars.by_ref().take(a).collect::<String>();
            let _ = chars.by_ref().take(b - a).count();
            let right = chars.collect::<String>();
            if !inserted
                && old_end <= end
                && old[offset..end]
                    .first()
                    .and_then(|t| {
                        if let Token::Text(_, mark) = t {
                            Some(*mark)
                        } else {
                            None
                        }
                    })
                    .is_some_and(|mark| {
                        replacement
                            .iter()
                            .all(|token| {
                                matches!(token, Token::Text(_, m) if same_non_link_marks(m, mark) || markless(m))
                            })
                    })
            {
                let middle: String = replacement
                    .iter()
                    .filter_map(|t| {
                        if let Token::Text(c, _) = t {
                            Some(*c)
                        } else {
                            None
                        }
                    })
                    .collect();
                if a != 0 || !middle.is_empty() || !right.is_empty() {
                    content.extend(keep_fragment(
                        source,
                        node,
                        run,
                        single,
                        &(left + &middle + &right),
                    )?);
                }
            } else {
                if !left.is_empty() {
                    content.extend(keep_fragment(source, node, run, single, &left)?);
                }
                if !inserted {
                    if old_end <= end
                        && let Some(Token::Text(_, origin)) = old.get(offset)
                    {
                        content.extend(render_with_donor(
                            ns,
                            source,
                            run,
                            single,
                            replacement,
                            origin,
                            relationships,
                        )?);
                    } else {
                        content.extend(render_tokens(ns, replacement, relationships)?);
                    }
                }
                if !right.is_empty() {
                    retained_end = Some(keep_fragment(source, node, run, single, &right)?);
                }
            }
            inserted = true;
            if let Some(right) = retained_end.take() {
                content.extend(right);
            }
        }
        offset = end;
    }
    if offset != old.len() {
        return Err(EditError::Locked);
    }
    if !inserted {
        content.extend(render_tokens(ns, replacement, relationships)?);
    }
    replace_children(source, p, &content)
}

fn write_link_only(
    source: &[u8],
    p: &XmlNode,
    ns: &str,
    old: &[Token<'_>],
    new: &[Token<'_>],
    rels: &mut Option<Arc<XmlPart>>,
) -> Result<Vec<u8>, EditError> {
    let cx = Context::new(ns, None, None, None);
    let xml = &source[p.range.clone()];
    reject_unprojected_markup(xml)?;
    if !p.text.trim().is_empty() {
        return Err(EditError::Locked);
    }
    let mut content = Vec::new();
    let mut offset = 0;
    for node in &p.children {
        let mut spans = Vec::new();
        let mut locked = false;
        cx.inline(node, &[], None, &mut spans, &mut locked);
        let len = tokens(&spans)?.len();
        let before = old.get(offset..offset + len).ok_or_else(invalid)?;
        let after = new.get(offset..offset + len).ok_or_else(invalid)?;
        if before == after {
            content.extend_from_slice(&source[node.range.clone()]);
        } else {
            if locked {
                return Err(EditError::Locked);
            }
            if node.is(ns, "hyperlink")
                && before
                    .iter()
                    .any(|t| matches!(t,Token::Text(_,m) if m.link.is_none()))
            {
                // The projection could not safely resolve this original link.
                return Err(EditError::Locked);
            }
            let run = linked_run(node, ns)?;
            let text = plain_text_run(run, ns)?;
            let Token::Text(_, first) = before.first().ok_or_else(invalid)? else {
                return Err(EditError::Locked);
            };
            content.extend(render_link_fragment(
                ns,
                source,
                (node, run, text),
                after,
                first,
                rels,
            )?);
        }
        offset += len;
    }
    if offset != old.len() {
        return Err(EditError::Locked);
    }
    replace_children(source, p, &content)
}

fn render_link_fragment(
    ns: &str,
    source: &[u8],
    source_nodes: (&XmlNode, &XmlNode, &XmlNode),
    mut piece: &[Token<'_>],
    origin: &Marks,
    rels: &mut Option<Arc<XmlPart>>,
) -> Result<Vec<u8>, EditError> {
    let (outer, run, text) = source_nodes;
    let mut out = Vec::new();
    while let Some(Token::Text(_, marks)) = piece.first() {
        let n = piece
            .iter()
            .take_while(|x| matches!(x,Token::Text(_,m) if m==marks))
            .count();
        let value = piece[..n]
            .iter()
            .filter_map(|x| {
                if let Token::Text(c, _) = x {
                    Some(*c)
                } else {
                    None
                }
            })
            .collect::<String>();
        if marks.link == origin.link {
            out.extend(keep_fragment(source, outer, run, text, &value)?);
        } else {
            out.extend(wrap_link(
                ns,
                slice_run(source, run, text, &value)?,
                marks.link.as_deref(),
                rels,
            )?);
        }
        piece = &piece[n..];
    }
    if !piece.is_empty() {
        return Err(EditError::Locked);
    }
    Ok(out)
}

fn linked_run<'a>(node: &'a XmlNode, ns: &str) -> Result<&'a XmlNode, EditError> {
    if !node.is(ns, "hyperlink") {
        return Ok(node);
    }
    if node.children.len() != 1 || !node.text.trim().is_empty() {
        return Err(EditError::Locked);
    }
    Ok(&node.children[0])
}

fn keep_fragment(
    source: &[u8],
    outer: &XmlNode,
    run: &XmlNode,
    text: &XmlNode,
    fragment: &str,
) -> Result<Vec<u8>, EditError> {
    let bytes = slice_run(source, run, text, fragment)?;
    if outer.range.start == run.range.start {
        return Ok(bytes);
    }
    let mut result = source[outer.range.start..run.range.start].to_vec();
    result.extend(bytes);
    result.extend_from_slice(&source[run.range.end..outer.range.end]);
    Ok(result)
}

fn same_non_link_marks(a: &Marks, b: &Marks) -> bool {
    (a.bold, a.italic, a.underline, a.strike) == (b.bold, b.italic, b.underline, b.strike)
}

fn highlight_name(color: crate::look::Rgb) -> Option<&'static str> {
    Some(match color.0 {
        0x000000 => "black",
        0x0000FF => "blue",
        0x00FFFF => "cyan",
        0x00FF00 => "green",
        0xFF00FF => "magenta",
        0xFF0000 => "red",
        0xFFFF00 => "yellow",
        0xFFFFFF => "white",
        0x000080 => "darkBlue",
        0x008080 => "darkCyan",
        0x008000 => "darkGreen",
        0x800080 => "darkMagenta",
        0x800000 => "darkRed",
        0x808000 => "darkYellow",
        0x808080 => "darkGray",
        0xC0C0C0 => "lightGray",
        _ => return None,
    })
}

/// A new run with no explicit formatting keeps the formatting of text that
/// did not change, as typing does in Word.
fn markless(marks: &Marks) -> bool {
    *marks == Marks::default()
}

fn token_matches(old: &Token<'_>, new: &Token<'_>) -> bool {
    match (old, new) {
        (Token::Text(a, m), Token::Text(b, n)) => {
            a == b && (same_non_link_marks(m, n) || markless(n))
        }
        _ => old == new,
    }
}

fn render_with_donor(
    ns: &str,
    source: &[u8],
    run: &XmlNode,
    text: &XmlNode,
    mut piece: &[Token<'_>],
    origin: &Marks,
    rels: &mut Option<Arc<XmlPart>>,
) -> Result<Vec<u8>, EditError> {
    if !piece
        .iter()
        .all(|v| matches!(v,Token::Text(_,m) if same_non_link_marks(m,origin)))
    {
        return render_tokens(ns, piece, rels);
    }
    let mut out = Vec::new();
    while let Some(Token::Text(_, marks)) = piece.first() {
        let n = piece
            .iter()
            .take_while(|x| matches!(x,Token::Text(_,m) if m==marks))
            .count();
        let value = piece[..n]
            .iter()
            .filter_map(|x| {
                if let Token::Text(c, _) = x {
                    Some(*c)
                } else {
                    None
                }
            })
            .collect::<String>();
        out.extend(wrap_link(
            ns,
            slice_run(source, run, text, &value)?,
            marks.link.as_deref(),
            rels,
        )?);
        piece = &piece[n..];
    }
    Ok(out)
}

fn unused_prefix(bytes: &[u8], base: &str) -> Result<String, EditError> {
    for n in 0..16 {
        let prefix = format!("{base}{n}");
        let mark = format!("{prefix}:");
        if !bytes.windows(mark.len()).any(|s| s == mark.as_bytes()) {
            return Ok(prefix);
        }
    }
    Err(EditError::Locked)
}

fn wrap_link(
    ns: &str,
    mut run: Vec<u8>,
    target: Option<&str>,
    rels: &mut Option<Arc<XmlPart>>,
) -> Result<Vec<u8>, EditError> {
    let Some(target) = target else {
        return Ok(run);
    };
    let word = unused_prefix(&run, "atriumW")?;
    let relation = unused_prefix(&run, "atriumR")?;
    let open = if let Some(anchor) = target.strip_prefix('#') {
        if anchor.is_empty()
            || anchor.len() > 255
            || anchor.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return Err(invalid());
        }
        let anchor = quick_xml::escape::escape(anchor);
        format!(r#"<{word}:hyperlink xmlns:{word}="{ns}" {word}:anchor="{anchor}">"#)
    } else {
        if !allowed_link(target) {
            return Err(invalid());
        }
        let id = add_relationship(rels, ns, target, "hyperlink", true)?;
        let id = quick_xml::escape::escape(&id);
        let ty = if ns == WS { RS } else { R };
        format!(
            r#"<{word}:hyperlink xmlns:{word}="{ns}" xmlns:{relation}="{ty}" {relation}:id="{id}">"#
        )
    };
    let mut out = open.into_bytes();
    out.append(&mut run);
    out.extend(format!("</{word}:hyperlink>").bytes());
    Ok(out)
}

fn add_relationship(
    rels: &mut Option<Arc<XmlPart>>,
    ns: &str,
    target: &str,
    kind: &str,
    external: bool,
) -> Result<String, EditError> {
    let ty = format!("{}/{kind}", if ns == WS { RS } else { R });
    let namespace = rels
        .as_ref()
        .map(|old| rel_namespace(old.root()).ok_or_else(invalid))
        .transpose()?
        .unwrap_or(OPC_REL);
    let mut ids = HashSet::new();
    if let Some(old) = rels.as_ref() {
        for rel in old
            .root()
            .children
            .iter()
            .filter(|n| n.is(namespace, "Relationship"))
        {
            let id = rel
                .attr(None, "Id")
                .filter(|s| !s.is_empty())
                .ok_or_else(invalid)?;
            if !ids.insert(id) {
                return Err(invalid());
            }
            let ext = rel.attr(None, "TargetMode") == Some("External");
            if ext == external
                && rel.attr(None, "Type") == Some(ty.as_str())
                && rel.attr(None, "Target") == Some(target)
            {
                return Ok(id.to_owned());
            }
        }
    }
    let id = (1..=ids.len() + 1)
        .map(|n| format!("rIdAtrium{n}"))
        .find(|id| !ids.contains(id.as_str()))
        .ok_or_else(invalid)?;
    let target = quick_xml::escape::escape(target);
    let mode = if external {
        r#" TargetMode="External""#
    } else {
        ""
    };
    let rel = format!(
        r#"<Relationship xmlns="{namespace}" Id="{id}" Type="{ty}" Target="{target}"{mode}/>"#
    );
    let bytes = if let Some(old) = rels.as_ref() {
        let node = append_child(old.bytes(), old.root(), rel.as_bytes())?;
        let mut bytes = old.bytes().to_vec();
        bytes.splice(old.root().range.clone(), node);
        bytes
    } else {
        format!(r#"<Relationships xmlns="{namespace}">{rel}</Relationships>"#).into_bytes()
    };
    if bytes.len() > 1024 * 1024 {
        return Err(invalid());
    }
    *rels = Some(Arc::new(XmlPart::parse(bytes).map_err(|_| invalid())?));
    Ok(id)
}

fn one_text_run<'a>(run: &'a XmlNode, ns: &str) -> Result<&'a XmlNode, EditError> {
    let mut texts = run.children.iter().filter(|n| n.is(ns, "t"));
    let t = texts.next().ok_or(EditError::Locked)?;
    if texts.next().is_some()
        || !run.attributes.is_empty()
        || !t.children.is_empty()
        || t.attributes.iter().any(|a| {
            a.namespace.as_deref() != Some("http://www.w3.org/XML/1998/namespace")
                || a.local_name != "space"
        })
    {
        return Err(EditError::Locked);
    }
    Ok(t)
}

fn plain_text_run<'a>(node: &'a XmlNode, ns: &str) -> Result<&'a XmlNode, EditError> {
    if !node.is(ns, "r")
        || !node.text.trim().is_empty()
        || node
            .children
            .iter()
            .any(|n| !n.is(ns, "t") && !n.is(ns, "rPr"))
    {
        return Err(EditError::Locked);
    }
    one_text_run(node, ns)
}

fn slice_run(
    source: &[u8],
    run: &XmlNode,
    text: &XmlNode,
    value: &str,
) -> Result<Vec<u8>, EditError> {
    let mut out = source[run.range.start..text.range.start].to_vec();
    out.extend(write_text(source, text, value)?);
    out.extend_from_slice(&source[text.range.end..run.range.end]);
    Ok(out)
}

fn set_property(
    source: &[u8],
    p: &XmlNode,
    ns: &str,
    kind: &str,
    value: &str,
) -> Result<Vec<u8>, EditError> {
    let new = format!(r#"<w:{kind} xmlns:w="{ns}" w:val="{value}"/>"#);
    let start = p.range.start;
    let mut out = source[p.range.clone()].to_vec();
    if let Some(props) = child(p, ns, "pPr") {
        if let Some(previous) = child(props, ns, kind) {
            if !previous.children.is_empty()
                || previous
                    .attributes
                    .iter()
                    .any(|a| a.namespace.as_deref() != Some(ns) || a.local_name != "val")
            {
                return Err(EditError::Locked);
            }
            reject_unprojected_markup(&source[previous.range.clone()])?;
            out.splice(
                previous.range.start - start..previous.range.end - start,
                new.bytes(),
            );
        } else if let Some(next) = props.children.iter().find(|x| {
            [
                "textDirection",
                "textAlignment",
                "textboxTightWrap",
                "outlineLvl",
                "divId",
                "cnfStyle",
                "rPr",
                "sectPr",
                "pPrChange",
            ]
            .iter()
            .any(|kind| x.is(ns, kind))
        }) {
            let at = next.range.start - start;
            out.splice(at..at, new.bytes());
        } else {
            out.splice(
                props.range.start - start..props.range.end - start,
                append_child(source, props, new.as_bytes())?,
            );
        }
    } else {
        let ppr = format!(r#"<w:pPr xmlns:w="{ns}">{new}</w:pPr>"#);
        let end = tag_end(&out).ok_or_else(invalid)?;
        if out.get(end - 1) == Some(&b'/') {
            return append_child(source, p, ppr.as_bytes());
        }
        out.splice(end + 1..end + 1, ppr.bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Office, Op};
    const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";

    fn fixture(body: &str) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.docx");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let xml = format!(r#"<w:document xmlns:w="{W}"><w:body>{body}</w:body></w:document>"#);
        let types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = format!(
            r#"<Relationships xmlns="{OPC_REL}"><Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/></Relationships>"#
        );
        for (path, data) in [
            ("[Content_Types].xml", types),
            ("_rels/.rels", &rels),
            ("word/document.xml", &xml),
            ("word/unknown.dat", "opaque"),
        ] {
            zip.start_file(path, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        (dir, path, xml.into_bytes())
    }

    fn look_fixture(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("looks.docx");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let document = format!(
            r#"<w:document xmlns:w="{W}" xmlns:r="{R}"><w:body>{body}<w:sectPr><w:headerReference w:type="default" r:id="rId4"/><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1800" w:bottom="1440" w:left="1800" w:header="851" w:footer="992" w:gutter="0"/><w:cols w:num="2" w:space="425"/><w:docGrid w:linePitch="312"/></w:sectPr></w:body></w:document>"#
        );
        let styles = format!(
            r#"<w:styles xmlns:w="{W}"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:eastAsia="等线"/><w:sz w:val="21"/></w:rPr></w:rPrDefault></w:docDefaults><w:style w:type="paragraph" w:styleId="Body"><w:name w:val="Body"/><w:pPr><w:spacing w:before="60"/></w:pPr><w:rPr><w:rFonts w:asciiTheme="minorHAnsi" w:eastAsiaTheme="minorEastAsia"/><w:sz w:val="24"/><w:color w:val="333333"/></w:rPr></w:style></w:styles>"#
        );
        let numbering = format!(
            r#"<w:numbering xmlns:w="{W}"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:start w:val="3"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/><w:suff w:val="space"/><w:rPr><w:b/></w:rPr></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/><w:rPr><w:rFonts w:ascii="Symbol"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="7"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="8"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#
        );
        let theme = format!(
            r#"<a:theme xmlns:a="{A_NS}" name="T"><a:themeElements><a:clrScheme name="C"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme><a:fontScheme name="F"><a:majorFont><a:latin typeface="Aptos Display"/><a:ea typeface="宋体"/></a:majorFont><a:minorFont><a:latin typeface="Aptos"/><a:ea typeface="等线"/></a:minorFont></a:fontScheme></a:themeElements></a:theme>"#
        );
        let types = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/><Override PartName="/word/theme/theme1.xml" ContentType="application/vnd.openxmlformats-officedocument.theme+xml"/><Override PartName="/word/header1.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/></Types>"#;
        let root_rels = format!(
            r#"<Relationships xmlns="{OPC_REL}"><Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/></Relationships>"#
        );
        let document_rels = format!(
            r#"<Relationships xmlns="{OPC_REL}"><Relationship Id="rId1" Type="{R}/styles" Target="styles.xml"/><Relationship Id="rId2" Type="{R}/numbering" Target="numbering.xml"/><Relationship Id="rId3" Type="{R}/theme" Target="theme/theme1.xml"/><Relationship Id="rId4" Type="{R}/header" Target="header1.xml"/></Relationships>"#
        );
        let header =
            format!(r#"<w:hdr xmlns:w="{W}"><w:p><w:r><w:t>页眉</w:t></w:r></w:p></w:hdr>"#);
        for (name, data) in [
            ("[Content_Types].xml".to_owned(), types.to_owned()),
            ("_rels/.rels".to_owned(), root_rels),
            ("word/document.xml".to_owned(), document),
            ("word/_rels/document.xml.rels".to_owned(), document_rels),
            ("word/styles.xml".to_owned(), styles),
            ("word/numbering.xml".to_owned(), numbering),
            ("word/theme/theme1.xml".to_owned(), theme),
            ("word/header1.xml".to_owned(), header),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        (dir, path)
    }

    #[test]
    fn reads_effective_styles_numbering_theme_and_sections() {
        let body = r#"<w:p><w:pPr><w:pStyle w:val="Body"/><w:numPr><w:ilvl w:val="0"/><w:numId w:val="7"/></w:numPr><w:spacing w:after="240" w:line="360" w:lineRule="auto"/><w:ind w:left="720"/><w:jc w:val="both"/><w:keepNext/></w:pPr><w:r><w:rPr><w:color w:themeColor="accent1" w:themeTint="80"/><w:sz w:val="28"/></w:rPr><w:t>甲</w:t></w:r></w:p><w:tbl><w:tblPr><w:tblBorders><w:top w:val="single" w:sz="8" w:color="FF0000"/><w:insideH w:val="single" w:sz="4" w:color="00FF00"/></w:tblBorders><w:tblCellMar><w:left w:w="200"/></w:tblCellMar></w:tblPr><w:tblGrid><w:gridCol w:w="2000"/><w:gridCol w:w="3000"/></w:tblGrid><w:tr><w:trPr><w:tblHeader/><w:trHeight w:val="500" w:hRule="exact"/></w:trPr><w:tc><w:tcPr><w:shd w:fill="EEEEEE"/><w:vAlign w:val="center"/></w:tcPr><w:p><w:r><w:t>乙</w:t></w:r></w:p></w:tc><w:tc><w:p/></w:tc></w:tr></w:tbl>"#;
        let (_dir, path) = look_fixture(body);
        let package = Package::open(&path).unwrap();
        assert_eq!(
            package
                .related_part("word/document.xml", "theme")
                .unwrap()
                .as_deref(),
            Some("word/theme/theme1.xml")
        );
        let doc = crate::open(&path).unwrap();
        let Office::Word(word) = &doc.office else {
            panic!("not Word")
        };
        let Block::Paragraph(p) = &word.blocks()[0] else {
            panic!("not paragraph")
        };
        assert_eq!(p.look.before, 60);
        assert_eq!(p.look.after, 240);
        assert_eq!(p.look.line, crate::look::LineSpacing::Auto(360));
        assert_eq!(p.look.left, 720);
        assert!(p.look.keep_next);
        assert_eq!(p.align, Align::Justify);
        let list = p.look.list.as_ref().expect("list");
        assert_eq!(list.list, 7);
        assert_eq!(list.pattern, "%1.");
        assert_eq!(list.starts[0], 3);
        assert_eq!(list.suffix, crate::look::LabelSuffix::Space);
        assert!(matches!(list.formats[0], crate::look::NumFormat::Decimal));
        let Inline::Text { marks, .. } = &p.inlines[0] else {
            panic!("not text")
        };
        assert_eq!(marks.size, Some(280));
        assert_eq!(marks.fonts.latin, "Aptos");
        assert_eq!(marks.fonts.east_asia, "等线");
        assert_eq!(marks.color, Some(crate::look::Rgb(0xA2B9E2)));
        let Block::Table(t) = &word.blocks()[1] else {
            panic!("not table")
        };
        assert_eq!(t.look.columns, vec![2000, 3000]);
        assert_eq!(t.look.padding[3], 200);
        assert_eq!(t.look.rows[0], Some((500, true)));
        assert_eq!(t.look.header_rows, 1);
        assert_eq!(t.look.cells[0][0].shading, Some(crate::look::Rgb(0xEEEEEE)));
        assert_eq!(t.look.cells[0][0].v_align, crate::look::VAlign::Center);
        let sections = word.sections();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].page.columns, 2);
        assert_eq!(sections[0].page.line_pitch, Some(312));
        let header = sections[0].header.as_ref().expect("header");
        let Block::Paragraph(p) = &header[0] else {
            panic!("header paragraph")
        };
        assert_eq!(p.text(), "页眉");
    }

    /// A bullet level's mark comes from `lvlText`; an empty mark made the
    /// layout draw no bullet at all.
    #[test]
    fn a_bullet_level_keeps_its_lvl_text() {
        let body = r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="8"/></w:numPr></w:pPr><w:r><w:t>项目</w:t></w:r></w:p>"#;
        let (_dir, path) = look_fixture(body);
        let doc = crate::open(&path).unwrap();
        let Office::Word(word) = &doc.office else {
            panic!("not Word")
        };
        let Block::Paragraph(p) = &word.blocks()[0] else {
            panic!("not paragraph")
        };
        let list = p.look.list.as_ref().expect("list");
        assert_eq!(list.pattern, "•");
        assert!(
            matches!(&list.formats[0], crate::look::NumFormat::Bullet(mark) if mark == "•"),
            "formats: {:?}",
            list.formats
        );
    }

    #[test]
    fn writes_paragraph_look_and_reopens_without_touching_the_table() {
        let body = r#"<w:p><w:pPr><w:spacing w:before="60" w:after="240"/><w:ind w:left="720"/></w:pPr><w:r><w:t>甲</w:t></w:r></w:p><w:tbl><w:tblPr/><w:tblGrid><w:gridCol w:w="2000"/></w:tblGrid><w:tr><w:tc><w:p><w:r><w:t>乙</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
        let (dir, path) = look_fixture(body);
        let mut doc = crate::open(&path).unwrap();
        let id = paragraph(&doc, 0).id;
        let look = crate::look::ParaLook {
            before: 120,
            after: 0,
            line: crate::look::LineSpacing::Exact(300),
            left: 360,
            first_line: -180,
            keep_lines: true,
            widow_control: false,
            ..Default::default()
        };
        doc.apply(Op::Word(WordOp::SetLook {
            para: id,
            look: Box::new(look.clone()),
        }))
        .unwrap();
        let out = dir.path().join("look-saved.docx");
        doc.save_as(&out).unwrap();
        let reopened = crate::open(&out).unwrap();
        let Office::Word(word) = &reopened.office else {
            panic!("not Word")
        };
        let Block::Paragraph(p) = &word.blocks()[0] else {
            panic!("not paragraph")
        };
        assert_eq!(p.look.before, 120);
        assert_eq!(p.look.after, 0);
        assert_eq!(p.look.line, crate::look::LineSpacing::Exact(300));
        assert_eq!(p.look.left, 360);
        assert_eq!(p.look.first_line, -180);
        assert!(p.look.keep_lines);
        assert!(!p.look.widow_control);
        assert_eq!(p.text(), "甲");
        let package = Package::open(&out).unwrap();
        let xml = package.read_part("word/document.xml").unwrap();
        let xml = std::str::from_utf8(&xml).unwrap();
        assert!(xml.contains(r#"<w:t>乙</w:t>"#));
        assert!(xml.contains(r#"<w:gridCol w:w="2000"/>"#));
    }

    fn plain(text: &str) -> Inline {
        Inline::Text {
            text: text.into(),
            marks: Marks::default(),
        }
    }
    fn paragraph(doc: &crate::Document, at: usize) -> &Para {
        let Office::Word(word) = &doc.office else {
            panic!("not Word")
        };
        let Block::Paragraph(p) = &word.blocks()[at] else {
            panic!("not paragraph")
        };
        p
    }

    #[test]
    fn hyperlinks_round_trip_create_retarget_remove_and_exact_unsaved_undo() {
        let existing = r#"<w:hyperlink w:anchor='internal'><w:r><w:t>旧</w:t></w:r></w:hyperlink>"#;
        let (dir, source, _) = fixture(&format!(
            r#"<w:p>{existing}<w:r><w:rPr><w:rFonts w:ascii="FontA"/></w:rPr><w:t>甲乙</w:t></w:r><w:r><w:rPr><w:rFonts w:ascii="FontB"/></w:rPr><w:t>丙丁</w:t></w:r></w:p>"#
        ));
        let linked = |text: &str, target: Option<&str>| Inline::Text {
            text: text.into(),
            marks: Marks {
                link: target.map(str::to_owned),
                ..Marks::default()
            },
        };
        let o = "https://example.org/?a=1&b=2";
        let n = "mailto:hello@example.org";
        let mut doc = crate::open(&source).unwrap();
        let id = paragraph(&doc, 0).id;
        let create = Op::Word(WordOp::SetContent {
            para: id,
            inlines: vec![
                linked("旧", Some("#internal")),
                plain("甲"),
                linked("乙丙", Some(o)),
                plain("丁"),
            ],
        });
        let inv = doc.apply(create.clone()).unwrap();
        doc.apply(inv).unwrap();
        let identical = dir.path().join("identity.docx");
        doc.save_as(&identical).unwrap();
        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(&identical).unwrap()
        );
        doc.apply(create).unwrap();
        let target = dir.path().join("link.docx");
        doc.save_as(&target).unwrap();
        let raw = Package::open(&target).unwrap();
        let bytes = raw.read_part("word/document.xml").unwrap();
        let xml = std::str::from_utf8(&bytes).unwrap();
        assert!(xml.contains(existing));
        assert_eq!(xml.matches("FontA").count(), 2);
        assert_eq!(xml.matches("FontB").count(), 2);
        assert!(raw.has_part("word/_rels/document.xml.rels"));
        let read = crate::open(&target).unwrap();
        let count = paragraph(&read, 0)
            .inlines
            .iter()
            .filter(|i| matches!(i,Inline::Text{marks,..} if marks.link.as_deref()==Some(o)))
            .count();
        assert_eq!(count, 2);
        doc.apply(Op::Word(WordOp::SetContent {
            para: id,
            inlines: vec![
                linked("旧", Some("#internal")),
                plain("甲"),
                linked("乙", Some(n)),
                plain("丙丁"),
            ],
        }))
        .unwrap();
        doc.save().unwrap();
        let reread = crate::open(&target).unwrap();
        let applied = &paragraph(&reread, 0).inlines;
        assert!(applied.iter().any(
            |i| matches!(i,Inline::Text{text,marks} if text=="乙" && marks.link.as_deref()==Some(n))
        ));
        assert!(
            applied.iter().any(
                |i| matches!(i,Inline::Text{text,marks} if text=="丙" && marks.link.is_none())
            )
        );
    }

    #[test]
    fn list_style_creates_package_parts_reuses_and_returns_to_normal() {
        let (dir, source, _) = fixture(
            r#"<w:p><w:pPr><w:outlineLvl w:val="1"/><w:jc w:val="right"/></w:pPr><w:r><w:t>标题</w:t></w:r></w:p><w:p><w:r><w:t>第二条</w:t></w:r></w:p>"#,
        );
        let mut doc = crate::open(&source).unwrap();
        let first = paragraph(&doc, 0).id;
        let second = paragraph(&doc, 1).id;
        let list = ParaStyle::ListItem {
            level: 2,
            ordered: true,
        };
        let operation = Op::Word(WordOp::SetStyle {
            para: first,
            style: list.clone(),
        });
        let undo = doc.apply(operation.clone()).unwrap();
        doc.apply(undo).unwrap();
        let identity = dir.path().join("exact.docx");
        doc.save_as(&identity).unwrap();
        assert_eq!(
            std::fs::read(&source).unwrap(),
            std::fs::read(identity).unwrap()
        );
        doc.apply(operation).unwrap();
        doc.apply(Op::Word(WordOp::SetStyle {
            para: second,
            style: list.clone(),
        }))
        .unwrap();
        let target = dir.path().join("list.docx");
        doc.save_as(&target).unwrap();
        let reopened = crate::open(&target).unwrap();
        assert_eq!(paragraph(&reopened, 0).style, list);
        assert_eq!(paragraph(&reopened, 1).style, list);
        assert_eq!(paragraph(&reopened, 0).align, Align::Right);
        let package = Package::open(&target).unwrap();
        assert_eq!(
            package
                .related_part("word/document.xml", "numbering")
                .unwrap()
                .as_deref(),
            Some("word/numbering.xml")
        );
        let numbering = package.part("word/numbering.xml").unwrap();
        assert_eq!(
            numbering
                .root()
                .children
                .iter()
                .filter(|n| n.is(W, "num"))
                .count(),
            1
        );
        doc.apply(Op::Word(WordOp::SetStyle {
            para: first,
            style: ParaStyle::Normal,
        }))
        .unwrap();
        doc.save().unwrap();
        assert_eq!(
            paragraph(&crate::open(&target).unwrap(), 0).style,
            ParaStyle::Normal
        );
    }

    #[test]
    fn embedded_png_atom_uses_private_lifetime_and_only_internal_embed() {
        use std::io::Write;
        let (dir, path, _) = fixture("");
        let mut package = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let main = format!(
            r#"<w:document xmlns:w="{W}" xmlns:a="{DRAWING}" xmlns:r="{R}"><w:body><w:p><w:r><w:drawing><a:blip r:embed="local"/></w:drawing></w:r><w:r><w:drawing><a:blip r:link="remote"/></w:drawing></w:r></w:p></w:body></w:document>"#
        );
        let content = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="png" ContentType="image/png"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let root = format!(
            r#"<Relationships xmlns="{OPC_REL}"><Relationship Id="r" Type="{R}/officeDocument" Target="word/document.xml"/></Relationships>"#
        );
        let rel = format!(
            r#"<Relationships xmlns="{OPC_REL}"><Relationship Id="local" Type="{R}/image" Target="media/猫.png"/><Relationship Id="remote" Type="{R}/image" TargetMode="External" Target="https://example.invalid/cat.png"/></Relationships>"#
        );
        let png = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 0, 0, 0, 0, 58, 126, 155, 85, 0, 0, 0, 10, 73, 68, 65, 84, 120, 156, 99, 0, 0, 0, 2,
            0, 1, 72, 175, 164, 113, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ];
        for (name, bytes) in [
            ("[Content_Types].xml", content.as_bytes()),
            ("_rels/.rels", root.as_bytes()),
            ("word/document.xml", main.as_bytes()),
            ("word/_rels/document.xml.rels", rel.as_bytes()),
            ("word/media/猫.png", &png),
        ] {
            package
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            package.write_all(bytes).unwrap();
        }
        package.finish().unwrap();
        let doc = crate::open(&path).unwrap();
        let para = paragraph(&doc, 0);
        let Inline::Atom(Atom {
            kind: AtomKind::Image(Some(extract)),
            ..
        }) = &para.inlines[0]
        else {
            panic!("missing image path")
        };
        assert!(matches!(
            para.inlines[1],
            Inline::Atom(Atom {
                kind: AtomKind::Image(None),
                ..
            })
        ));
        let extracted = extract.clone();
        assert!(!extracted.starts_with(dir.path()));
        assert_eq!(std::fs::read(&extracted).unwrap(), png);
        drop(doc);
        assert!(!extracted.exists());
    }

    #[test]
    fn public_edit_preserves_other_runs_and_parts_and_reopens() {
        let raw_keep = r#"<w:r><w:rPr><w:color w:val="AABBCC"/></w:rPr><w:t>尾段</w:t></w:r>"#;
        let (dir, path, _) = fixture(&format!(
            r#"<w:p><w:r><w:rPr><w:rFonts w:ascii="F"/></w:rPr><w:t>原文</w:t></w:r>{raw_keep}</w:p><!--between--><w:p><w:r><w:t>邻段</w:t></w:r></w:p><w:sectPr/>"#
        ));
        let mut d = crate::open(&path).unwrap();
        let id = paragraph(&d, 0).id;
        d.apply(Op::Word(WordOp::SetContent {
            para: id,
            inlines: vec![plain("改稿尾段")],
        }))
        .unwrap();
        let out = dir.path().join("saved.docx");
        d.save_as(&out).unwrap();
        let raw = Package::open(&out).unwrap();
        let xml = raw.read_part("word/document.xml").unwrap();
        let xml = std::str::from_utf8(&xml).unwrap();
        assert!(xml.contains(raw_keep));
        assert!(xml.contains(r#"<w:rFonts w:ascii="F"/>"#));
        assert!(xml.contains("<!--between--><w:p><w:r><w:t>邻段</w:t></w:r></w:p>"));
        assert_eq!(raw.read_part("word/unknown.dat").unwrap(), b"opaque");
        assert_eq!(paragraph(&crate::open(&out).unwrap(), 0).text(), "改稿尾段");
    }

    #[test]
    fn undo_reconstructs_exact_archive_and_does_not_renumber() {
        let (dir, path, _) = fixture(
            r#"<w:p><w:r><w:t>&#x7532;</w:t></w:r></w:p><w:p><w:pPr><w:jc w:val="center"/></w:pPr><w:r><w:t>乙</w:t></w:r></w:p><w:sectPr/>"#,
        );
        let mut doc = crate::open(&path).unwrap();
        let id = paragraph(&doc, 0).id;
        let second = paragraph(&doc, 1).id;
        let first = doc
            .apply(Op::Word(WordOp::SetContent {
                para: id,
                inlines: vec![plain("改")],
            }))
            .unwrap();
        let second_undo = doc
            .apply(Op::Word(WordOp::DeletePara { para: second }))
            .unwrap();
        doc.apply(second_undo).unwrap();
        assert_eq!(paragraph(&doc, 1).id, second);
        doc.apply(first).unwrap();
        let target = dir.path().join("undo.docx");
        doc.save_as(&target).unwrap();
        assert_eq!(std::fs::read(target).unwrap(), std::fs::read(path).unwrap());
    }

    #[test]
    fn document_align_style_and_first_cell_position_reopen() {
        let (dir, path, _) = fixture(
            r#"<w:p><w:r><w:t>甲</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:tcPr/><w:p><w:r><w:t>格</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        );
        let mut doc = crate::open(&path).unwrap();
        let id = paragraph(&doc, 0).id;
        let Office::Word(word) = &doc.office else {
            panic!()
        };
        let table = word.blocks()[1].id();
        doc.apply(Op::Word(WordOp::SetStyle {
            para: id,
            style: ParaStyle::Heading(2),
        }))
        .unwrap();
        doc.apply(Op::Word(WordOp::SetAlign {
            para: id,
            align: Align::Center,
        }))
        .unwrap();
        doc.apply(Op::Word(WordOp::InsertPara {
            at: Place::CellStart {
                table,
                row: 0,
                col: 0,
            },
            id: None,
            style: ParaStyle::Normal,
            inlines: vec![plain("新")],
        }))
        .unwrap();
        let target = dir.path().join("changed.docx");
        doc.save_as(&target).unwrap();
        let reopened = crate::open(&target).unwrap();
        assert_eq!(paragraph(&reopened, 0).style, ParaStyle::Heading(2));
        assert_eq!(paragraph(&reopened, 0).align, Align::Center);
        let Office::Word(word) = reopened.office else {
            panic!()
        };
        let Block::Table(t) = &word.blocks()[1] else {
            panic!()
        };
        let Block::Paragraph(p) = &t.rows[0][0][0] else {
            panic!()
        };
        assert_eq!(p.text(), "新");
    }

    const PIC_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
    const WP_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";

    fn raw_package(
        name: &str,
        document: String,
        document_rels: String,
        footnotes: Option<String>,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let footnote_override = if footnotes.is_some() {
            r#"<Override PartName="/word/footnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"/>"#
        } else {
            ""
        };
        let types = format!(
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>{footnote_override}</Types>"#
        );
        let root_rels = format!(
            r#"<Relationships xmlns="{OPC_REL}"><Relationship Id="rId1" Type="{R}/officeDocument" Target="word/document.xml"/></Relationships>"#
        );
        let mut parts = vec![
            ("[Content_Types].xml".to_owned(), types),
            ("_rels/.rels".to_owned(), root_rels),
            ("word/document.xml".to_owned(), document),
            ("word/_rels/document.xml.rels".to_owned(), document_rels),
        ];
        if let Some(footnotes) = footnotes {
            parts.push(("word/footnotes.xml".to_owned(), footnotes));
        }
        for (name, data) in parts {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        (dir, path)
    }

    fn empty_rels() -> String {
        format!(r#"<Relationships xmlns="{OPC_REL}"></Relationships>"#)
    }

    #[test]
    fn snap_to_grid_defaults_on_and_can_be_turned_off() {
        let document = format!(
            r#"<w:document xmlns:w="{W}" xmlns:r="{R}"><w:body><w:p><w:r><w:t>one</w:t></w:r></w:p><w:p><w:pPr><w:snapToGrid w:val="0"/></w:pPr><w:r><w:t>two</w:t></w:r></w:p><w:sectPr><w:docGrid w:type="lines" w:linePitch="312"/></w:sectPr></w:body></w:document>"#
        );
        let (_dir, path) = raw_package("grid.docx", document, empty_rels(), None);
        let document = crate::open(&path).unwrap();
        let Office::Word(word) = document.office else {
            panic!("not a Word document")
        };
        let looks: Vec<bool> = word
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::Paragraph(p) => Some(p.look.snap_to_grid),
                _ => None,
            })
            .collect();
        assert_eq!(looks, vec![true, false]);
    }

    #[test]
    fn reads_picture_extent_and_square_wrap_clearance() {
        let body = format!(
            r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="1828800" cy="1371600"/><wp:docPr id="1" name="inline"/><a:graphic><a:graphicData uri="{PIC_NS}"><pic:pic><pic:blipFill><a:blip r:embed="rIdImg"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p><w:p><w:r><w:drawing><wp:anchor distT="127000" distR="190500" distB="254000" distL="317500" simplePos="0" relativeHeight="1" behindDoc="0" locked="0" layoutInCell="1" allowOverlap="1"><wp:extent cx="914400" cy="457200"/><wp:wrapSquare wrapText="bothSides"/><wp:docPr id="2" name="float"/><a:graphic><a:graphicData uri="{PIC_NS}"><pic:pic><pic:blipFill><a:blip r:embed="rIdImg"/></pic:blipFill></pic:pic></a:graphicData></a:graphic></wp:anchor></w:drawing></w:r></w:p>"#
        );
        let document = format!(
            r#"<w:document xmlns:w="{W}" xmlns:r="{R}" xmlns:wp="{WP_NS}" xmlns:a="{A_NS}" xmlns:pic="{PIC_NS}"><w:body>{body}</w:body></w:document>"#
        );
        let (_dir, path) = raw_package("drawing.docx", document, empty_rels(), None);
        let document = crate::open(&path).unwrap();
        let Office::Word(word) = document.office else {
            panic!("not a Word document")
        };
        let atoms: Vec<_> = word
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::Paragraph(p) => p.inlines.iter().find_map(|inline| match inline {
                    Inline::Atom(atom) if matches!(atom.kind, AtomKind::Image(_)) => Some(atom),
                    _ => None,
                }),
                _ => None,
            })
            .collect();
        assert_eq!(atoms.len(), 2);
        // EMU to twips: 635 EMU per twip.
        assert_eq!(atoms[0].look.size, Some((2880, 2160)));
        assert_eq!(atoms[1].look.size, Some((1440, 720)));
        let float = atoms[1].look.float.expect("floating picture");
        assert_eq!(float.wrap, crate::look::Wrap::Square);
        assert_eq!(float.clear, [200, 300, 400, 500]);
    }

    #[test]
    fn joins_footnote_part_to_its_reference_atom() {
        let document = format!(
            r#"<w:document xmlns:w="{W}" xmlns:r="{R}"><w:body><w:p><w:r><w:t>Body</w:t></w:r><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteReference w:id="1"/></w:r></w:p></w:body></w:document>"#
        );
        let footnotes = format!(
            r#"<w:footnotes xmlns:w="{W}"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:r><w:rPr><w:vertAlign w:val="superscript"/></w:rPr><w:footnoteRef/></w:r><w:r><w:t>note body</w:t></w:r></w:p></w:footnote></w:footnotes>"#
        );
        let rels = format!(
            r#"<Relationships xmlns="{OPC_REL}"><Relationship Id="rId1" Type="{R}/footnotes" Target="footnotes.xml"/></Relationships>"#
        );
        let (_dir, path) = raw_package("footnote.docx", document, rels, Some(footnotes));
        let document = crate::open(&path).unwrap();
        let Office::Word(word) = document.office else {
            panic!("not a Word document")
        };
        assert_eq!(word.footnotes().len(), 1);
        let Block::Paragraph(note) = &word.footnotes()[0].blocks[0] else {
            panic!("note has no paragraph")
        };
        assert_eq!(note.text(), "1 note body");
        let reference = word.blocks().iter().find_map(|block| match block {
            Block::Paragraph(p) => p.inlines.iter().find_map(|inline| match inline {
                Inline::Atom(atom) if matches!(atom.kind, AtomKind::Footnote) => Some(atom),
                _ => None,
            }),
            _ => None,
        });
        assert_eq!(reference.map(|atom| atom.id), Some(word.footnotes()[0].id));
    }
}
