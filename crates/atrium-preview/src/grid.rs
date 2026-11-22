//! Spreadsheets and CSV as cell grids.

use std::io::Read;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use calamine::{Data, HorizontalAlignment, Reader, Sheets, open_workbook_auto};

use crate::{Ask, Cell, CellAlign, Preview, Refused};

const MAX_ROWS: usize = 10_000;
const MAX_COLS: usize = 200;

pub(crate) fn sheet(path: &Path, ask: &Ask) -> Result<Preview, Refused> {
    let result = catch_unwind(AssertUnwindSafe(|| read_sheet(path, ask)));
    match result {
        Ok(result) => result,
        Err(_) => Err(Refused::Damaged),
    }
}

fn read_sheet(path: &Path, ask: &Ask) -> Result<Preview, Refused> {
    let mut workbook = open_workbook_auto(path).map_err(|_| Refused::Damaged)?;
    let sheets = workbook.sheet_names().to_vec();
    if sheets.is_empty() {
        return Ok(Preview::Table {
            sheets: vec!["Sheet1".into()],
            current: 0,
            rows: Vec::new(),
            more_rows: false,
        });
    }
    let current = ask.sheet.unwrap_or(0).min(sheets.len() - 1);
    let name = sheets[current].clone();
    let range = workbook
        .worksheet_range_at(current)
        .ok_or(Refused::Damaged)?
        .map_err(|_| Refused::Damaged)?;
    let styles = workbook.worksheet_style(&name).ok();

    let start_row = range.start().map(|(row, _)| row).unwrap_or(0) as usize;
    let start_col = range.start().map(|(_, col)| col).unwrap_or(0) as usize;
    let height = range.height();
    let width = range.width();
    let rows_wanted = height.min(MAX_ROWS);
    let cols_wanted = width.min(MAX_COLS);

    let merged = merged_regions(&mut workbook, &name);
    let mut rows = Vec::with_capacity(rows_wanted);
    for row in 0..rows_wanted {
        let mut cells = Vec::with_capacity(cols_wanted);
        for col in 0..cols_wanted {
            let absolute = ((start_row + row) as u32, (start_col + col) as u32);
            let data = range.get((row, col));
            let style = styles.as_ref().and_then(|styles| styles.get(absolute));
            let mut cell = cell_from_data(data, style);
            for (dims_start, dims_end) in &merged {
                let (r0, c0) = (dims_start.0 as usize, dims_start.1 as usize);
                let (r1, c1) = (dims_end.0 as usize, dims_end.1 as usize);
                if absolute.0 as usize == r0 && absolute.1 as usize == c0 {
                    let span_rows = (r1.saturating_sub(r0) + 1).min(u16::MAX as usize) as u16;
                    let span_cols = (c1.saturating_sub(c0) + 1).min(u16::MAX as usize) as u16;
                    cell.span = (span_rows, span_cols);
                } else if (r0..=r1).contains(&(absolute.0 as usize))
                    && (c0..=c1).contains(&(absolute.1 as usize))
                {
                    cell.text.clear();
                    cell.bold = false;
                }
            }
            cells.push(cell);
        }
        rows.push(cells);
    }

    Ok(Preview::Table {
        sheets,
        current,
        rows,
        more_rows: height > MAX_ROWS,
    })
}

fn merged_regions(
    workbook: &mut Sheets<std::io::BufReader<std::fs::File>>,
    sheet: &str,
) -> Vec<((u32, u32), (u32, u32))> {
    if let Sheets::Xlsx(xlsx) = workbook {
        xlsx.merge_cells_by_sheet_name(sheet)
            .into_iter()
            .flatten()
            .map(|dims| (dims.start, dims.end))
            .collect()
    } else {
        Vec::new()
    }
}

fn cell_from_data(data: Option<&Data>, style: Option<&calamine::Style>) -> Cell {
    let (text, numeric) = match data {
        Some(Data::String(text)) => (text.clone(), false),
        Some(Data::Float(value)) => (number(*value, style), true),
        Some(Data::Int(value)) => (number(*value as f64, style), true),
        Some(Data::Bool(value)) => (if *value { "TRUE" } else { "FALSE" }.into(), false),
        Some(Data::DateTime(value)) => (datetime(value, style), true),
        Some(Data::DateTimeIso(text)) => (text.clone(), false),
        Some(Data::DurationIso(text)) => (text.clone(), false),
        Some(Data::Error(error)) => (format!("#{}", error), false),
        Some(Data::Empty) | None => (String::new(), false),
    };
    let bold = style
        .and_then(|style| style.font.as_ref())
        .is_some_and(|font| font.is_bold());
    let align = match style
        .and_then(|style| style.alignment.as_ref())
        .map(|alignment| alignment.horizontal)
    {
        Some(HorizontalAlignment::Right) => CellAlign::Right,
        Some(HorizontalAlignment::Center | HorizontalAlignment::CenterContinuous) => {
            CellAlign::Center
        }
        Some(HorizontalAlignment::Justify | HorizontalAlignment::Distributed) => CellAlign::Left,
        Some(HorizontalAlignment::Left) => CellAlign::Left,
        _ if numeric => CellAlign::Right,
        _ => CellAlign::Left,
    };
    Cell {
        text,
        bold,
        align,
        span: (1, 1),
    }
}

