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
    /// Puts a cell back exactly; the inverse of an edit.
    Restore {
        sheet: usize,
        r: u32,
        c: u32,
        cell: Option<Cell>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Workbook {
    pub(crate) sheets: Vec<Sheet>,
    /// Each distinct cell style once; index 0 is the default.
    pub(crate) styles: Vec<crate::look::CellStyle>,
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
        }
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
    /// This first version applies the style alone; backlog item 17 adds the
    /// rest (docs/slides-sheets-implementation.md).
    pub fn shown(&self, sheet: usize, r: u32, c: u32) -> Shown {
        let Some(s) = self.sheets.get(sheet) else {
            return Shown {
                style: Default::default(),
                text: String::new(),
                bar: None,
            };
        };
        let style = self
            .styles
            .get(s.style_index(r, c) as usize)
            .cloned()
            .unwrap_or_default();
        let text = s
            .cells
            .get(&(r, c))
            .map(|cell| {
                // Formulas show the result the file keeps until Excel recalculates.
                if cell.formula.is_some() && !cell.edited {
                    cell.display.clone()
                } else {
                    cell.input()
                }
            })
            .unwrap_or_default();
        Shown {
            style,
            text,
            bar: None,
        }
    }

    pub fn sheets(&self) -> &[Sheet] {
        &self.sheets
    }

    pub fn apply(&mut self, op: SheetOp) -> Result<SheetOp, EditError> {
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
}
