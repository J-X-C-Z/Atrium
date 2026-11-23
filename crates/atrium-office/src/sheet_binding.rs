//! Sparse SpreadsheetML projection and localized cell-edit planning.
use crate::{
    OpenError, SaveError,
    package::Package,
    sheet,
    word::EditError,
    xml::{XmlNode, XmlPart},
};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const STRICT: &str = "http://purl.oclc.org/ooxml/spreadsheetml/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const STRICT_REL: &str = "http://purl.oclc.org/ooxml/officeDocument/relationships";
const MAIN: &str = "xl/workbook.xml";
type Patches = Vec<(String, Vec<u8>)>;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(String),
    Date(f64),
}
#[derive(Debug, Clone)]
pub struct Cell {
    pub value: Value,
    pub formula: Option<String>,
    pub style: u32,
}
#[derive(Debug)]
pub struct Sheet {
    pub name: String,
    pub hidden: bool,
    pub cells: BTreeMap<(u32, u32), Cell>,
    pub merged: Vec<String>,
    pub widths: Vec<(u32, u32, f64)>,
    path: String,
    locked: Vec<CellRange>,
    unknown_pivot: bool,
}
#[derive(Debug)]
pub(crate) struct SheetBinding {
    baseline: sheet::Workbook,
    pub sheets: Vec<Sheet>,
    formats: Vec<String>,
    bold_styles: Vec<bool>,
    part: String,
    date1904: bool,
}
fn bad(s: &str) -> OpenError {
    OpenError::Damaged(s.into())
}
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
fn parse_addr(s: &str) -> Option<(u32, u32)> {
    let i = s.find(|c: char| !c.is_ascii_uppercase())?;
    if i == 0 || i > 3 || s.len() > 10 {
        return None;
    }
    let col = s[..i]
        .bytes()
        .fold(0u32, |a, b| a * 26 + u32::from(b - b'A' + 1));
    let suffix = &s[i..];
    if suffix.starts_with('0') || !suffix.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let row = suffix.parse().ok()?;
    ((1..=1_048_576).contains(&row) && col <= 16_384).then_some((row, col))
}
#[derive(Debug)]
struct CellRange {
    first: (u32, u32),
    last: (u32, u32),
}
impl CellRange {
    fn parse(s: &str) -> Option<Self> {
        let (a, b) = s.split_once(':').unwrap_or((s, s));
        let first = parse_addr(a)?;
        let last = parse_addr(b)?;
        (first.0 <= last.0 && first.1 <= last.1).then_some(Self { first, last })
    }
    fn contains(&self, r: u32, c: u32) -> bool {
        (self.first.0..=self.last.0).contains(&r) && (self.first.1..=self.last.1).contains(&c)
    }
}
fn addr(r: u32, mut c: u32) -> String {
    let mut chars = Vec::new();
    while c > 0 {
        c -= 1;
        chars.push(char::from(b'A' + (c % 26) as u8));
        c /= 26;
    }
    chars.into_iter().rev().collect::<String>() + &r.to_string()
}
fn string(n: &XmlNode) -> String {
    let mut s = String::new();
    for x in &n.children {
        if is(x, "t") {
            s.push_str(&x.text)
        } else if is(x, "r")
            && let Some(t) = child(x, "t")
        {
            s.push_str(&t.text)
        }
    }
    s
}
impl SheetBinding {
    fn open(package: &Package) -> Result<Self, OpenError> {
        let part = package
            .related_part("", "officeDocument")?
            .unwrap_or_else(|| MAIN.into());
        let main = package.part(&part)?;
        if !is(main.root(), "workbook") {
            return Err(bad("not a SpreadsheetML workbook"));
        }
        let rels = package.relationships(&part)?;
        let ids: HashMap<_, _> = rels
            .iter()
            .filter(|rel| !rel.external && rel.kind("worksheet"))
            .map(|rel| (rel.id.as_str(), &rel.target))
            .collect();
        let strings = package.related_part(&part, "sharedStrings")?;
        let styles = package.related_part(&part, "styles")?;
        let shared = if let Some(path) = strings {
            package
                .part(&path)?
                .root()
                .children
                .iter()
                .filter(|x| is(x, "si"))
                .map(string)
                .collect()
        } else {
            Vec::new()
        };
        let (formats, bold_styles) = if let Some(path) = styles {
            let xml = package.part(&path)?;
            (read_formats(xml.root()), read_bold(xml.root()))
        } else {
            (Vec::new(), Vec::new())
        };
        let date1904 = child(main.root(), "workbookPr")
            .and_then(|n| attr(n, "date1904"))
            .is_some_and(|x| matches!(x, "1" | "true"));
        let mut sheets = Vec::new();
        let mut text_bytes = 0usize;
        for n in child(main.root(), "sheets")
            .ok_or_else(|| bad("missing sheets"))?
            .children
            .iter()
            .filter(|x| is(x, "sheet"))
        {
            let id = n
                .attr(Some(REL), "id")
                .or_else(|| n.attr(Some(STRICT_REL), "id"))
                .ok_or_else(|| bad("missing sheet id"))?;
            let Some(path) = ids.get(id) else { continue };
            let xml = package.part(path)?;
            let data = child(xml.root(), "sheetData").ok_or_else(|| bad("missing sheet data"))?;
            let mut cells = BTreeMap::new();
            let mut locked = Vec::new();
            for row in data.children.iter().filter(|x| is(x, "row")) {
                for n in row.children.iter().filter(|x| is(x, "c")) {
                    let (r, c) = attr(n, "r")
                        .and_then(parse_addr)
                        .ok_or_else(|| bad("bad cell address"))?;
                    let style = attr(n, "s")
                        .and_then(|x| x.parse::<u32>().ok())
                        .unwrap_or(0);
                    let date = formats.get(style as usize).is_some_and(|x| date_format(x));
                    let v = child(n, "v").map(|v| v.text.as_str());
                    let value = match (attr(n, "t").unwrap_or("n"), v) {
                        ("inlineStr", _) => {
                            Value::Text(child(n, "is").map(string).unwrap_or_default())
                        }
                        ("s", Some(v)) => Value::Text(
                            shared
                                .get(
                                    v.parse::<usize>()
                                        .map_err(|_| bad("bad shared string index"))?,
                                )
                                .ok_or_else(|| bad("missing shared string"))?
                                .clone(),
                        ),
                        ("b", Some("1")) => Value::Bool(true),
                        ("b", Some("0")) => Value::Bool(false),
                        ("e", Some(v)) => Value::Error(v.into()),
                        ("n", Some("")) => Value::Empty,
                        ("n", Some(v)) => match v.parse::<f64>().ok().filter(|x| x.is_finite()) {
                            Some(x) if date => Value::Date(x),
                            Some(x) => Value::Number(x),
                            None => Value::Text(v.into()),
                        },
                        (_, Some(v)) => Value::Text(v.into()),
                        (_, None) => Value::Empty,
                    };
                    if let Some(f) = child(n, "f")
                        && let Some(kind @ ("array" | "shared" | "dataTable")) = attr(f, "t")
                    {
                        let range = attr(f, "ref")
                            .map(|s| {
                                CellRange::parse(s)
                                    .ok_or_else(|| bad("malformed grouped formula range"))
                            })
                            .transpose()?;
                        if let Some(range) = range {
                            if kind == "array"
                                && range.first == (r, c)
                                && attr(n, "cm").is_none()
                                && attr(n, "vm").is_none()
                            {
                                if range.last.0 > r {
                                    locked.push(CellRange {
                                        first: (r + 1, c),
                                        last: range.last,
                                    });
                                }
                                if range.last.1 > c {
                                    locked.push(CellRange {
                                        first: (r, c + 1),
                                        last: (r, range.last.1),
                                    });
                                }
                            } else {
                                locked.push(range);
                            }
                        } else {
                            locked.push(CellRange {
                                first: (r, c),
                                last: (r, c),
                            });
                        }
                    }
                    let formula = child(n, "f").map(|f| f.text.clone());
                    text_bytes = text_bytes
                        .saturating_add(match &value {
                            Value::Text(s) | Value::Error(s) => s.len(),
                            _ => 0,
                        })
                        .saturating_add(formula.as_ref().map_or(0, String::len));
                    if text_bytes > 64 * 1024 * 1024 {
                        return Err(OpenError::TooBig);
                    }

                    if cells
                        .insert(
                            (r, c),
                            Cell {
                                style,
                                value,
                                formula,
                            },
                        )
                        .is_some()
                    {
                        return Err(bad("duplicate cell"));
                    }
                }
            }
            let merged = child(xml.root(), "mergeCells")
                .into_iter()
                .flat_map(|x| &x.children)
                .filter(|x| is(x, "mergeCell"))
                .filter_map(|x| attr(x, "ref").map(str::to_owned))
                .collect();
            let widths = child(xml.root(), "cols")
                .into_iter()
                .flat_map(|x| &x.children)
                .filter(|x| is(x, "col"))
                .filter_map(|x| {
                    Some((
                        attr(x, "min")?.parse().ok()?,
                        attr(x, "max")?.parse().ok()?,
                        attr(x, "width")?.parse().ok()?,
                    ))
                })
                .collect();
            let pivot_rels = package
                .relationships(path)?
                .into_iter()
                .filter(|r| !r.external && r.kind("pivotTable"))
                .collect::<Vec<_>>();
            let mut unknown_pivot =
                child(xml.root(), "pivotTableParts").is_some() && pivot_rels.is_empty();
            for rel in pivot_rels {
                let range = if package.has_part(&rel.target) {
                    let part = package.part(&rel.target)?;
                    let root = part.root();
                    is(root, "pivotTableDefinition")
                        .then(|| child(root, "location"))
                        .flatten()
                        .and_then(|loc| attr(loc, "ref"))
                        .and_then(CellRange::parse)
                } else {
                    None
                };
                if let Some(range) = range {
                    locked.push(range)
                } else {
                    unknown_pivot = true
                }
            }
            sheets.push(Sheet {
                name: attr(n, "name").unwrap_or("").into(),
                hidden: attr(n, "state").is_some_and(|x| x != "visible"),
                cells,
                merged,
                widths,
                path: (*path).clone(),
                locked,
                unknown_pivot,
            });
        }
        let mut binding = Self {
            baseline: sheet::Workbook::default(),
            sheets,
            formats,
            bold_styles,
            part,
            date1904,
        };
        binding.baseline = binding.model();
        Ok(binding)
    }
    pub(crate) fn load(package: &Package) -> Result<(sheet::Workbook, Self), OpenError> {
        let binding = Self::open(package)?;
        Ok((binding.baseline.clone(), binding))
    }

