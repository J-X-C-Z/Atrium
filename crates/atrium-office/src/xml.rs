//! Namespace-aware read-only XML with offsets into the original part.
use std::{collections::HashSet, ops::Range};

use quick_xml::{
    XmlVersion,
    escape::unescape,
    events::{BytesStart, Event},
    name::{Namespace, NamespaceResolver, PrefixDeclaration, QName, ResolveResult},
    reader::Reader,
};

use crate::OpenError;

const MAX_DEPTH: usize = 256;
// Compact adversarial XML can consume far more memory as a tree than on disk.
const MAX_NODES: usize = 250_000;
const MAX_ATTRIBUTES: usize = 256;
const MAX_NAME_BYTES: usize = 1024;
pub(crate) const MAX_PART_BYTES: usize = 64 * 1024 * 1024;
// Decompressed source size alone does not bound owned names, namespace copies,
// node records, and attribute records. Count all of these before storing them.
const MAX_TREE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct XmlPart {
    bytes: Vec<u8>,
    root: XmlNode,
}

#[derive(Debug)]
pub struct XmlNode {
    pub namespace: Option<String>,
    pub local_name: String,
    pub range: Range<usize>,
    pub children: Vec<XmlNode>,
    pub attributes: Vec<XmlAttribute>,
    /// Direct character data, excluding the text of child elements.
    pub text: String,
}

#[derive(Debug)]
pub struct XmlAttribute {
    pub namespace: Option<String>,
    pub local_name: String,
    pub value: String,
}

impl XmlNode {
    pub fn is(&self, namespace: &str, local_name: &str) -> bool {
        self.namespace.as_deref() == Some(namespace) && self.local_name == local_name
    }

    pub fn attr(&self, namespace: Option<&str>, local_name: &str) -> Option<&str> {
        self.attributes.iter().find_map(|a| {
            (a.namespace.as_deref() == namespace && a.local_name == local_name)
                .then_some(a.value.as_str())
        })
    }
}

