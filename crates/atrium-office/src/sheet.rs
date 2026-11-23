//! A workbook as sheets of cells.

use std::collections::BTreeMap;

use crate::word::EditError;

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Empty,
    Number(f64),
    Text(String),
    Bool(bool),
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub value: Value,
    /// Without the leading `=`.
    pub formula: Option<String>,
    /// The value as the cell's number format shows it.
    pub display: String,
    pub bold: bool,
    /// Index into `Workbook::styles`; 0 is the default style.
    pub style: u32,
    /// Typed here since the file was opened; its formula is recalculated when
    /// the file next opens in a spreadsheet app.
    pub edited: bool,
}

impl Cell {
    /// What editing the cell starts from: its formula, or its value.
    pub fn input(&self) -> String {
        match &self.formula {
            Some(f) => format!("={f}"),
            None => match &self.value {
                Value::Number(_) | Value::Text(_) | Value::Bool(_) => self.display.clone(),
                _ => String::new(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub name: String,
    pub hidden: bool,
    /// By (row, column), from 0.
    pub cells: BTreeMap<(u32, u32), Cell>,
    /// Ranges as (first row, first column, last row, last column).
    pub merged: Vec<(u32, u32, u32, u32)>,
    /// Widths in characters, by column, where the file sets them.
    pub widths: BTreeMap<u32, f32>,
    /// Pivot table areas and the like: shown, not edited.
    pub locked: Vec<(u32, u32, u32, u32)>,
    pub look: crate::look::SheetLook,
}

impl Sheet {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            hidden: false,
            cells: BTreeMap::new(),
            merged: Vec::new(),
            widths: BTreeMap::new(),
            locked: Vec::new(),
            look: Default::default(),
        }
    }

    /// The style index of a cell: its own, else its row's, else its column's.
    pub fn style_index(&self, r: u32, c: u32) -> u32 {
        self.cells
            .get(&(r, c))
            .map(|cell| cell.style)
            .filter(|s| *s != 0)
            .or_else(|| self.look.row_styles.get(&r).copied())
            .or_else(|| self.look.column_styles.get(&c).copied())
            .unwrap_or(0)
    }

    /// Rows and columns in use.
    pub fn extent(&self) -> (u32, u32) {
        self.cells
            .keys()
            .fold((0, 0), |(r, c), (cr, cc)| (r.max(cr + 1), c.max(cc + 1)))
    }

    fn is_locked(&self, r: u32, c: u32) -> bool {
        self.locked
            .iter()
            .any(|(r0, c0, r1, c1)| (*r0..=*r1).contains(&r) && (*c0..=*c1).contains(&c))
    }
}

/// A1-style name of a cell.
pub fn a1(r: u32, c: u32) -> String {
    let mut col = String::new();
    let mut n = c + 1;
    while n > 0 {
        let rem = (n - 1) % 26;
        col.insert(0, (b'A' + rem as u8) as char);
        n = (n - 1) / 26;
    }
    format!("{col}{}", r + 1)
}

#[derive(Clone, Debug, PartialEq)]
pub enum SheetOp {
    /// `input` as typed: `=…` is a formula, a number is a number, anything else text.
    SetCell {
        sheet: usize,
        r: u32,
        c: u32,
        input: String,
    },
    /// Gives a cell another style from `Workbook::styles`, keeping its value.
    SetStyle {
        sheet: usize,
        r: u32,
        c: u32,
        style: u32,
    },
    /// Formats a rectangle in one undo step.
    SetStyles {
        sheet: usize,
        from: (u32, u32),
        to: (u32, u32),
        style: u32,
    },
    /// Sets a column's width in points; `RestoreSheet` undoes it.
    SetColumnWidth {
        sheet: usize,
        column: u32,
        width: f32,
    },
    /// Sets a row's height in points; `RestoreSheet` undoes it.
    SetRowHeight { sheet: usize, row: u32, height: f32 },
    /// Freezes `rows` rows and `cols` columns.
    SetFrozen { sheet: usize, rows: u32, cols: u32 },
    /// Puts a cell back exactly; the inverse of an edit.
    Restore {
        sheet: usize,
        r: u32,
        c: u32,
        cell: Option<Cell>,
    },
    /// Puts a rectangle back; the inverse of `SetStyles`.
    RestoreRange {
        sheet: usize,
        from: (u32, u32),
        to: (u32, u32),
        cells: Vec<Option<Cell>>,
    },
    /// Puts a sheet's layout back; the inverse of the size and pane edits.
    RestoreSheet {
        sheet: usize,
        look: Box<crate::look::SheetLook>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Workbook {
    pub(crate) sheets: Vec<Sheet>,
    /// Each distinct cell style once; index 0 is the default.
    pub(crate) styles: Vec<crate::look::CellStyle>,
    /// The workbook's date system: 1904 (Mac) instead of 1900.
    pub(crate) date1904: bool,
}

/// How a cell shows once its style and conditional formats apply.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub style: crate::look::CellStyle,
    /// The value as its number format writes it.
    pub text: String,
    /// A data bar: how much of the cell it fills, from 0 to 1, and its colour.
    pub bar: Option<(f32, crate::look::Rgb)>,
}

/// The cell `input` makes, keeping `old`'s style.
pub fn parse_input(input: &str, old: Option<&Cell>) -> Option<Cell> {
    let bold = old.is_some_and(|c| c.bold);
    let style = old.map(|c| c.style).unwrap_or(0);
    let input = input.trim_end_matches(['\n', '\r']);
    if input.is_empty() {
        return None;
    }
    let (value, formula) = if let Some(f) = input.strip_prefix('=') {
        (Value::Empty, Some(f.to_string()))
    } else if let Ok(n) = input.trim().replace(',', "").parse::<f64>() {
        (Value::Number(n), None)
    } else if matches!(input.trim().to_ascii_uppercase().as_str(), "TRUE" | "FALSE") {
        (Value::Bool(input.trim().eq_ignore_ascii_case("true")), None)
    } else {
        (Value::Text(input.to_string()), None)
    };
    Some(Cell {
        value,
        formula,
        display: input.to_string(),
        bold,
        style,
        edited: true,
    })
}

impl Workbook {
    pub fn from_sheets(sheets: Vec<Sheet>) -> Self {
        Self {
            sheets,
            styles: vec![crate::look::CellStyle::default()],
            date1904: false,
        }
    }

    /// Sets the workbook's date system; the loader calls this after `from_sheets`.
    pub fn with_date1904(mut self, date1904: bool) -> Self {
        self.date1904 = date1904;
        self
    }

    pub fn date1904(&self) -> bool {
        self.date1904
    }

    /// Sets the workbook's styles; the loader calls this after `from_sheets`.
    pub fn with_styles(mut self, styles: Vec<crate::look::CellStyle>) -> Self {
        if !styles.is_empty() {
            self.styles = styles;
        }
        self
    }

    pub fn styles(&self) -> &[crate::look::CellStyle] {
        &self.styles
    }

    /// The index of `style`, added if the workbook does not have it yet.
    pub fn add_style(&mut self, style: crate::look::CellStyle) -> u32 {
        if let Some(i) = self.styles.iter().position(|s| *s == style) {
            return i as u32;
        }
        self.styles.push(style);
        (self.styles.len() - 1) as u32
    }

    /// How a cell shows: its style, number format and conditional formats.
    pub fn shown(&self, sheet: usize, r: u32, c: u32) -> Shown {
        let Some(s) = self.sheets.get(sheet) else {
            return Shown {
                style: Default::default(),
                text: String::new(),
                bar: None,
            };
        };
        let mut style = self
            .styles
            .get(s.style_index(r, c) as usize)
            .cloned()
            .unwrap_or_default();
        let base_format = style.number_format.clone();
        let cell = s.cells.get(&(r, c));
        let formatted = cell
            .map(|c| crate::numfmt::format_value(&c.value, &style.number_format, self.date1904))
            .unwrap_or_default();
        if let Some(color) = formatted.color {
            style.font.color = Some(color);
        }
        let mut text = match cell {
            // Formulas show the result the file keeps until Excel recalculates.
            Some(c) if c.formula.is_some() && !c.edited => c.display.clone(),
            Some(_) => formatted.text.clone(),
            None => String::new(),
        };
        let mut bar = None;
        let mut rules: Vec<(usize, &crate::look::CondRule)> = s
            .look
            .conditional
            .iter()
            .enumerate()
            .filter(|(_, rule)| contains(rule.range, r, c))
            .collect();
        rules.sort_by_key(|(i, rule)| (rule.priority, *i));
        for (_, rule) in rules {
            if !rule_applies(s, rule, r, c, cell) {
                continue;
            }
            if let Some(dxf) = &rule.style {
                dxf.merge_into(&mut style);
            }
            match &rule.kind {
                crate::look::CondKind::ColorScale(colors) => {
                    if let Some(color) = scale_color(s, rule.range, cell, colors) {
                        style.fill = Some(color);
                    }
                }
                crate::look::CondKind::DataBar(color) => {
                    if let Some(t) = bar_fraction(s, rule.range, cell) {
                        bar = Some((t, *color));
                    }
                }
                _ => {}
            }
            if rule.stop_if_true {
                break;
            }
        }
        if let Some(c) = cell
            && !(c.formula.is_some() && !c.edited)
            && style.number_format != base_format
        {
            let formatted =
                crate::numfmt::format_value(&c.value, &style.number_format, self.date1904);
            text = formatted.text;
            if let Some(color) = formatted.color {
                style.font.color = Some(color);
            }
        }
        Shown { style, text, bar }
    }

    pub fn sheets(&self) -> &[Sheet] {
        &self.sheets
    }

    pub fn apply(&mut self, op: SheetOp) -> Result<SheetOp, EditError> {
        if let SheetOp::RestoreSheet { sheet, look } = &op {
            let s = self.sheets.get_mut(*sheet).ok_or(EditError::Missing)?;
            let old = std::mem::replace(&mut s.look, (**look).clone());
            return Ok(SheetOp::RestoreSheet {
                sheet: *sheet,
                look: Box::new(old),
            });
        }
        if let SheetOp::SetColumnWidth {
            sheet,
            column,
            width,
        } = &op
        {
            if !width.is_finite() || *width < 0.0 || *column >= 16_384 {
                return Err(EditError::Invalid("invalid column width".into()));
            }
            let s = self.sheets.get_mut(*sheet).ok_or(EditError::Missing)?;
            let old = Box::new(s.look.clone());
            s.look.columns.insert(*column, *width);
            return Ok(SheetOp::RestoreSheet {
                sheet: *sheet,
                look: old,
            });
        }
        if let SheetOp::SetRowHeight { sheet, row, height } = &op {
            if !height.is_finite() || *height < 0.0 || *row >= 1_048_576 {
                return Err(EditError::Invalid("invalid row height".into()));
            }
            let s = self.sheets.get_mut(*sheet).ok_or(EditError::Missing)?;
            let old = Box::new(s.look.clone());
            s.look.rows.insert(*row, *height);
            return Ok(SheetOp::RestoreSheet {
                sheet: *sheet,
                look: old,
            });
        }
        if let SheetOp::SetFrozen { sheet, rows, cols } = &op {
            if *rows >= 1_048_576 || *cols >= 16_384 {
                return Err(EditError::Invalid("invalid frozen pane".into()));
            }
            let s = self.sheets.get_mut(*sheet).ok_or(EditError::Missing)?;
            let old = Box::new(s.look.clone());
            s.look.frozen = (*rows, *cols);
            return Ok(SheetOp::RestoreSheet {
                sheet: *sheet,
                look: old,
            });
        }
        if let SheetOp::RestoreRange {
            sheet,
            from,
            to,
            cells,
        } = &op
        {
            let s = self.sheets.get_mut(*sheet).ok_or(EditError::Missing)?;
            if from.0 > to.0 || from.1 > to.1 {
                return Err(EditError::Invalid("reversed range".into()));
            }
            if cells.len() != range_len(*from, *to) {
                return Err(EditError::Invalid("range size changed".into()));
            }
            let mut old = Vec::with_capacity(cells.len());
            let mut i = 0;
            for r in from.0..=to.0 {
                for c in from.1..=to.1 {
                    if s.is_locked(r, c) {
                        return Err(EditError::Locked);
                    }
                    old.push(s.cells.get(&(r, c)).cloned());
                    match &cells[i] {
                        Some(cell) => {
                            s.cells.insert((r, c), cell.clone());
                        }
                        None => {
                            s.cells.remove(&(r, c));
                        }
                    }
                    i += 1;
                }
            }
            return Ok(SheetOp::RestoreRange {
                sheet: *sheet,
                from: *from,
                to: *to,
                cells: old,
            });
        }
        if let SheetOp::SetStyles {
            sheet,
            from,
            to,
            style,
        } = &op
        {
            if *style as usize >= self.styles.len() {
                return Err(EditError::Invalid("no such style".into()));
            }
            if from.0 > to.0 || from.1 > to.1 {
                return Err(EditError::Invalid("reversed range".into()));
            }
            let s = self.sheets.get_mut(*sheet).ok_or(EditError::Missing)?;
            let mut old = Vec::with_capacity(range_len(*from, *to));
            for r in from.0..=to.0 {
                for c in from.1..=to.1 {
                    if s.is_locked(r, c) {
                        return Err(EditError::Locked);
                    }
                    old.push(s.cells.get(&(r, c)).cloned());
                }
            }
            let mut i = 0;
            for r in from.0..=to.0 {
                for c in from.1..=to.1 {
                    let mut cell = s.cells.get(&(r, c)).cloned().unwrap_or(Cell {
                        value: Value::Empty,
                        formula: None,
                        display: String::new(),
                        bold: false,
                        style: 0,
                        edited: false,
                    });
                    cell.style = *style;
                    s.cells.insert((r, c), cell);
                    i += 1;
                }
            }
            let _ = i;
            return Ok(SheetOp::RestoreRange {
                sheet: *sheet,
                from: *from,
                to: *to,
                cells: old,
            });
        }
        let (sheet, r, c, new) = match op {
            SheetOp::SetCell { sheet, r, c, input } => {
                let old = self
                    .sheets
                    .get(sheet)
                    .ok_or(EditError::Missing)?
                    .cells
                    .get(&(r, c));
                (sheet, r, c, parse_input(&input, old))
            }
            SheetOp::Restore { sheet, r, c, cell } => (sheet, r, c, cell),
            SheetOp::SetStyle { sheet, r, c, style } => {
                if style as usize >= self.styles.len() {
                    return Err(EditError::Invalid("no such style".into()));
                }
                let s = self.sheets.get(sheet).ok_or(EditError::Missing)?;
                let mut cell = s.cells.get(&(r, c)).cloned().unwrap_or(Cell {
                    value: Value::Empty,
                    formula: None,
                    display: String::new(),
                    bold: false,
                    style: 0,
                    edited: false,
                });
                cell.style = style;
                (sheet, r, c, Some(cell))
            }
            _ => return Err(EditError::Invalid("unsupported sheet edit".into())),
        };
        let s = self.sheets.get_mut(sheet).ok_or(EditError::Missing)?;
        if s.is_locked(r, c) {
            return Err(EditError::Locked);
        }
        let old = match new {
            Some(cell) => s.cells.insert((r, c), cell),
            None => s.cells.remove(&(r, c)),
        };
        Ok(SheetOp::Restore {
            sheet,
            r,
            c,
            cell: old,
        })
    }
}

fn range_len(from: (u32, u32), to: (u32, u32)) -> usize {
    let rows = (to.0 - from.0 + 1) as usize;
    let cols = (to.1 - from.1 + 1) as usize;
    rows.saturating_mul(cols)
}

fn contains(range: (u32, u32, u32, u32), r: u32, c: u32) -> bool {
    (range.0..=range.2).contains(&r) && (range.1..=range.3).contains(&c)
}

fn number(cell: Option<&Cell>) -> Option<f64> {
    match cell?.value {
        Value::Number(n) => Some(n),
        Value::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn text(cell: Option<&Cell>) -> String {
    match cell.map(|c| &c.value) {
        Some(Value::Text(t)) | Some(Value::Error(t)) => t.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.into(),
        _ => String::new(),
    }
}

fn values(s: &Sheet, range: (u32, u32, u32, u32)) -> Vec<f64> {
    s.cells
        .iter()
        .filter(|((r, c), _)| contains(range, *r, *c))
        .filter_map(|(_, cell)| number(Some(cell)))
        .collect()
}

fn rule_applies(
    s: &Sheet,
    rule: &crate::look::CondRule,
    _r: u32,
    _c: u32,
    cell: Option<&Cell>,
) -> bool {
    use crate::look::{Compare, CondKind};
    let n = number(cell);
    match &rule.kind {
        CondKind::CellIs(compare) => n.is_some_and(|v| match *compare {
            Compare::Between(a, b) => v >= a && v <= b,
            Compare::NotBetween(a, b) => v < a || v > b,
            Compare::Equal(a) => (v - a).abs() < 1e-12,
            Compare::NotEqual(a) => (v - a).abs() >= 1e-12,
            Compare::Greater(a) => v > a,
            Compare::Less(a) => v < a,
            Compare::AtLeast(a) => v >= a,
            Compare::AtMost(a) => v <= a,
        }),
        CondKind::Contains(needle) => {
            let hay = text(cell).to_lowercase();
            hay.contains(&needle.to_lowercase())
        }
        CondKind::Top { n, bottom, percent } => {
            let Some(v) = n_number(cell) else {
                return false;
            };
            let mut all = values(s, rule.range);
            if all.is_empty() {
                return false;
            }
            all.sort_by(|a, b| a.total_cmp(b));
            let count = if *percent {
                ((all.len() as f64 * f64::from(*n) / 100.0).ceil() as usize).max(1)
            } else {
                (*n as usize).max(1)
            };
            if *bottom {
                all.into_iter().take(count).any(|x| (x - v).abs() < 1e-12)
            } else {
                all.into_iter()
                    .rev()
                    .take(count)
                    .any(|x| (x - v).abs() < 1e-12)
            }
        }
        CondKind::AboveAverage { below } => {
            let Some(v) = n_number(cell) else {
                return false;
            };
            let all = values(s, rule.range);
            if all.is_empty() {
                return false;
            }
            let avg = all.iter().sum::<f64>() / all.len() as f64;
            if *below { v < avg } else { v > avg }
        }
        CondKind::Duplicate { unique } => {
            let key = text(cell);
            let count = s
                .cells
                .iter()
                .filter(|((rr, cc), _)| contains(rule.range, *rr, *cc))
                .filter(|(_, other)| text(Some(other)) == key)
                .count();
            if *unique { count == 1 } else { count > 1 }
        }
        CondKind::ColorScale(_) | CondKind::DataBar(_) => n.is_some(),
        CondKind::Other(_) => false,
    }
}

fn n_number(cell: Option<&Cell>) -> Option<f64> {
    number(cell)
}

fn scale_color(
    s: &Sheet,
    range: (u32, u32, u32, u32),
    cell: Option<&Cell>,
    colors: &[crate::look::Rgb],
) -> Option<crate::look::Rgb> {
    let v = number(cell)?;
    let all = values(s, range);
    if all.is_empty() || colors.is_empty() {
        return None;
    }
    let min = all.iter().copied().fold(f64::INFINITY, f64::min);
    let max = all.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let t = if (max - min).abs() < 1e-12 {
        0.0
    } else {
        ((v - min) / (max - min)).clamp(0.0, 1.0)
    };
    match colors.len() {
        1 => Some(colors[0]),
        2 => Some(mix(colors[0], colors[1], t)),
        _ => {
            if t <= 0.5 {
                Some(mix(colors[0], colors[1], t * 2.0))
            } else {
                Some(mix(colors[1], colors[2], (t - 0.5) * 2.0))
            }
        }
    }
}

fn bar_fraction(s: &Sheet, range: (u32, u32, u32, u32), cell: Option<&Cell>) -> Option<f32> {
    let v = number(cell)?;
    let all = values(s, range);
    if all.is_empty() {
        return None;
    }
    let min = all.iter().copied().fold(f64::INFINITY, f64::min);
    let max = all.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if (max - min).abs() < 1e-12 {
        return Some(0.0);
    }
    Some((((v - min) / (max - min)).clamp(0.0, 1.0)) as f32)
}

fn mix(a: crate::look::Rgb, b: crate::look::Rgb, t: f64) -> crate::look::Rgb {
    let ch = |shift: u32| {
        let x = ((a.0 >> shift) & 0xFF) as f64;
        let y = ((b.0 >> shift) & 0xFF) as f64;
        (x + (y - x) * t).round() as u32
    };
    crate::look::Rgb((ch(16) << 16) | (ch(8) << 8) | ch(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_are_named_like_a_spreadsheet() {
        assert_eq!(a1(0, 0), "A1");
        assert_eq!(a1(6, 1), "B7");
        assert_eq!(a1(0, 26), "AA1");
    }

    #[test]
    fn typing_makes_the_right_kind_and_undoes() {
        let mut wb = Workbook::from_sheets(vec![Sheet::new("预算")]);
        let undo = wb
            .apply(SheetOp::SetCell {
                sheet: 0,
                r: 0,
                c: 0,
                input: "=SUM(B1:B3)".into(),
            })
            .unwrap();
        assert_eq!(
            wb.sheets[0].cells[&(0, 0)].formula.as_deref(),
            Some("SUM(B1:B3)")
        );
        wb.apply(SheetOp::SetCell {
            sheet: 0,
            r: 1,
            c: 0,
            input: "1,200".into(),
        })
        .unwrap();
        assert_eq!(wb.sheets[0].cells[&(1, 0)].value, Value::Number(1200.0));
        wb.apply(undo).unwrap();
        assert!(!wb.sheets[0].cells.contains_key(&(0, 0)));
    }

    #[test]
    fn conditional_rules_follow_priority_and_stop_if_true() {
        let mut first = crate::look::CellStyle::partial();
        first.fill = Some(crate::look::Rgb(0xFF0000));
        first.present.fill = true;
        let mut second = crate::look::CellStyle::partial();
        second.fill = Some(crate::look::Rgb(0x0000FF));
        second.present.fill = true;
        let mut sheet = Sheet::new("rules");
        sheet.cells.insert(
            (0, 0),
            Cell {
                value: Value::Number(5.0),
                formula: None,
                display: "5".into(),
                bold: false,
                style: 0,
                edited: false,
            },
        );
        sheet.look.conditional = vec![
            crate::look::CondRule {
                range: (0, 0, 0, 0),
                kind: crate::look::CondKind::CellIs(crate::look::Compare::Greater(0.0)),
                style: Some(first),
                priority: 1,
                stop_if_true: true,
            },
            crate::look::CondRule {
                range: (0, 0, 0, 0),
                kind: crate::look::CondKind::CellIs(crate::look::Compare::Greater(0.0)),
                style: Some(second),
                priority: 2,
                stop_if_true: false,
            },
        ];
        let wb = Workbook::from_sheets(vec![sheet]);
        assert_eq!(
            wb.shown(0, 0, 0).style.fill,
            Some(crate::look::Rgb(0xFF0000))
        );
    }

    #[test]
    fn set_styles_formats_a_range_and_undoes_in_one_step() {
        let mut wb = Workbook::from_sheets(vec![Sheet::new("range")]);
        let style = wb.add_style(crate::look::CellStyle {
            fill: Some(crate::look::Rgb(0x00FF00)),
            ..Default::default()
        });
        let undo = wb
            .apply(SheetOp::SetStyles {
                sheet: 0,
                from: (0, 0),
                to: (1, 1),
                style,
            })
            .unwrap();
        assert_eq!(wb.sheets[0].cells[&(1, 1)].style, style);
        wb.apply(undo).unwrap();
        assert!(wb.sheets[0].cells.is_empty());
    }
}