    pub(crate) fn rebase(package: &Package, model: &sheet::Workbook) -> Result<Self, OpenError> {
        let mut next = Self::open(package)?;
        next.baseline = model.clone();
        Ok(next)
    }

    pub(crate) fn changes(
        &self,
        package: &Package,
        current: &sheet::Workbook,
    ) -> Result<Patches, SaveError> {
        if self.baseline.sheets().len() != current.sheets().len() {
            return Err(SaveError::Unsupported);
        }
        let mut output = BTreeMap::new();
        let mut working = package.clone();
        for (sheet, (before, after)) in self
            .baseline
            .sheets()
            .iter()
            .zip(current.sheets())
            .enumerate()
        {
            let keys = before
                .cells
                .keys()
                .chain(after.cells.keys())
                .copied()
                .collect::<BTreeSet<_>>();
            for (r, c) in keys {
                let old = before.cells.get(&(r, c));
                let new = after.cells.get(&(r, c));
                if old == new {
                    continue;
                }
                let op = if let Some(cell) = new.filter(|c| c.edited) {
                    sheet::SheetOp::SetCell {
                        sheet,
                        r,
                        c,
                        input: cell.input(),
                    }
                } else {
                    sheet::SheetOp::Restore {
                        sheet,
                        r,
                        c,
                        cell: new.cloned(),
                    }
                };
                let patches = self
                    .apply(&working, &op)
                    .map_err(|e| SaveError::Io(e.to_string()))?;
                working
                    .replace_parts(&patches)
                    .map_err(|e| SaveError::Io(e.to_string()))?;
                output.extend(patches);
            }
        }
        let mut result = Vec::new();
        for (path, bytes) in output {
            if package
                .read_part(&path)
                .map_err(|e| SaveError::Io(e.to_string()))?
                != bytes
            {
                result.push((path, bytes));
            }
        }
        Ok(result)
    }