impl XmlPart {
    pub fn parse(bytes: Vec<u8>) -> Result<Self, OpenError> {
        if bytes.len() > MAX_PART_BYTES {
            return Err(OpenError::TooBig);
        }
        let original = std::str::from_utf8(&bytes).map_err(damaged)?;
        check_chars(original)?;
        let bom_len = if bytes.starts_with(b"\xef\xbb\xbf") {
            3
        } else {
            0
        };
        let mut reader = Reader::from_reader(&bytes[bom_len..]);
        let mut resolver = NamespaceResolver::default();
        let mut budget = MAX_TREE_BYTES;
        reader.config_mut().check_comments = true;
        reader.config_mut().allow_unmatched_ends = false;
        reader.config_mut().check_end_names = true;
        let mut stack = Vec::<XmlNode>::new();
        let mut root = None;
        let mut count = 0;
        loop {
            let start = bom_len + reader.buffer_position() as usize;
            let event = reader.read_event().map_err(damaged)?;
            let end = bom_len + reader.buffer_position() as usize;
            let empty = matches!(event, Event::Empty(_));
            match event {
                Event::Start(element) | Event::Empty(element) => {
                    count += 1;
                    if count > MAX_NODES || stack.len() >= MAX_DEPTH {
                        return Err(OpenError::TooBig);
                    }
                    if stack.is_empty() && root.is_some() {
                        return Err(damaged("multiple root elements"));
                    }
                    let node = make_node(&mut resolver, &element, start..end, &mut budget)?;
                    if empty {
                        resolver.pop();
                        close_node(&mut stack, &mut root, node);
                    } else {
                        stack.push(node);
                    }
                }
                Event::End(_element) => {
                    let mut node = stack
                        .pop()
                        .ok_or_else(|| damaged("unmatched closing tag"))?;
                    node.range.end = end;
                    resolver.pop();
                    close_node(&mut stack, &mut root, node);
                }
                Event::Text(text) => {
                    let value = text.xml10_content().map_err(damaged)?;
                    if value.contains("]]>") {
                        return Err(damaged("CDATA terminator in text"));
                    }
                    if let Some(node) = stack.last_mut() {
                        charge(&mut budget, value.len())?;
                        node.text.push_str(&value);
                    } else if !value
                        .bytes()
                        .all(|b| matches!(b, b' ' | b'\r' | b'\n' | b'\t'))
                    {
                        return Err(damaged("non-whitespace outside root"));
                    }
                }
                Event::CData(text) => {
                    let node = stack
                        .last_mut()
                        .ok_or_else(|| damaged("CDATA outside root"))?;
                    let value = text.xml10_content().map_err(damaged)?;
                    charge(&mut budget, value.len())?;
                    node.text.push_str(&value);
                }
                Event::GeneralRef(reference) => {
                    let node = stack
                        .last_mut()
                        .ok_or_else(|| damaged("reference outside root"))?;
                    let name = std::str::from_utf8(reference.as_ref()).map_err(damaged)?;
                    if name.len() > 64 {
                        return Err(damaged("invalid character reference"));
                    }
                    let encoded = format!("&{name};");
                    let value = unescape(&encoded).map_err(damaged)?;
                    check_chars(&value)?;
                    charge(&mut budget, value.len())?;
                    node.text.push_str(&value);
                }
                Event::Decl(decl) => {
                    if start != bom_len {
                        return Err(damaged("misplaced XML declaration"));
                    }
                    check_declaration(&decl)?;
                }
                Event::DocType(_) => return Err(damaged("DTD is not allowed")),
                Event::PI(pi) => {
                    if pi.target().eq_ignore_ascii_case(b"xml") {
                        return Err(damaged("invalid XML declaration"));
                    }
                    check_name(QName(pi.target()))?;
                }
                Event::Comment(_) => {}
                Event::Eof => {
                    if !stack.is_empty() {
                        return Err(damaged("unclosed XML element"));
                    }
                    break;
                }
            }
        }
        Ok(Self {
            bytes,
            root: root.ok_or_else(|| damaged("missing root element"))?,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn root(&self) -> &XmlNode {
        &self.root
    }
}

fn close_node(stack: &mut [XmlNode], root: &mut Option<XmlNode>, node: XmlNode) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else {
        *root = Some(node);
    }
}

fn check_declaration(raw: &[u8]) -> Result<(), OpenError> {
    let raw = std::str::from_utf8(raw).map_err(damaged)?;
    let declaration = BytesStart::from_content(raw, 3);
    let mut attrs = declaration.attributes();
    let version = attrs
        .next()
        .ok_or_else(|| damaged("missing XML version"))?
        .map_err(damaged)?;
    if version.key.as_ref() != b"version" || version.value.as_ref() != b"1.0" {
        return Err(damaged("unsupported XML version"));
    }
    let mut next = attrs.next().transpose().map_err(damaged)?;
    if let Some(encoding) = next.as_ref()
        && encoding.key.as_ref() == b"encoding"
    {
        if !encoding.value.eq_ignore_ascii_case(b"UTF-8") {
            return Err(damaged("unsupported XML encoding"));
        }
        next = attrs.next().transpose().map_err(damaged)?;
    }
    if next.is_some_and(|standalone| {
        standalone.key.as_ref() != b"standalone"
            || !matches!(standalone.value.as_ref(), b"yes" | b"no")
    }) || attrs.next().is_some()
    {
        return Err(damaged("invalid XML declaration"));
    }
    Ok(())
}

fn charge(budget: &mut usize, amount: usize) -> Result<(), OpenError> {
    *budget = budget.checked_sub(amount).ok_or(OpenError::TooBig)?;
    Ok(())
}

fn make_node(
    resolver: &mut NamespaceResolver,
    element: &BytesStart<'_>,
    range: Range<usize>,
    budget: &mut usize,
) -> Result<XmlNode, OpenError> {
    check_name(element.name())?;
    charge(budget, std::mem::size_of::<XmlNode>())?;
    resolver.set_level(resolver.level() + 1);
    let mut raw_attributes = Vec::new();
    for (index, attribute) in element.attributes().enumerate() {
        if index >= MAX_ATTRIBUTES {
            return Err(OpenError::TooBig);
        }
        let attribute = attribute.map_err(damaged)?;
        check_name(attribute.key)?;
        if attribute.value.contains(&b'<') {
            return Err(damaged("unescaped < in attribute"));
        }
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(damaged)?;
        check_chars(&value)?;
        if let Some(prefix) = attribute.key.as_namespace_binding() {
            if value.len() > MAX_NAME_BYTES {
                return Err(OpenError::TooBig);
            }
            if matches!(prefix, PrefixDeclaration::Named(_)) && value.is_empty()
                || value == "http://www.w3.org/2000/xmlns/"
                || (matches!(prefix, PrefixDeclaration::Default)
                    && value == "http://www.w3.org/XML/1998/namespace")
            {
                return Err(damaged("invalid namespace binding"));
            }
            resolver
                .add(prefix, Namespace(value.as_bytes()))
                .map_err(damaged)?;
        } else {
            // normalized_value borrows the temporary Attribute. Copy only after
            // admission, then resolve after all declarations have been seen.
            charge(budget, std::mem::size_of::<XmlAttribute>() + value.len())?;
            raw_attributes.push((attribute.key, value.into_owned()));
        }
    }

    let (namespace, local) = resolver.resolve_element(element.name());
    let namespace = namespace_ref(namespace)?;
    if namespace == Some("http://www.w3.org/2000/xmlns/") {
        return Err(damaged("xmlns prefix cannot name an element"));
    }
    let local_name = std::str::from_utf8(local.into_inner()).map_err(damaged)?;
    charge(budget, local_name.len() + namespace.map_or(0, str::len))?;
    let mut attributes = Vec::with_capacity(raw_attributes.len());
    let mut names = HashSet::with_capacity(raw_attributes.len());
    for (key, value) in raw_attributes {
        let (ns, name) = resolver.resolve_attribute(key);
        let ns = namespace_ref(ns)?;
        let name = std::str::from_utf8(name.into_inner()).map_err(damaged)?;
        if !names.insert((ns, name)) {
            return Err(damaged("duplicate expanded attribute name"));
        }
        charge(budget, name.len() + ns.map_or(0, str::len))?;
        attributes.push(XmlAttribute {
            namespace: ns.map(str::to_owned),
            local_name: name.to_owned(),
            value,
        });
    }
    Ok(XmlNode {
        namespace: namespace.map(str::to_owned),
        local_name: local_name.to_owned(),
        range,
        children: Vec::new(),
        attributes,
        text: String::new(),
    })
}

fn namespace_ref(resolved: ResolveResult<'_>) -> Result<Option<&str>, OpenError> {
    match resolved {
        ResolveResult::Unbound => Ok(None),
        ResolveResult::Bound(ns) => {
            if ns.0.len() > MAX_NAME_BYTES {
                return Err(OpenError::TooBig);
            }
            std::str::from_utf8(ns.0).map(Some).map_err(damaged)
        }
        ResolveResult::Unknown(_) => Err(damaged("undeclared namespace prefix")),
    }
}

fn check_name(name: QName<'_>) -> Result<(), OpenError> {
    let bytes = name.as_ref();
    if bytes.len() > MAX_NAME_BYTES {
        return Err(OpenError::TooBig);
    }
    let name = std::str::from_utf8(bytes).map_err(damaged)?;
    let mut parts = name.split(':');
    let valid = |part: &str| {
        let mut chars = part.chars();
        chars.next().is_some_and(is_name_start) && chars.all(is_name_char)
    };
    if !parts.next().is_some_and(valid)
        || parts.next().is_some_and(|s| !valid(s))
        || parts.next().is_some()
    {
        return Err(damaged("invalid XML qualified name"));
    }
    Ok(())
}

fn is_name_start(c: char) -> bool {
    matches!(c, 'A'..='Z' | 'a'..='z' | '_' | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' |
        '\u{F8}'..='\u{2FF}' | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' |
        '\u{200C}'..='\u{200D}' | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' |
        '\u{3001}'..='\u{D7FF}' | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' |
        '\u{10000}'..='\u{EFFFF}')
}

fn is_name_char(c: char) -> bool {
    is_name_start(c)
        || matches!(c, '-' | '.' | '0'..='9' | '\u{B7}' | '\u{0300}'..='\u{036F}' | '\u{203F}'..='\u{2040}')
}

fn check_chars(s: &str) -> Result<(), OpenError> {
    if s.chars().all(|c| matches!(c, '\u{9}' | '\u{A}' | '\u{D}' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')) {
        Ok(())
    } else {
        Err(damaged("invalid XML character"))
    }
}

fn damaged(error: impl std::fmt::Display) -> OpenError {
    OpenError::Damaged(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_range_covers_only_elements_and_namespaces_are_scoped() {
        let xml = "\u{feff}<?xml version='1.0'?>\r\n<!--keep--><root xmlns='urn:a' xmlns:p='urn:b'>\r\n<p:子 p:attr='a&amp;b' attr='plain'>中文&#x1F680;&amp;<![CDATA[< ! >]]><!--stay--></p:子><child xmlns='urn:c'/><child/></root>\r\n";
        let part = XmlPart::parse(xml.as_bytes().to_vec()).unwrap();
        assert_eq!(part.bytes(), xml.as_bytes());
        let root = part.root();
        assert!(root.is("urn:a", "root"));
        assert_eq!(&part.bytes()[root.range.clone()][..5], b"<root");
        let child = &root.children[0];
        assert!(child.is("urn:b", "子"));
        assert_eq!(child.attr(Some("urn:b"), "attr"), Some("a&b"));
        assert_eq!(child.attr(None, "attr"), Some("plain"));
        assert_eq!(child.text, "中文🚀&< ! >");
        assert_eq!(
            &xml[child.range.clone()],
            "<p:子 p:attr='a&amp;b' attr='plain'>中文&#x1F680;&amp;<![CDATA[< ! >]]><!--stay--></p:子>"
        );
        assert!(root.children[1].is("urn:c", "child"));
        assert!(root.children[2].is("urn:a", "child"));
        assert_eq!(&xml[root.children[2].range.clone()], "<child/>");
    }

    #[test]
    fn depth_limit_includes_empty_elements() {
        let doc = |depth| format!("{}<last/>{}", "<a>".repeat(depth), "</a>".repeat(depth));
        assert!(XmlPart::parse(doc(255).into_bytes()).is_ok());
        assert!(matches!(
            XmlPart::parse(doc(256).into_bytes()),
            Err(OpenError::TooBig)
        ));
    }

    #[test]
    fn rejects_owned_tree_amplification_even_when_source_is_small() {
        let namespace = "n".repeat(1000);
        let xml = format!("<a xmlns='{namespace}'>{}</a>", "<b/>".repeat(70_000));
        assert!(xml.len() < 300_000);
        assert!(matches!(
            XmlPart::parse(xml.into_bytes()),
            Err(OpenError::TooBig)
        ));
    }

    #[test]
    fn namespace_bindings_use_normalized_attribute_values() {
        let xml = b"<?xml version='1.0' encoding='utf-8' standalone='yes'?><p:a xmlns:p='urn:a&amp;b' p:x='a\r\nb&#x9;c'/>";
        let part = XmlPart::parse(xml.to_vec()).unwrap();
        assert!(part.root().is("urn:a&b", "a"));
        assert_eq!(part.root().attr(Some("urn:a&b"), "x"), Some("a b\tc"));
    }

    #[test]
    fn invalid_and_external_markup_is_never_interpreted() {
        for input in [
            "<",
            "<a>",
            "<a></b>",
            "<a/><b/>",
            "text <a/>",
            "<p:a/>",
            "<a x='1' x='2'/>",
            "<a xmlns:p='urn:x' xmlns:q='urn:x' p:x='1' q:x='2'/>",
            "<!DOCTYPE a [<!ENTITY x SYSTEM 'about:blank'>]><a>&x;</a>",
            "<a>&undefined;</a>",
            "<a>&#1;</a>",
            "<a><![CDATA[ok]]>]]></a>",
            "<a x='<bad>'/>",
            "<p:a xmlns:p='urn:a' xmlns:q='urn:a'></q:a>",
            "<a:b:c/>",
            "<0/>",
            "<a></a><!--bad--comment-->",
            "<?xml version='1.0' encoding='UTF-16'?><a/>",
            "<?xml version='1.0' foo='yes'?><a/>",
            "<?xml version='1.0' standalone='xyz'?><a/>",
            "<a/><?xml version='1.0'?>",
        ] {
            assert!(
                matches!(
                    XmlPart::parse(input.as_bytes().to_vec()),
                    Err(OpenError::Damaged(_))
                ),
                "{input}"
            );
        }
        assert!(matches!(
            XmlPart::parse(b"<a>\xff</a>".to_vec()),
            Err(OpenError::Damaged(_))
        ));
    }
}