fn datetime(value: &calamine::ExcelDateTime, style: Option<&calamine::Style>) -> String {
    let (year, month, day, hour, minute, second, milli) = value.to_ymd_hms_milli();
    if value.is_duration() {
        let total = value.as_f64();
        let seconds = (total * 86_400.0).round() as u64;
        return format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        );
    }
    let date = format!("{year:04}-{month:02}-{day:02}");
    let has_time = hour != 0 || minute != 0 || second != 0 || milli != 0;
    let _ = style;
    if has_time {
        format!("{date} {hour:02}:{minute:02}")
    } else {
        date
    }
}

fn number(value: f64, style: Option<&calamine::Style>) -> String {
    let format = style
        .and_then(|style| style.number_format.as_ref())
        .map(|format| format.format_code.to_ascii_lowercase())
        .unwrap_or_default();
    if format.contains('%') {
        return format!("{:.2}%", value * 100.0);
    }
    let currency = if format.contains('¥') || format.contains("￥") || format.contains("cny") {
        Some("¥")
    } else if format.contains('$') || format.contains("usd") {
        Some("$")
    } else {
        None
    };
    if let Some(symbol) = currency {
        return format!("{symbol}{}", thousands(value, 2));
    }
    if format.contains("0.00") {
        return thousands(value, 2);
    }
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        let mut text = format!("{value}");
        if text.len() > 18 {
            text = format!("{value:.6}");
            while text.ends_with('0') {
                text.pop();
            }
            if text.ends_with('.') {
                text.pop();
            }
        }
        text
    }
}

fn thousands(value: f64, decimals: usize) -> String {
    let negative = value < 0.0;
    let text = format!("{:.*}", decimals, value.abs());
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let mut grouped = String::new();
    for (index, ch) in whole.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let whole: String = grouped.chars().rev().collect();
    let mut out = whole;
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(fraction);
    }
    if negative {
        out.insert(0, '-');
    }
    out
}

/// CSV and TSV: decoding, separator sniffing and a bounded number of rows.
pub(crate) fn csv(path: &Path, ask: &Ask) -> Result<Preview, Refused> {
    let result = catch_unwind(AssertUnwindSafe(|| read_csv(path, ask)));
    match result {
        Ok(result) => result,
        Err(_) => Err(Refused::Damaged),
    }
}

fn read_csv(path: &Path, ask: &Ask) -> Result<Preview, Refused> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|_| Refused::Failed("the file is gone".into()))?;
    let text = decode(&bytes);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let delimiter = if ext == "tsv" { b'\t' } else { sniff(&text) };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    let mut more_rows = false;
    for record in reader.records() {
        let Ok(record) = record else {
            break;
        };
        if rows.len() >= MAX_ROWS {
            more_rows = true;
            break;
        }
        let cells: Vec<Cell> = record
            .iter()
            .take(MAX_COLS)
            .map(|field| Cell {
                text: field.to_string(),
                bold: false,
                align: CellAlign::Left,
                span: (1, 1),
            })
            .collect();
        rows.push(cells);
    }
    let name = if ext == "tsv" { "TSV" } else { "CSV" };
    let _ = ask;
    Ok(Preview::Table {
        sheets: vec![name.into()],
        current: 0,
        rows,
        more_rows,
    })
}

/// UTF-8, a UTF-16 BOM, then GBK (Chinese) and Shift-JIS.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        let (text, _, _) = encoding_rs::UTF_16LE.decode(rest);
        return text.into_owned();
    }
    if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        let (text, _, _) = encoding_rs::UTF_16BE.decode(rest);
        return text.into_owned();
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    let (text, _, had_errors) = encoding_rs::GBK.decode(bytes);
    if !had_errors {
        return text.into_owned();
    }
    let (text, _, _) = encoding_rs::SHIFT_JIS.decode(bytes);
    text.into_owned()
}

fn sniff(text: &str) -> u8 {
    let line = text.lines().next().unwrap_or_default();
    let mut best = (b',', 0usize);
    for delimiter in b"\t;|," {
        let count = line.bytes().filter(|byte| byte == delimiter).count();
        if count > best.1 {
            best = (*delimiter, count);
        }
    }
    best.0
}