    fn model(&self) -> sheet::Workbook {
        let rect = |r: &CellRange| (r.first.0 - 1, r.first.1 - 1, r.last.0 - 1, r.last.1 - 1);
        sheet::Workbook::from_sheets(
            self.sheets
                .iter()
                .map(|s| {
                    let cells = s
                        .cells
                        .iter()
                        .map(|(&(r, c), v)| {
                            let value = match &v.value {
                                Value::Empty => sheet::Value::Empty,
                                Value::Number(n) | Value::Date(n) => sheet::Value::Number(*n),
                                Value::Text(s) => sheet::Value::Text(s.clone()),
                                Value::Bool(b) => sheet::Value::Bool(*b),
                                Value::Error(s) => sheet::Value::Error(s.clone()),
                            };
                            (
                                (r - 1, c - 1),
                                sheet::Cell {
                                    value,
                                    formula: v.formula.clone(),
                                    display: self.display(v),
                                    bold: self
                                        .bold_styles
                                        .get(v.style as usize)
                                        .copied()
                                        .unwrap_or(false),
                                    style: 0,
                                    edited: false,
                                },
                            )
                        })
                        .collect();
                    let widths = s
                        .widths
                        .iter()
                        .filter(|&(a, b, w)| *a > 0 && *b <= 16_384 && w.is_finite() && *w >= 0.0)
                        .flat_map(|&(a, b, w)| (a..=b).map(move |c| (c - 1, w as f32)))
                        .collect();
                    let merged = s
                        .merged
                        .iter()
                        .filter_map(|r| CellRange::parse(r))
                        .map(|r| rect(&r))
                        .collect();
                    let locked = if s.unknown_pivot {
                        vec![(0, 0, 1_048_575, 16_383)]
                    } else {
                        s.locked.iter().map(rect).collect()
                    };
                    sheet::Sheet {
                        name: s.name.clone(),
                        hidden: s.hidden,
                        cells,
                        merged,
                        widths,
                        locked,
                        look: Default::default(),
                    }
                })
                .collect(),
        )
    }

    /// Pure part edits. The document journal retains exact prior XML for inverses.
    fn apply(&self, package: &Package, op: &sheet::SheetOp) -> Result<Patches, EditError> {
        let (s, r, c, input, restore) = match op {
            sheet::SheetOp::SetCell { sheet, r, c, input } => (
                *sheet,
                *r,
                *c,
                Some(input.trim_end_matches(['\n', '\r'])).filter(|x| !x.is_empty()),
                None,
            ),
            sheet::SheetOp::Restore { sheet, r, c, cell } => (*sheet, *r, *c, None, cell.as_ref()),
            // Written back in backlog item 17 (docs/slides-sheets-implementation.md).
            sheet::SheetOp::SetStyle { .. } => {
                return Err(EditError::Invalid(
                    "cell formatting cannot be saved yet".into(),
                ));
            }
        };
        if r >= 1_048_576 || c >= 16_384 || s >= self.sheets.len() {
            return Err(EditError::Missing);
        }
        self.patch_cell(package, s, r + 1, c + 1, input, restore)
            .map_err(|err| match err {
                OpenError::Damaged(s) if s.contains("locked") => EditError::Locked,
                err => EditError::Invalid(err.to_string()),
            })
    }

