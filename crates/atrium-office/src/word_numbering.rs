//! Numbering mutation for the existing Word paragraph style operation.
use crate::{
    EditError,
    xml::{XmlNode, XmlPart},
};
use std::collections::HashMap;
const CT: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
const NUM_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";

fn invalid() -> EditError {
    EditError::Invalid("unsupported or invalid Word numbering".into())
}
fn child<'a>(n: &'a XmlNode, ns: &str, k: &str) -> Option<&'a XmlNode> {
    n.children.iter().find(|e| e.is(ns, k))
}
fn value<'a>(n: &'a XmlNode, ns: &str, k: &str) -> Option<&'a str> {
    child(n, ns, k)?.attr(Some(ns), "val")
}
fn id(n: &XmlNode, ns: &str, attribute: &str) -> Result<u32, EditError> {
    n.attr(Some(ns), attribute)
        .and_then(|s| s.parse().ok())
        .ok_or_else(invalid)
}
fn put_child(xml: &XmlPart, node: &XmlNode, child: &[u8]) -> Result<XmlPart, EditError> {
    let mut raw = xml.bytes().to_vec();
    let envelope = &xml.bytes()[node.range.clone()];
    let mut quote = None;
    let end = envelope
        .iter()
        .position(|&b| {
            if let Some(q) = quote {
                if b == q {
                    quote = None;
                }
                false
            } else if b == b'\'' || b == b'"' {
                quote = Some(b);
                false
            } else {
                b == b'>'
            }
        })
        .ok_or_else(invalid)?;
    if envelope.get(end.saturating_sub(1)) == Some(&b'/') {
        let n = envelope[1..]
            .iter()
            .position(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'>'))
            .ok_or_else(invalid)?
            + 1;
        let mut insertion = vec![b'>'];
        insertion.extend_from_slice(child);
        insertion.extend_from_slice(b"</");
        insertion.extend_from_slice(&envelope[1..n]);
        insertion.push(b'>');
        raw.splice(
            node.range.start + end - 1..node.range.start + end + 1,
            insertion,
        );
    } else {
        let tail = envelope
            .windows(2)
            .rposition(|b| b == b"</")
            .ok_or_else(invalid)?
            + node.range.start;
        raw.splice(tail..tail, child.iter().copied());
    }
    XmlPart::parse(raw).map_err(|_| invalid())
}
fn manifest_override(manifest: &XmlPart, name: &str) -> Result<XmlPart, EditError> {
    let root = manifest.root();
    if !root.is(CT, "Types")
        || name.len() > 4096
        || !name.ends_with("numbering.xml")
        || !name.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
    {
        return Err(invalid());
    }
    let part = format!("/{name}");
    if root.children.iter().any(|n| {
        n.is(CT, "Override")
            && n.attr(None, "PartName")
                .is_some_and(|v| v.eq_ignore_ascii_case(&part))
    }) {
        return Err(invalid());
    }
    let node = format!(r#"<Override xmlns="{CT}" PartName="{part}" ContentType="{NUM_TYPE}"/>"#);
    put_child(manifest, root, node.as_bytes())
}

type Selection = (String, Option<XmlPart>, Option<XmlPart>);
/// Reuse an effective numbering level of the requested kind or create a
/// single numbering instance. New parts require a content type override.
pub(crate) fn with_list(
    ns: &str,
    numbering: Option<&XmlPart>,
    manifest: &XmlPart,
    name: &str,
    level: u8,
    ordered: bool,
) -> Result<Selection, EditError> {
    if level > 8
        || !matches!(
            ns,
            "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                | "http://purl.oclc.org/ooxml/wordprocessingml/main"
        )
    {
        return Err(invalid());
    }
    let mut abstracts = HashMap::new();
    let mut ids = HashMap::new();
    let mut previous_order = Vec::new();
    if let Some(p) = numbering {
        let root = p.root();
        if !root.is(ns, "numbering") {
            return Err(invalid());
        }
        for ch in &root.children {
            if ch.is(ns, "abstractNum") {
                let n = id(ch, ns, "abstractNumId")?;
                if abstracts.insert(n, ch).is_some() {
                    return Err(invalid());
                }
            } else if ch.is(ns, "num") {
                let n = id(ch, ns, "numId")?;
                if ids.insert(n, ch).is_some() {
                    return Err(invalid());
                }
                previous_order.push((n, ch));
            }
        }
    }
    let wanted = u32::from(level);
    let first_level =
        |n: &XmlNode| n.attr(Some(ns), "ilvl").and_then(|s| s.parse::<u32>().ok()) == Some(wanted);
    for (num_id, n) in previous_order {
        if num_id == 0 {
            continue;
        }
        let selected = n
            .children
            .iter()
            .find(|ch| ch.is(ns, "lvlOverride") && first_level(ch))
            .and_then(|over| child(over, ns, "lvl"));
        let base = value(n, ns, "abstractNumId")
            .and_then(|s| s.parse::<u32>().ok())
            .and_then(|id| abstracts.get(&id))
            .and_then(|root| {
                root.children
                    .iter()
                    .find(|ch| ch.is(ns, "lvl") && first_level(ch))
            });
        let Some(selection) = selected.or(base) else {
            continue;
        };
        let f = value(selection, ns, "numFmt").unwrap_or("decimal");
        if f != "none" && (f != "bullet") == ordered {
            return Ok((num_id.to_string(), None, None));
        }
    }
    let abstract_id = abstracts
        .keys()
        .max()
        .map_or(Some(0), |id| id.checked_add(1))
        .ok_or_else(invalid)?;
    let num_id = ids
        .keys()
        .max()
        .map_or(Some(1), |id| id.checked_add(1))
        .ok_or_else(invalid)?
        .max(1);
    let mut abstract_xml = format!(
        r#"<w:abstractNum xmlns:w="{ns}" w:abstractNumId="{abstract_id}"><w:multiLevelType w:val="multilevel"/>"#
    );
    let num_fmt = if ordered { "decimal" } else { "bullet" };
    for k in 0..9 {
        let txt = if ordered {
            format!("%{}.", k + 1)
        } else {
            "•".into()
        };
        let left = (k + 1) * 360;
        abstract_xml.push_str(&format!(r#"<w:lvl w:ilvl="{k}"><w:start w:val="1"/><w:numFmt w:val="{num_fmt}"/><w:lvlText w:val="{txt}"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="{left}" w:hanging="180"/></w:pPr></w:lvl>"#));
    }
    abstract_xml.push_str("</w:abstractNum>");
    let num = format!(
        r#"<w:num xmlns:w="{ns}" w:numId="{num_id}"><w:abstractNumId w:val="{abstract_id}"/></w:num>"#
    );
    let (new, content) = if let Some(p) = numbering {
        let root = p.root();
        let first_num = root
            .children
            .iter()
            .find(|ch| ch.is(ns, "num") || ch.is(ns, "numIdMacAtCleanup"));
        let cleanup = root
            .children
            .iter()
            .find(|ch| ch.is(ns, "numIdMacAtCleanup"));
        let with_complete = if let Some(first) = first_num {
            let mut bytes = p.bytes().to_vec();
            if let Some(c) = cleanup {
                bytes.splice(c.range.start..c.range.start, num.bytes());
            } else {
                let tail = p.bytes()[root.range.clone()]
                    .windows(2)
                    .rposition(|b| b == b"</")
                    .ok_or_else(invalid)?
                    + root.range.start;
                bytes.splice(tail..tail, num.bytes());
            }
            bytes.splice(first.range.start..first.range.start, abstract_xml.bytes());
            XmlPart::parse(bytes).map_err(|_| invalid())?
        } else {
            put_child(p, root, format!("{abstract_xml}{num}").as_bytes())?
        };
        (with_complete, None)
    } else {
        let body = format!(r#"<w:numbering xmlns:w="{ns}">{abstract_xml}{num}</w:numbering>"#);
        (
            XmlPart::parse(body.into_bytes()).map_err(|_| invalid())?,
            Some(manifest_override(manifest, name)?),
        )
    };
    Ok((num_id.to_string(), Some(new), content))
}

#[cfg(test)]
mod tests {
    use super::*;
    const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    fn xml(s: String) -> XmlPart {
        XmlPart::parse(s.into_bytes()).unwrap()
    }
    #[test]
    fn new_numbering_adds_manifest_then_reuses_compatible_level() {
        let manifest = xml(format!(r#"<Types xmlns="{CT}" authored="verbatim"/>"#));
        let (id, num, types) =
            with_list(W, None, &manifest, "word/numbering.xml", 3, true).unwrap();
        let num = num.unwrap();
        let types = types.unwrap();
        assert_eq!(id, "1");
        assert!(String::from_utf8_lossy(types.bytes()).contains(r#"authored="verbatim""#));
        assert!(String::from_utf8_lossy(types.bytes()).contains(NUM_TYPE));
        let (id, next, types) =
            with_list(W, Some(&num), &manifest, "word/numbering.xml", 3, true).unwrap();
        assert_eq!(id, "1");
        assert!(next.is_none() && types.is_none());
        let (id, next, _) =
            with_list(W, Some(&num), &manifest, "word/numbering.xml", 3, false).unwrap();
        assert_eq!(id, "2");
        assert!(String::from_utf8_lossy(next.unwrap().bytes()).contains("bullet"));
    }
}
