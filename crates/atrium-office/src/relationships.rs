//! OPC relationship metadata; external targets are never opened.
use crate::{OpenError, Package};
const NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
const STRICT: &str = "http://purl.oclc.org/ooxml/package/relationships";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rel {
    pub id: String,
    pub relation_type: String,
    pub target: String,
    pub external: bool,
}
impl Rel {
    pub fn kind(&self, kind: &str) -> bool {
        [
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships/",
            "http://purl.oclc.org/ooxml/officeDocument/relationships/",
        ]
        .iter()
        .any(|prefix| self.relation_type.strip_prefix(prefix) == Some(kind))
    }
}
fn bad(msg: &str) -> OpenError {
    OpenError::Damaged(msg.into())
}
impl Package {
    pub fn relationships(&self, source: &str) -> Result<Vec<Rel>, OpenError> {
        let part = if source.is_empty() {
            "_rels/.rels".into()
        } else {
            let (dir, base) = source.rsplit_once('/').unwrap_or(("", source));
            if dir.is_empty() {
                format!("_rels/{base}.rels")
            } else {
                format!("{dir}/_rels/{base}.rels")
            }
        };
        if !self.has_part(&part) {
            return Ok(Vec::new());
        }
        let xml = self.part(&part)?;
        let root = xml.root();
        let ns = root
            .namespace
            .as_deref()
            .filter(|ns| *ns == NS || *ns == STRICT)
            .ok_or_else(|| bad("invalid relationship namespace"))?;
        if !root.is(ns, "Relationships") {
            return Err(bad("invalid relationship root"));
        }
        let mut rels = Vec::new();
        let mut ids = std::collections::HashSet::new();
        for n in &root.children {
            if !n.is(ns, "Relationship") {
                continue;
            }
            let id = n
                .attr(None, "Id")
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("missing relationship ID"))?;
            if !ids.insert(id) {
                return Err(bad("duplicate relationship ID"));
            }
            let relation_type = n
                .attr(None, "Type")
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("missing relationship type"))?
                .to_owned();
            let target = n
                .attr(None, "Target")
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("missing relationship target"))?;
            let external = match n.attr(None, "TargetMode") {
                None | Some("Internal") => false,
                Some("External") => true,
                _ => return Err(bad("invalid TargetMode")),
            };
            rels.push(Rel {
                id: id.to_owned(),
                relation_type,
                target: if external {
                    target.to_owned()
                } else {
                    resolve(source, target)?
                },
                external,
            });
        }
        Ok(rels)
    }
    /// Locate a unique internal relationship of a well-known OOXML type.
    pub fn related_part(&self, source: &str, kind: &str) -> Result<Option<String>, OpenError> {
        let mut rels = self
            .relationships(source)?
            .into_iter()
            .filter(|r| !r.external && r.kind(kind));
        let target = rels.next().map(|r| r.target);
        if rels.next().is_some() {
            return Err(bad("ambiguous relationship type"));
        }
        Ok(target)
    }
}
fn resolve(source: &str, target: &str) -> Result<String, OpenError> {
    // Part references may use escaped URI bytes. Decode a component at a time
    // without allowing percent escapes to introduce separators or dot segments.
    if target.contains(['\\', ':', '?', '#']) || target.starts_with("//") {
        return Err(bad("unsupported internal target URI"));
    }
    let dir = source.rsplit_once('/').map_or("", |(dir, _)| dir);
    let mut components = if target.starts_with('/') || dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').map(str::to_owned).collect()
    };
    for p in target.strip_prefix('/').unwrap_or(target).split('/') {
        match p {
            "." => {}
            ".." => {
                components
                    .pop()
                    .ok_or_else(|| bad("target escapes package root"))?;
            }
            "" => return Err(bad("empty target component")),
            _ => {
                let mut out = Vec::new();
                let mut b = p.as_bytes().iter().copied();
                while let Some(c) = b.next() {
                    if c == b'%' {
                        let digit = |ch: u8| {
                            (ch as char)
                                .to_digit(16)
                                .map(|v| v as u8)
                                .ok_or_else(|| bad("invalid percent encoding"))
                        };
                        let next = b.next().ok_or_else(|| bad("truncated percent encoding"))?;
                        let last = b.next().ok_or_else(|| bad("truncated percent encoding"))?;
                        out.push(digit(next)? * 16 + digit(last)?);
                    } else {
                        out.push(c);
                    }
                }
                let name = String::from_utf8(out).map_err(|_| bad("invalid target UTF-8"))?;
                if name == "." || name == ".." || name.contains(['/', '\\', ':', '?', '#', '\0']) {
                    return Err(bad("invalid target component"));
                }
                components.push(name);
            }
        }
    }
    if components.is_empty() {
        return Err(bad("empty target"));
    }
    Ok(components.join("/"))
}
impl Package {
    /// Validate the declared OOXML main-part content type for editable formats.
    pub(crate) fn validate_main_type(&self, extension: &str) -> Result<(), OpenError> {
        let (fallback, expected) = match extension {
            "docx" => (
                "word/document.xml",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
            ),
            "xlsx" => (
                "xl/workbook.xml",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
            ),
            "pptx" => (
                "ppt/presentation.xml",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
            ),
            _ => return Err(bad("expected .docx, .xlsx or .pptx")),
        };
        let main = self
            .related_part("", "officeDocument")?
            .unwrap_or_else(|| fallback.into());
        let xml = self.part("[Content_Types].xml")?;
        let root = xml.root();
        const CONTENTS: &str = "http://schemas.openxmlformats.org/package/2006/content-types";
        const STRICT_CONTENTS: &str = "http://purl.oclc.org/ooxml/package/content-types";
        let ns = root
            .namespace
            .as_deref()
            .filter(|n| *n == CONTENTS || *n == STRICT_CONTENTS)
            .ok_or_else(|| bad("invalid content types namespace"))?;
        if !root.is(ns, "Types") {
            return Err(bad("invalid content types root"));
        }
        let mut specific = None;
        let mut default = None;
        for n in &root.children {
            if n.is(ns, "Override")
                && let Some(name) = n.attr(None, "PartName")
                && name.starts_with('/')
                && resolve("", name)? == main
                && specific
                    .replace(
                        n.attr(None, "ContentType")
                            .ok_or_else(|| bad("missing main ContentType"))?,
                    )
                    .is_some()
            {
                return Err(bad("ambiguous main ContentType"));
            }
            if n.is(ns, "Default")
                && n.attr(None, "Extension").is_some_and(|x| {
                    main.rsplit_once('.')
                        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case(x))
                })
                && default
                    .replace(
                        n.attr(None, "ContentType")
                            .ok_or_else(|| bad("missing default ContentType"))?,
                    )
                    .is_some()
            {
                return Err(bad("ambiguous default ContentType"));
            }
        }
        if specific.or(default) != Some(expected) || !self.has_part(&main) {
            return Err(bad("main-part ContentType does not match file format"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_without_escaping_or_fetching() {
        assert_eq!(
            resolve("custom/word.xml", "../media/%E4%B8%AD.png").unwrap(),
            "media/中.png"
        );
        for value in [
            "../../a",
            "%2e%2e/a",
            "one%2ftwo",
            "https://example.com",
            "//outside",
        ] {
            assert!(resolve("custom/word.xml", value).is_err());
        }
    }
}