    fn display(&self, cell: &Cell) -> String {
        let f = self
            .formats
            .get(cell.style as usize)
            .map(String::as_str)
            .unwrap_or("General");
        match &cell.value {
            Value::Number(n) => format_number(*n, f),
            Value::Date(n) => format_date(*n, f, self.date1904),
            Value::Empty => String::new(),
            Value::Text(s) | Value::Error(s) => s.clone(),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.into(),
        }
    }
    /// Produces a transactional patch set; caller atomically applies all parts.
    fn patch_cell(
        &self,
        package: &Package,
        sheet: usize,
        row: u32,
        col: u32,
        input: Option<&str>,
        restore: Option<&sheet::Cell>,
    ) -> Result<Patches, OpenError> {
        let sheet = self
            .sheets
            .get(sheet)
            .ok_or_else(|| bad("sheet out of range"))?;
        if sheet.unknown_pivot || sheet.locked.iter().any(|x| x.contains(row, col)) {
            return Err(bad("worksheet range is locked"));
        }
        if !(1..=1_048_576).contains(&row) || !(1..=16_384).contains(&col) {
            return Err(bad("cell out of range"));
        }
        let xml = package.part(&sheet.path)?;
        let data = child(xml.root(), "sheetData").ok_or_else(|| bad("missing sheet data"))?;
        let r = data
            .children
            .iter()
            .find(|n| is(n, "row") && attr(n, "r").and_then(|x| x.parse().ok()) == Some(row));
        let cell = r.and_then(|r| {
            r.children
                .iter()
                .find(|n| is(n, "c") && attr(n, "r").and_then(parse_addr) == Some((row, col)))
        });
        if cell.is_none() && input.is_none() && restore.is_none() {
            return Ok(Vec::new());
        }
        let raw_formula = input
            .and_then(|s| s.strip_prefix('='))
            .or_else(|| restore.and_then(|x| x.formula.as_deref()));
        let all_cells = || {
            data.children
                .iter()
                .filter(|n| is(n, "row"))
                .flat_map(|r| r.children.iter().filter(|n| is(n, "c")))
        };
        let mut array = None;
        for n in all_cells() {
            if let Some(f) = child(n, "f")
                && let Some(kind @ ("shared" | "array" | "dataTable")) = attr(f, "t")
            {
                let owner = attr(n, "r").and_then(parse_addr) == Some((row, col));
                let range = attr(f, "ref")
                    .map(|r| CellRange::parse(r).ok_or_else(|| bad("malformed formula range")))
                    .transpose()?;
                if owner || range.as_ref().is_some_and(|range| range.contains(row, col)) {
                    if kind == "array"
                        && owner
                        && attr(n, "cm").is_none()
                        && attr(n, "vm").is_none()
                        && !f.text.is_empty()
                        && f.children.is_empty()
                        && raw_formula.is_some_and(|s| !s.is_empty())
                        && let Some(range) = range
                        && range.first == (row, col)
                    {
                        array = Some((f, range));
                    } else {
                        return Err(bad("grouped formula range is locked"));
                    }
                }
            }
        }
        let address = addr(row, col);
        let format = sheet
            .cells
            .get(&(row, col))
            .and_then(|c| self.formats.get(c.style as usize));
        let input_kind = if let Some(c) = restore {
            match (&c.formula, &c.value) {
                (Some(f), _) => Some(("formula", f.clone())),
                (None, sheet::Value::Number(v)) if v.is_finite() => Some(("number", v.to_string())),
                (None, sheet::Value::Number(_)) => return Err(bad("invalid numeric value")),
                (None, sheet::Value::Bool(v)) => Some(("b", u8::from(*v).to_string())),
                (None, sheet::Value::Text(v)) => Some(("inlineStr", v.clone())),
                (None, sheet::Value::Error(v)) => Some(("e", v.clone())),
                (None, sheet::Value::Empty) => None,
            }
        } else {
            input.map(|s| {
                if let Some(f) = s.strip_prefix('=').filter(|f| !f.is_empty()) {
                    ("formula", f.to_owned())
                } else if let Some(v) = s
                    .trim()
                    .replace(',', "")
                    .parse::<f64>()
                    .ok()
                    .filter(|x| x.is_finite())
                {
                    ("number", v.to_string())
                } else if format.is_some_and(|f| date_format(f))
                    && let Some(v) = date_serial(s, self.date1904)
                {
                    ("number", v.to_string())
                } else if ["TRUE", "FALSE"]
                    .iter()
                    .any(|x| x.eq_ignore_ascii_case(s.trim()))
                {
                    (
                        "b",
                        u8::from(s.trim().eq_ignore_ascii_case("true")).to_string(),
                    )
                } else {
                    ("inlineStr", s.to_owned())
                }
            })
        };
        let q = prefix(&xml, cell.or(r).unwrap_or(data));
        let mut payload = String::new();
        if let Some((kind, s)) = &input_kind {
            let s = quick_xml::escape::escape(s.as_str());
            match *kind {
                "formula" => payload = format!("<{q}f>{s}</{q}f>"),
                "number" | "b" | "e" => payload = format!("<{q}v>{s}</{q}v>"),
                _ => payload = format!("<{q}is><{q}t xml:space=\"preserve\">{s}</{q}t></{q}is>"),
            }
        }
        let ty = input_kind
            .as_ref()
            .and_then(|(k, _)| matches!(*k, "inlineStr" | "b" | "e").then_some(*k));
        let updated = if let Some((formula, range)) = array {
            let input = raw_formula.ok_or_else(|| bad("expected array formula"))?;
            let mut edits = vec![(
                element_payload(&xml, formula)?,
                quick_xml::escape::escape(input).into_owned().into_bytes(),
            )];
            for n in all_cells() {
                let p = attr(n, "r")
                    .and_then(parse_addr)
                    .ok_or_else(|| bad("bad array member address"))?;
                if !range.contains(p.0, p.1) {
                    continue;
                }
                if attr(n, "cm").is_some()
                    || attr(n, "vm").is_some()
                    || attr(n, "t").is_some_and(|t| t != "n")
                    || child(n, "is").is_some()
                    || (p != (row, col) && child(n, "f").is_some())
                {
                    return Err(bad("array member has unsupported metadata"));
                }
                if let Some(v) = child(n, "v") {
                    // Calc needs explicit cache placeholders for the array anchors
                    // and members. Clear numeric payloads, not the element itself.
                    if !v.text.is_empty() {
                        if !v.children.is_empty() {
                            return Err(bad("invalid array cache"));
                        }
                        edits.push((element_payload(&xml, v)?, Vec::new()));
                    }
                } else {
                    let end = n.range.end;
                    let raw = &xml.bytes()[n.range.clone()];
                    if raw.ends_with(b"/>") {
                        return Err(bad("unsupported empty array member"));
                    }
                    let close = raw
                        .windows(2)
                        .rposition(|b| b == b"</")
                        .ok_or_else(|| bad("missing array member closing tag"))?;
                    let at = n
                        .children
                        .iter()
                        .find(|x| is(x, "extLst"))
                        .map_or(end - raw.len() + close, |x| x.range.start);
                    let q = prefix(&xml, n);
                    edits.push((at..at, format!("<{q}v></{q}v>").into_bytes()));
                }
            }
            edits.sort_unstable_by_key(|(r, _)| r.start);
            let mut bytes = Vec::with_capacity(xml.bytes().len());
            let mut cursor = 0;
            for (range, replacement) in edits {
                bytes.extend_from_slice(
                    xml.bytes()
                        .get(cursor..range.start)
                        .ok_or_else(|| bad("overlapping array edits"))?,
                );
                bytes.extend(replacement);
                cursor = range.end;
            }
            bytes.extend_from_slice(&xml.bytes()[cursor..]);
            bytes
        } else if let Some(c) = cell {
            rewrite_cell(&xml, c, &q, ty, &payload)?
        } else {
            let ty = ty.map(|v| format!(" t=\"{v}\"")).unwrap_or_default();
            let cell = format!("<{q}c r=\"{address}\"{ty}>{payload}</{q}c>");
            if let Some(r) = r {
                insert_child(
                    &xml,
                    r,
                    &cell,
                    r.children.iter().find(|n| {
                        is(n, "c")
                            && attr(n, "r")
                                .and_then(parse_addr)
                                .is_some_and(|(_, c)| c > col)
                    }),
                )?
            } else {
                let s = format!("<{q}row r=\"{row}\">{cell}</{q}row>");
                insert_child(
                    &xml,
                    data,
                    &s,
                    data.children.iter().find(|n| {
                        is(n, "row")
                            && attr(n, "r")
                                .and_then(|x| x.parse::<u32>().ok())
                                .is_some_and(|x| x > row)
                    }),
                )?
            }
        };
        let updated = if cell.is_none() {
            // Preserve the optional dimension when it already includes the cell.
            // Expand its rectangle on sparse inserts outside the previous bounds.
            let candidate = XmlPart::parse(updated)?;
            if let Some(dimension) = child(candidate.root(), "dimension") {
                let range = attr(dimension, "ref")
                    .and_then(CellRange::parse)
                    .ok_or_else(|| bad("invalid worksheet dimension"))?;
                if range.contains(row, col) {
                    candidate.bytes().to_vec()
                } else {
                    let bounds = format!(
                        "{}:{}",
                        addr(row.min(range.first.0), col.min(range.first.1)),
                        addr(row.max(range.last.0), col.max(range.last.1))
                    );
                    replace_start_attr(&candidate, dimension, "ref", Some(&bounds))?
                }
            } else {
                candidate.bytes().to_vec()
            }
        } else {
            updated
        };
        XmlPart::parse(updated.clone())?;
        let mut patches = vec![(sheet.path.clone(), updated)];
        if input_kind.is_some_and(|(k, _)| k == "formula") {
            let xml = package.part(&self.part)?;
            let node = xml.root();
            let updated = if let Some(calc) = child(node, "calcPr") {
                replace_start_attr(&xml, calc, "fullCalcOnLoad", Some("1"))?
            } else {
                let q = prefix(&xml, node);
                let before = node.children.iter().find(|x| {
                    ns(x)
                        && matches!(
                            x.local_name.as_str(),
                            "oleSize"
                                | "customWorkbookViews"
                                | "pivotCaches"
                                | "smartTagPr"
                                | "smartTagTypes"
                                | "webPublishing"
                                | "fileRecoveryPr"
                                | "webPublishObjects"
                                | "extLst"
                        )
                });
                insert_child(
                    &xml,
                    node,
                    &format!("<{q}calcPr fullCalcOnLoad=\"1\"/>"),
                    before,
                )?
            };
            XmlPart::parse(updated.clone())?;
            patches.push((self.part.clone(), updated));
        }
        Ok(patches)
    }
}

fn element_payload(xml: &XmlPart, node: &XmlNode) -> Result<std::ops::Range<usize>, OpenError> {
    let bytes = &xml.bytes()[node.range.clone()];
    let e = start_end(bytes)?;
    let close = bytes
        .windows(2)
        .rposition(|x| x == b"</")
        .filter(|&x| x > e)
        .ok_or_else(|| bad("missing XML closing tag"))?;
    Ok(node.range.start + e + 1..node.range.start + close)
}
fn start_end(b: &[u8]) -> Result<usize, OpenError> {
    let mut quote = 0;
    for (i, &x) in b.iter().enumerate() {
        if quote != 0 {
            if quote == x {
                quote = 0
            }
        } else if matches!(x, b'\'' | b'"') {
            quote = x
        } else if x == b'>' {
            return Ok(i);
        }
    }
    Err(bad("unterminated start tag"))
}
fn prefix(xml: &XmlPart, n: &XmlNode) -> String {
    let b = &xml.bytes()[n.range.start + 1..n.range.end];
    let end = b
        .iter()
        .position(|x| x.is_ascii_whitespace() || matches!(*x, b'/' | b'>'))
        .unwrap_or(b.len());
    let q = std::str::from_utf8(&b[..end]).unwrap_or_default();
    q.split_once(':')
        .map(|(p, _)| format!("{p}:"))
        .unwrap_or_default()
}
fn splice(xml: &XmlPart, r: std::ops::Range<usize>, s: &[u8]) -> Vec<u8> {
    let mut x = xml.bytes().to_vec();
    x.splice(r, s.iter().copied());
    x
}
fn start_attr(b: &[u8], key: &str, val: Option<&str>) -> Result<Vec<u8>, OpenError> {
    let e = start_end(b)?;
    let tag = &b[1..e];
    let name = tag
        .iter()
        .position(|x| x.is_ascii_whitespace() || *x == b'/')
        .unwrap_or(tag.len());
    let mut i = 1 + name;
    let mut found = None;
    while i < e {
        let start = i;
        while i < e && b[i].is_ascii_whitespace() {
            i += 1
        }
        if i == e || b[i] == b'/' {
            break;
        }
        let k = i;
        while i < e && !b[i].is_ascii_whitespace() && b[i] != b'=' {
            i += 1
        }
        let kend = i;
        while i < e && b[i].is_ascii_whitespace() {
            i += 1
        }
        if i == e || b[i] != b'=' {
            return Err(bad("invalid xml attribute"));
        }
        i += 1;
        while i < e && b[i].is_ascii_whitespace() {
            i += 1
        }
        if i == e || !matches!(b[i], b'\'' | b'"') {
            return Err(bad("invalid xml quote"));
        }
        let quote = b[i];
        i += 1;
        while i < e && b[i] != quote {
            i += 1
        }
        if i == e {
            return Err(bad("unterminated attribute"));
        }
        i += 1;
        if &b[k..kend] == key.as_bytes() {
            found = Some(start..i);
            break;
        }
    }
    let at = if e > 0 && b[e - 1] == b'/' { e - 1 } else { e };
    let mut out = b[..=e].to_vec();
    let payload = val.map(|v| format!(" {key}=\"{v}\"")).unwrap_or_default();
    out.splice(found.unwrap_or(at..at), payload.bytes());
    Ok(out)
}
fn replace_start_attr(
    xml: &XmlPart,
    n: &XmlNode,
    key: &str,
    val: Option<&str>,
) -> Result<Vec<u8>, OpenError> {
    let bytes = &xml.bytes()[n.range.clone()];
    let end = start_end(bytes)?;
    Ok(splice(
        xml,
        n.range.start..n.range.start + end + 1,
        &start_attr(bytes, key, val)?,
    ))
}
fn insert_child(
    xml: &XmlPart,
    n: &XmlNode,
    s: &str,
    before: Option<&XmlNode>,
) -> Result<Vec<u8>, OpenError> {
    if let Some(c) = before {
        return Ok(splice(xml, c.range.start..c.range.start, s.as_bytes()));
    }
    let bytes = &xml.bytes()[n.range.clone()];
    if bytes.ends_with(b"/>") {
        let q = prefix(xml, n);
        let text = format!(">{s}</{q}{}>", n.local_name);
        return Ok(splice(xml, n.range.end - 2..n.range.end, text.as_bytes()));
    }
    let end = bytes
        .windows(2)
        .rposition(|x| x == b"</")
        .ok_or_else(|| bad("missing closing tag"))?
        + n.range.start;
    Ok(splice(xml, end..end, s.as_bytes()))
}
fn rewrite_cell(
    xml: &XmlPart,
    n: &XmlNode,
    q: &str,
    ty: Option<&str>,
    payload: &str,
) -> Result<Vec<u8>, OpenError> {
    let bytes = &xml.bytes()[n.range.clone()];
    let e = start_end(bytes)?;
    let mut out = start_attr(bytes, "t", ty)?;
    if out.ends_with(b"/>") {
        if payload.is_empty() {
            return Ok(splice(xml, n.range.clone(), &out));
        }
        out.truncate(out.len() - 2);
        out.push(b'>');
        out.extend(payload.bytes());
        out.extend(format!("</{q}c>").bytes());
    } else {
        let mut cursor = n.range.start + e + 1;
        let mut pending = Some(payload);
        for child in &n.children {
            if is(child, "f") || is(child, "v") || is(child, "is") {
                out.extend_from_slice(&xml.bytes()[cursor..child.range.start]);
                if let Some(s) = pending.take() {
                    out.extend(s.bytes());
                }
                cursor = child.range.end;
            } else if let Some(s) = pending.take() {
                out.extend_from_slice(&xml.bytes()[cursor..child.range.start]);
                out.extend(s.bytes());
                cursor = child.range.start;
            }
        }
        if let Some(s) = pending.take() {
            out.extend(s.bytes());
        }
        out.extend_from_slice(&xml.bytes()[cursor..n.range.end]);
    }
    Ok(splice(xml, n.range.clone(), &out))
}
fn read_bold(root: &XmlNode) -> Vec<bool> {
    let fonts: Vec<bool> = child(root, "fonts")
        .into_iter()
        .flat_map(|x| &x.children)
        .filter(|x| is(x, "font"))
        .map(|x| {
            child(x, "b").is_some_and(|b| !matches!(attr(b, "val"), Some("0" | "false" | "off")))
        })
        .collect();
    child(root, "cellXfs")
        .into_iter()
        .flat_map(|n| &n.children)
        .filter(|n| is(n, "xf"))
        .map(|xf| {
            attr(xf, "fontId")
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| fonts.get(i))
                .copied()
                .unwrap_or(false)
        })
        .collect()
}

fn read_formats(root: &XmlNode) -> Vec<String> {
    let custom: HashMap<u32, &str> = child(root, "numFmts")
        .into_iter()
        .flat_map(|n| &n.children)
        .filter(|n| is(n, "numFmt"))
        .filter_map(|n| Some((attr(n, "numFmtId")?.parse().ok()?, attr(n, "formatCode")?)))
        .collect();
    child(root, "cellXfs")
        .into_iter()
        .flat_map(|n| &n.children)
        .filter(|n| is(n, "xf"))
        .map(|n| {
            let i = attr(n, "numFmtId")
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
            custom
                .get(&i)
                .copied()
                .unwrap_or(match i {
                    1 => "0",
                    2 => "0.00",
                    3 => "#,##0",
                    4 => "#,##0.00",
                    9 => "0%",
                    10 => "0.00%",
                    14 => "yyyy-mm-dd",
                    15 => "dd-mmm-yy",
                    16 => "dd-mmm",
                    17 => "mmm-yy",
                    18 => "h:mm AM/PM",
                    19 => "h:mm:ss AM/PM",
                    20 => "h:mm",
                    21 => "h:mm:ss",
                    22 => "yyyy-mm-dd h:mm",
                    _ => "General",
                })
                .into()
        })
        .collect()
}
fn date_format(f: &str) -> bool {
    matches!(
        f.to_ascii_lowercase().as_str(),
        "yyyy-mm-dd"
            | "dd-mmm-yy"
            | "dd-mmm"
            | "mmm-yy"
            | "h:mm am/pm"
            | "h:mm:ss am/pm"
            | "h:mm"
            | "h:mm:ss"
            | "yyyy-mm-dd h:mm"
            | "yyyy\"年\"m\"月\"d\"日\""
    )
}
fn format_number(n: f64, f: &str) -> String {
    let (pattern, pre, suffix) = if f.starts_with(['¥', '￥', '€', '£', '$']) {
        let i = f.chars().next().unwrap().len_utf8();
        (&f[i..], &f[..i], "")
    } else if f.ends_with(['¥', '￥', '€', '£', '$']) {
        let i = f.len() - f.chars().next_back().unwrap().len_utf8();
        (&f[..i], "", &f[i..])
    } else {
        (f, "", "")
    };
    let (digits, group, percent) = match pattern {
        "0" => (0, false, false),
        "0.00" => (2, false, false),
        "#,##0" => (0, true, false),
        "#,##0.00" => (2, true, false),
        "0%" => (0, false, true),
        "0.00%" => (2, false, true),
        _ => return n.to_string(),
    };
    let number = if percent { n * 100.0 } else { n };
    let text = format!("{number:.digits$}");
    let s = if group {
        let (head, tail) = text
            .split_once('.')
            .map(|(h, t)| (h, Some(t)))
            .unwrap_or((&text, None));
        let (sign, head) = head
            .strip_prefix('-')
            .map(|h| ("-", h))
            .unwrap_or(("", head));
        let mut out = sign.to_string();
        for (i, c) in head.chars().enumerate() {
            if i > 0 && (head.len() - i).is_multiple_of(3) {
                out.push(',')
            }
            out.push(c)
        }
        if let Some(t) = tail {
            out.push('.');
            out.push_str(t)
        }
        out
    } else {
        text
    };
    format!("{pre}{s}{}{suffix}", if percent { "%" } else { "" })
}
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    (y + i64::from(m <= 2), m, d)
}
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}
fn date_serial(s: &str, date1904: bool) -> Option<f64> {
    let cleaned = s.replace(['年', '月'], "-").replace('日', "");
    let mut parts = cleaned.split('-');
    let y = parts.next()?.parse().ok()?;
    let m = parts.next()?.parse().ok()?;
    let d = parts.next()?.parse().ok()?;
    if parts.next().is_some()
        || !(1..=9999).contains(&y)
        || !(1..=12).contains(&m)
        || !(1..=31).contains(&d)
    {
        return None;
    }
    if !date1904 && (y, m, d) == (1900, 2, 29) {
        return Some(60.0);
    }
    let z = days_from_civil(y, m, d);
    if civil_from_days(z) != (y, m, d) {
        return None;
    }
    Some(
        (z + if date1904 {
            24107
        } else if (y, m, d) >= (1900, 3, 1) {
            25569
        } else {
            25568
        }) as f64,
    )
}
fn format_date(n: f64, f: &str, date1904: bool) -> String {
    if !n.is_finite() || !(0.0..=2_958_465.0).contains(&n) {
        return n.to_string();
    }
    let day = n.floor() as i64;
    let (y, m, d) = if !date1904 && day == 60 {
        (1900, 2, 29)
    } else {
        civil_from_days(
            day - if date1904 {
                24107
            } else if day < 60 {
                25568
            } else {
                25569
            },
        )
    };
    let t = ((n.fract() * 86400.0).round() as i64).rem_euclid(86400);
    let h = t / 3600;
    let min = t / 60 % 60;
    let sec = t % 60;
    let mm = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(m - 1) as usize];
    let am = if h < 12 { "AM" } else { "PM" };
    let h12 = (h + 11) % 12 + 1;
    match f {
        "yyyy\"年\"m\"月\"d\"日\"" => format!("{y:04}年{m}月{d}日"),
        "dd-mmm-yy" => format!("{d:02}-{mm}-{:02}", y % 100),
        "dd-mmm" => format!("{d:02}-{mm}"),
        "mmm-yy" => format!("{mm}-{:02}", y % 100),
        "h:mm AM/PM" => format!("{h12}:{min:02} {am}"),
        "h:mm:ss AM/PM" => format!("{h12}:{min:02}:{sec:02} {am}"),
        "h:mm" => format!("{h}:{min:02}"),
        "h:mm:ss" => format!("{h}:{min:02}:{sec:02}"),
        "yyyy-mm-dd h:mm" => format!("{y:04}-{m:02}-{d:02} {h}:{min:02}"),
        _ => format!("{y:04}-{m:02}-{d:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn date_systems_and_builtin_formats() {
        assert_eq!(date_serial("1900-02-29", false), Some(60.0));
        assert_eq!(date_serial("1900-03-01", false), Some(61.0));
        assert_eq!(date_serial("1904-01-01", true), Some(0.0));
        assert_eq!(date_serial("2025-02-29", true), None);
        assert_eq!(
            format_date(60.0, "yyyy\"年\"m\"月\"d\"日\"", false),
            "1900年2月29日"
        );
        assert_eq!(format_date(0.625, "h:mm AM/PM", true), "3:00 PM");
        assert_eq!(format_number(1234.5, "¥#,##0.00"), "¥1,234.50");
        assert_eq!(format_number(0.125, "0.00%"), "12.50%");
        assert_eq!(format_number(-1234.0, "#,##0"), "-1,234");
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use std::{fs::File, io::Write};
    const PREFIX: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    #[test]
    fn upstream_ops_keep_undone_bytes_while_saving_other_cells() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("source.xlsx");
        let mut z = zip::ZipWriter::new(File::create(&p).unwrap());
        for (n, s) in [
            ("[Content_Types].xml", "<Types/>".into()),
            (
                "xl/workbook.xml",
                format!(
                    "<workbook xmlns='{NS}' xmlns:r='{REL}'><sheets><sheet name='预算' r:id='w'/></sheets></workbook>"
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                format!(
                    "<Relationships xmlns='{PREFIX}'><Relationship Id='w' Type='{REL}/worksheet' Target='worksheets/sheet1.xml'/></Relationships>"
                ),
            ),
            (
                "xl/worksheets/sheet1.xml",
                format!(
                    "<worksheet xmlns='{NS}' xmlns:u='urn:other'><dimension ref='A1:D3'/><sheetData><row r='1'><c r='A1' t='n'><v>3.00</v><u:x/></c><c r='D1'><f t='array' ref='D1:D3'>A1:A3*2</f><v>6</v></c></row><row r='2'><c r='D2' t='n'><v>9</v></c></row></sheetData></worksheet>"
                ),
            ),
        ] {
            z.start_file(n, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(s.as_bytes()).unwrap();
        }
        z.finish().unwrap();
        let pkg = Package::open(&p).unwrap();
        let (mut model, binding) = SheetBinding::load(&pkg).unwrap();
        assert_eq!(
            model.sheets()[0].cells[&(0, 0)].value,
            sheet::Value::Number(3.0)
        );
        let undo = model
            .apply(sheet::SheetOp::SetCell {
                sheet: 0,
                r: 0,
                c: 0,
                input: "changed".into(),
            })
            .unwrap();
        model.apply(undo).unwrap();
        assert!(binding.changes(&pkg, &model).unwrap().is_empty());
        model
            .apply(sheet::SheetOp::SetCell {
                sheet: 0,
                r: 0,
                c: 3,
                input: "=A1:A3*3".into(),
            })
            .unwrap();
        model
            .apply(sheet::SheetOp::SetCell {
                sheet: 0,
                r: 0,
                c: 1,
                input: "TRUE".into(),
            })
            .unwrap();
        let updates = binding.changes(&pkg, &model).unwrap();
        let part = updates
            .iter()
            .find(|x| x.0 == "xl/worksheets/sheet1.xml")
            .unwrap();
        let s = std::str::from_utf8(&part.1).unwrap();
        assert!(s.contains("<c r='A1' t='n'><v>3.00</v><u:x/></c>"));
        assert!(s.contains("<c r=\"B1\" t=\"b\"><v>1</v></c>"));
        assert!(s.contains("<f t='array' ref='D1:D3'>A1:A3*3</f><v></v>"));
        assert!(s.contains("<c r='D2' t='n'><v></v></c>"));
        assert!(updates.iter().any(|x| {
            x.0 == MAIN
                && std::str::from_utf8(&x.1)
                    .unwrap()
                    .contains("fullCalcOnLoad=\"1\"")
        }));
        let out = dir.path().join("output.xlsx");
        pkg.save_replaced(&out, &updates).unwrap();
        let (after, _) = SheetBinding::load(&Package::open(&out).unwrap()).unwrap();
        assert_eq!(
            after.sheets()[0].cells[&(0, 1)].value,
            sheet::Value::Bool(true)
        );
        assert_eq!(after.sheets()[0].cells[&(1, 3)].value, sheet::Value::Empty);
    }
}
