//! Excel number formats, reduced to what a spreadsheet cell shows.
//!
//! The formatter is deliberately independent of the file binding: the model's
//! `Workbook::shown` and the binding's cached `Cell::display` use the same
//! code, so an edited value and a value read from a file agree.

use crate::look::Rgb;
use crate::sheet::Value;

/// A formatted value and the colour a number-format section asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Formatted {
    pub text: String,
    pub color: Option<Rgb>,
}

/// A built-in format code, by its `numFmtId`.
///
/// The first block is ECMA-376's table. The Chinese ids 27–36 and 50–58 are
/// the built-ins Excel and WPS use when the editing language is Chinese
/// (Simplified). They are kept here rather than in `styles.xml`, which often
/// stores only the id.
pub(crate) fn builtin_format(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "$#,##0_);($#,##0)",
        6 => "$#,##0_);[Red]($#,##0)",
        7 => "$#,##0.00_);($#,##0.00)",
        8 => "$#,##0.00_);[Red]($#,##0.00)",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "yyyy-mm-dd",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "m/d/yy h:mm",
        27 => "yyyy\"年\"m\"月\"",
        28 => "m\"月\"d\"日\"",
        29 => "m\"月\"d\"日\"",
        30 => "m-d-yy",
        31 => "yyyy\"年\"m\"月\"d\"日\"",
        32 => "h\"时\"mm\"分\"",
        33 => "h\"时\"mm\"分\"ss\"秒\"",
        34 => "上午/下午h\"时\"mm\"分\"",
        35 => "上午/下午h\"时\"mm\"分\"ss\"秒\"",
        36 => "yyyy\"年\"m\"月\"",
        37 => "#,##0 ;(#,##0)",
        38 => "#,##0 ;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        41 => "_(* #,##0_);_(* \\(#,##0\\);_(* \"-\"_);_(@_)",
        42 => "_(\"$\"* #,##0_);_(\"$\"* \\(#,##0\\);_(\"$\"* \"-\"_);_(@_)",
        43 => "_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)",
        44 => "_(\"$\"* #,##0.00_);_(\"$\"* \\(#,##0.00\\);_(\"$\"* \"-\"??_);_(@_)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mmss.0",
        48 => "##0.0E+0",
        49 => "@",
        50 => "yyyy\"年\"m\"月\"",
        51 => "m\"月\"d\"日\"",
        52 => "yyyy\"年\"m\"月\"",
        53 => "m\"月\"d\"日\"",
        54 => "m\"月\"d\"日\"",
        55 => "上午/下午h\"时\"mm\"分\"",
        56 => "上午/下午h\"时\"mm\"分\"ss\"秒\"",
        57 => "yyyy\"年\"m\"月\"",
        58 => "m\"月\"d\"日\"",
        _ => return None,
    })
}

/// Formats `value` with `code`. `date1904` selects the workbook's date system.
pub(crate) fn format_value(value: &Value, code: &str, date1904: bool) -> Formatted {
    let code = if code.trim().is_empty() {
        "General"
    } else {
        code
    };
    match value {
        Value::Text(text) => format_text(text, code),
        Value::Bool(true) => Formatted {
            text: "TRUE".into(),
            color: None,
        },
        Value::Bool(false) => Formatted {
            text: "FALSE".into(),
            color: None,
        },
        Value::Error(text) => Formatted {
            text: text.clone(),
            color: section_color(split_sections(code).first().copied().unwrap_or(code)),
        },
        Value::Empty => Formatted {
            text: String::new(),
            color: None,
        },
        Value::Number(n) => format_number(*n, code, date1904),
    }
}

fn format_text(text: &str, code: &str) -> Formatted {
    let sections = split_sections(code);
    let section = if sections.len() >= 4 {
        sections[3]
    } else if sections.first().is_some_and(|s| unquoted_contains(s, '@')) {
        sections[0]
    } else {
        return Formatted {
            text: text.to_owned(),
            color: None,
        };
    };
    let color = section_color(section);
    let mut out = String::new();
    let mut chars = section.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => copy_quoted(&mut chars, &mut out),
            '\\' => {
                if let Some(next) = chars.next() {
                    out.push(next)
                }
            }
            '_' | '*' => {
                chars.next();
            }
            '[' => skip_bracket(&mut chars),
            '@' => out.push_str(text),
            _ => out.push(c),
        }
    }
    Formatted { text: out, color }
}

fn format_number(n: f64, code: &str, date1904: bool) -> Formatted {
    let sections = split_sections(code);
    let (section, value, negative) = if n < 0.0 && sections.len() > 1 {
        (sections[1], -n, true)
    } else if n == 0.0 && sections.len() > 2 {
        (sections[2], 0.0, false)
    } else {
        (sections.first().copied().unwrap_or("General"), n, false)
    };
    let color = section_color(section);
    if section.trim().eq_ignore_ascii_case("general") {
        return Formatted {
            text: general_number(n),
            color,
        };
    }
    if is_date_format(section) {
        return Formatted {
            text: format_date(value, section, date1904),
            color,
        };
    }
    let mut text = format_number_section(value, section, negative);
    // A negative value with one section keeps Excel's minus sign; with a
    // negative section the section's own literals (often parentheses) say it.
    if !negative && n < 0.0 && !text.starts_with('-') {
        text.insert(0, '-');
    }
    Formatted { text, color }
}

fn general_number(n: f64) -> String {
    if n == 0.0 {
        return "0".into();
    }
    let abs = n.abs();
    if !n.is_finite() {
        return n.to_string();
    }
    if abs >= 1e11 || abs < 1e-9 {
        return scientific_plain(n);
    }
    let mut s = format!("{n:.15}");
    while s.ends_with('0') && s.contains('.') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" { "0".into() } else { s }
}

fn scientific_plain(n: f64) -> String {
    if n == 0.0 {
        return "0".into();
    }
    let exp = n.abs().log10().floor() as i32;
    let mantissa = n / 10f64.powi(exp);
    let mut m = format!("{mantissa:.5}");
    while m.ends_with('0') && m.contains('.') {
        m.pop();
    }
    if m.ends_with('.') {
        m.pop();
    }
    format!("{m}E{exp:+}")
}

fn split_sections(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quote = false;
    let mut bracket = 0usize;
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' if bracket == 0 => quote = !quote,
            b'[' if !quote => bracket += 1,
            b']' if !quote && bracket > 0 => bracket -= 1,
            b'\\' if !quote => i += 1,
            b';' if !quote && bracket == 0 => {
                out.push(&code[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&code[start..]);
    out
}

fn unquoted_contains(code: &str, needle: char) -> bool {
    let mut quote = false;
    let mut bracket = 0usize;
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' if bracket == 0 => quote = !quote,
            '[' if !quote => bracket += 1,
            ']' if !quote && bracket > 0 => bracket -= 1,
            '\\' if !quote => {
                chars.next();
            }
            _ if !quote && bracket == 0 && c == needle => return true,
            _ => {}
        }
    }
    false
}

fn section_color(section: &str) -> Option<Rgb> {
    let mut chars = section.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut sink = String::new();
                copy_quoted(&mut chars, &mut sink);
            }
            '\\' => {
                chars.next();
            }
            '[' => {
                let mut content = String::new();
                for x in chars.by_ref() {
                    if x == ']' {
                        break;
                    }
                    content.push(x);
                }
                if let Some(color) = named_color(content.trim()) {
                    return Some(color);
                }
            }
            _ => {}
        }
    }
    None
}

fn named_color(name: &str) -> Option<Rgb> {
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "black" => Rgb(0x000000),
        "blue" => Rgb(0x0000FF),
        "cyan" => Rgb(0x00FFFF),
        "green" => Rgb(0x00FF00),
        "magenta" => Rgb(0xFF00FF),
        "red" => Rgb(0xFF0000),
        "white" => Rgb(0xFFFFFF),
        "yellow" => Rgb(0xFFFF00),
        _ => {
            if let Some(rest) = lower.strip_prefix("color") {
                let index: usize = rest.trim().parse().ok()?;
                return indexed_color(index as u32);
            }
            return None;
        }
    })
}

fn copy_quoted(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, out: &mut String) {
    for c in chars.by_ref() {
        if c == '"' {
            break;
        }
        out.push(c);
    }
}

fn skip_bracket(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for c in chars.by_ref() {
        if c == ']' {
            break;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok<'a> {
    Digit(u8),
    Dot,
    Comma,
    Percent,
    Slash,
    Exp(char),
    At,
    Lit(&'a str),
}

fn tokens(code: &str) -> Vec<Tok<'_>> {
    let mut out = Vec::new();
    let bytes = code.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += 1;
                }
                out.push(Tok::Lit(&code[start..i.min(bytes.len())]));
                if i < bytes.len() {
                    i += 1;
                }
            }
            b'\\' => {
                i += 1;
                if i < bytes.len() {
                    let ch = code[i..].chars().next().unwrap_or_default();
                    let end = i + ch.len_utf8();
                    out.push(Tok::Lit(&code[i..end]));
                    i = end;
                }
            }
            b'_' | b'*' => {
                i += 1;
                if i < bytes.len() {
                    i += code[i..].chars().next().map_or(0, char::len_utf8);
                }
            }
            b'[' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                let content = &code[start..i.min(bytes.len())];
                if content.eq_ignore_ascii_case("h")
                    || content.eq_ignore_ascii_case("m")
                    || content.eq_ignore_ascii_case("s")
                {
                    // Elapsed units belong to the date path; the numeric path
                    // treats them as an empty literal.
                }
                if i < bytes.len() {
                    i += 1;
                }
            }
            b'0' | b'#' | b'?' => {
                out.push(Tok::Digit(bytes[i]));
                i += 1;
            }
            b'.' => {
                out.push(Tok::Dot);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            b'/' => {
                out.push(Tok::Slash);
                i += 1;
            }
            b'E' | b'e' => {
                let mut j = i + 1;
                let mut sign = ' ';
                if j < bytes.len() && matches!(bytes[j], b'+' | b'-') {
                    sign = bytes[j] as char;
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'0' {
                    out.push(Tok::Exp(sign));
                    i = j;
                } else {
                    out.push(Tok::Lit(&code[i..i + 1]));
                    i += 1;
                }
            }
            b'@' => {
                out.push(Tok::At);
                i += 1;
            }
            _ => {
                let ch = code[i..].chars().next().unwrap_or_default();
                let end = i + ch.len_utf8();
                out.push(Tok::Lit(&code[i..end]));
                i = end;
            }
        }
    }
    out
}

fn format_number_section(value: f64, code: &str, negative_section: bool) -> String {
    let toks = tokens(code);
    if toks.is_empty() {
        return String::new();
    }
    if !toks.iter().any(|t| {
        matches!(
            t,
            Tok::Digit(_) | Tok::Dot | Tok::Comma | Tok::Percent | Tok::Slash | Tok::Exp(_)
        )
    }) {
        let mut out = String::new();
        for t in &toks {
            push_literal(t, &mut out);
        }
        return out;
    }
    let scaled = toks.iter().filter(|t| **t == Tok::Percent).count();
    let mut value = value * 100f64.powi(scaled as i32);
    // A comma immediately after the last digit placeholder divides by 1000.
    let last_digit = toks
        .iter()
        .rposition(|t| matches!(t, Tok::Digit(_) | Tok::Percent));
    if let Some(last) = last_digit {
        let mut k = last + 1;
        while k < toks.len() && toks[k] == Tok::Comma {
            value /= 1000.0;
            k += 1;
        }
    }
    if toks.contains(&Tok::Slash) {
        return format_fraction(value, &toks);
    }
    if let Some(pos) = toks.iter().position(|t| matches!(t, Tok::Exp(_))) {
        return format_scientific(value, &toks, pos, negative_section);
    }
    format_fixed(value, &toks, negative_section)
}

fn prefix_suffix<'a>(toks: &'a [Tok<'a>]) -> (String, String) {
    let first = toks.iter().position(|t| {
        matches!(
            t,
            Tok::Digit(_) | Tok::Dot | Tok::Comma | Tok::Percent | Tok::Slash | Tok::Exp(_)
        )
    });
    let last = toks.iter().rposition(|t| {
        matches!(
            t,
            Tok::Digit(_) | Tok::Dot | Tok::Comma | Tok::Percent | Tok::Slash | Tok::Exp(_)
        )
    });
    let mut prefix = String::new();
    let mut suffix = String::new();
    if let Some(first) = first {
        for t in &toks[..first] {
            push_literal(t, &mut prefix);
        }
        if let Some(last) = last {
            for t in &toks[last + 1..] {
                push_literal(t, &mut suffix);
            }
        }
    }
    (prefix, suffix)
}

fn push_literal(t: &Tok<'_>, out: &mut String) {
    match t {
        Tok::Lit(s) => out.push_str(s),
        Tok::At => out.push('@'),
        _ => {}
    }
}

fn format_fixed(value: f64, toks: &[Tok<'_>], negative_section: bool) -> String {
    let (prefix, suffix) = prefix_suffix(toks);
    let dot = toks.iter().position(|t| *t == Tok::Dot);
    let int_end = dot.unwrap_or(toks.len());
    let int_digits: Vec<u8> = toks[..int_end]
        .iter()
        .filter_map(|t| match t {
            Tok::Digit(c) => Some(*c),
            _ => None,
        })
        .collect();
    let dec_digits: Vec<u8> = dot
        .map(|d| {
            toks[d + 1..]
                .iter()
                .take_while(|t| !matches!(t, Tok::Percent))
                .filter_map(|t| match t {
                    Tok::Digit(c) => Some(*c),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let min_int = int_digits.iter().filter(|c| **c == b'0').count();
    let decimals = dec_digits.len();
    let grouping = toks[..int_end].contains(&Tok::Comma);
    let percent = toks.iter().filter(|t| **t == Tok::Percent).count();
    let abs = value.abs();
    let factor = 10f64.powi(decimals as i32);
    let abs = (abs * factor + 0.5).floor() / factor;
    let text = format!("{abs:.decimals$}");
    let mut parts = text.splitn(2, '.');
    let int = parts.next().unwrap_or("0");
    let dec = parts.next().unwrap_or("");
    let mut int = int.trim_start_matches('0').to_string();
    if int.is_empty() && min_int == 0 {
        int.clear();
    } else if int.is_empty() {
        int.push('0');
    }
    while int.len() < min_int {
        int.insert(0, '0');
    }
    if grouping {
        int = group_digits(&int);
    }
    let mut out = prefix;
    if value < 0.0 && !negative_section {
        out.push('-');
    }
    out.push_str(&int);
    let mut dec = dec.to_owned();
    let all_optional = dec_digits.iter().all(|c| *c != b'0');
    if all_optional {
        while dec.ends_with('0') {
            dec.pop();
        }
    }
    if !dec.is_empty() || (dot.is_some() && decimals > 0) {
        out.push('.');
        out.push_str(&dec);
    }
    for _ in 0..percent {
        out.push('%');
    }
    out.push_str(&suffix);
    out
}

fn format_scientific(value: f64, toks: &[Tok<'_>], at: usize, negative_section: bool) -> String {
    let (prefix, suffix) = prefix_suffix(toks);
    let sign = match toks[at] {
        Tok::Exp(c) => c,
        _ => ' ',
    };
    let exp_digits = toks[at + 1..]
        .iter()
        .filter_map(|t| match t {
            Tok::Digit(c) if *c == b'0' => Some(1usize),
            _ => None,
        })
        .sum::<usize>()
        .max(1);
    let mantissa_digits = toks[..at]
        .iter()
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count();
    let dot = toks[..at].iter().position(|t| *t == Tok::Dot);
    let decimals = dot
        .map(|d| {
            toks[d + 1..at]
                .iter()
                .filter(|t| matches!(t, Tok::Digit(_)))
                .count()
        })
        .unwrap_or_else(|| mantissa_digits.saturating_sub(1));
    let abs = value.abs();
    let exp = if abs == 0.0 {
        0
    } else {
        abs.log10().floor() as i32
    };
    let mantissa = if abs == 0.0 {
        0.0
    } else {
        abs / 10f64.powi(exp)
    };
    let mut out = prefix;
    if value < 0.0 && !negative_section {
        out.push('-');
    }
    out.push_str(&format!("{mantissa:.decimals$}"));
    out.push('E');
    if exp >= 0 {
        if sign == '+' {
            out.push('+');
        } else if sign == '-' {
            // A '-' format shows no sign for non-negative exponents.
        }
    } else {
        out.push('-');
    }
    let exp_text = exp.abs().to_string();
    for _ in exp_text.len()..exp_digits {
        out.push('0');
    }
    out.push_str(&exp_text);
    out.push_str(&suffix);
    out
}

fn format_fraction(value: f64, toks: &[Tok<'_>]) -> String {
    let (prefix, suffix) = prefix_suffix(toks);
    let slash = toks.iter().position(|t| *t == Tok::Slash).unwrap_or(0);
    let int_placeholders = toks[..slash]
        .iter()
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count();
    let num_placeholders = toks[..slash]
        .iter()
        .rev()
        .take_while(|t| !matches!(t, Tok::Lit(" ")))
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count()
        .min(int_placeholders);
    let den_placeholders = toks[slash + 1..]
        .iter()
        .filter(|t| matches!(t, Tok::Digit(_)))
        .count()
        .max(1);
    let max_den = 10u32.pow(den_placeholders as u32) - 1;
    let abs = value.abs();
    let whole = abs.floor() as u64;
    let frac = abs - whole as f64;
    let (num, den) = best_fraction(frac, max_den);
    let mut out = prefix;
    if value < 0.0 {
        out.push('-');
    }
    let show_whole = int_placeholders > num_placeholders;
    if show_whole && (whole > 0 || int_placeholders > num_placeholders) {
        out.push_str(&whole.to_string());
        if num > 0 {
            out.push(' ');
        }
    }
    if num > 0 {
        let ns = num.to_string();
        for _ in ns.len()..num_placeholders.max(1) {
            out.push(' ');
        }
        out.push_str(&ns);
        out.push('/');
        out.push_str(&den.to_string());
    } else if !show_whole {
        out.push('0');
    }
    out.push_str(&suffix);
    out
}

fn best_fraction(value: f64, max_den: u32) -> (u32, u32) {
    if value <= 0.0 || max_den == 0 {
        return (0, 1);
    }
    let mut best = (0u32, 1u32);
    let mut best_err = f64::INFINITY;
    for den in 1..=max_den {
        let num = (value * den as f64).round() as u32;
        if num == 0 {
            continue;
        }
        let err = (value - num as f64 / den as f64).abs();
        if err < best_err - 1e-12 {
            best_err = err;
            best = (num, den);
        }
    }
    (best.0, best.1)
}

fn group_digits(int: &str) -> String {
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub(crate) fn is_date_format(code: &str) -> bool {
    let lower = code.to_ascii_lowercase();
    let mut quote = false;
    let mut bracket = false;
    let mut chars = lower.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if !bracket => quote = !quote,
            '[' if !quote => bracket = true,
            ']' if !quote => bracket = false,
            '\\' if !quote => {
                chars.next();
            }
            _ if !quote && !bracket => {
                if matches!(c, 'y' | 'd' | 'h' | 's') {
                    return true;
                }
                if c == 'm' && (lower.contains(':') || lower.contains('月')) {
                    return true;
                }
            }
            _ => {}
        }
    }
    lower.contains("am/pm") || lower.contains("上午/下午")
}

fn format_date(value: f64, code: &str, date1904: bool) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let serial = if value < 0.0 { 0.0 } else { value };
    let total_seconds = (serial * 86400.0).round() as i64;
    let day = (serial.floor() as i64).max(0);
    let (y, m, d) = serial_date(day, date1904);
    let secs = total_seconds.rem_euclid(86400);
    let h24 = secs / 3600;
    let min = secs / 60 % 60;
    let sec = secs % 60;
    let weekday = weekday(day, date1904);
    let frac = (serial.fract() * 86400.0).fract();
    let ampm = code.to_ascii_lowercase().contains("am/pm");
    let cn_ampm = code.contains("上午/下午");
    let h = if ampm || cn_ampm {
        (h24 + 11) % 12 + 1
    } else {
        h24
    };
    let months = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let days = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    let chars: Vec<char> = code.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut last: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    out.push(chars[i]);
                    i += 1;
                }
                i += usize::from(i < chars.len());
            }
            '\\' => {
                i += 1;
                if i < chars.len() {
                    out.push(chars[i]);
                    i += 1;
                }
            }
            '_' | '*' => i += 2,
            '[' => {
                let start = i + 1;
                i = start;
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
                let content: String = chars[start..i].iter().collect();
                i += usize::from(i < chars.len());
                let low = content.to_ascii_lowercase();
                match low.as_str() {
                    "h" => {
                        out.push_str(&(total_seconds / 3600).to_string());
                        last = Some('h');
                    }
                    "m" => {
                        out.push_str(&(total_seconds / 60).to_string());
                        last = Some('m');
                    }
                    "s" => {
                        out.push_str(&total_seconds.to_string());
                        last = Some('s');
                    }
                    _ => {}
                }
            }
            _ if starts_ci(&chars, i, "am/pm") => {
                i += 5;
                out.push_str(if h24 < 12 { "AM" } else { "PM" });
            }
            _ if starts_ci(&chars, i, "a/p") => {
                i += 3;
                out.push(if h24 < 12 { 'A' } else { 'P' });
            }
            _ if chars[i..].starts_with(&['上', '午', '/', '下', '午']) => {
                i += 5;
                out.push_str(if h24 < 12 { "上午" } else { "下午" });
            }
            'y' | 'Y' => {
                let n = run(&chars, &mut i, 'y');
                if n <= 2 {
                    out.push_str(&format!("{:02}", y % 100));
                } else {
                    out.push_str(&format!("{y:04}"));
                }
                last = Some('y');
            }
            'm' | 'M' => {
                let n = run(&chars, &mut i, 'm');
                let next = next_letter(&chars, i);
                if last == Some('h') || next == Some('s') {
                    out.push_str(&pad(min, n));
                    last = Some('m');
                } else {
                    match n {
                        1 => out.push_str(&m.to_string()),
                        2 => out.push_str(&format!("{m:02}")),
                        3 => out.push_str(&months[(m - 1) as usize][..3]),
                        4 => out.push_str(months[(m - 1) as usize]),
                        _ => out.push(months[(m - 1) as usize].chars().next().unwrap_or('M')),
                    }
                    last = Some('M');
                }
            }
            'd' | 'D' => {
                let n = run(&chars, &mut i, 'd');
                match n {
                    1 => out.push_str(&d.to_string()),
                    2 => out.push_str(&format!("{d:02}")),
                    3 => out.push_str(&days[weekday][..3]),
                    _ => out.push_str(days[weekday]),
                }
                last = Some('d');
            }
            'h' | 'H' => {
                let n = run(&chars, &mut i, 'h');
                out.push_str(&pad(h, n));
                last = Some('h');
            }
            's' | 'S' => {
                let n = run(&chars, &mut i, 's');
                out.push_str(&pad(sec, n));
                last = Some('s');
            }
            '.' => {
                out.push('.');
                i += 1;
                let mut zeros = 0;
                while i < chars.len() && chars[i] == '0' {
                    i += 1;
                    zeros += 1;
                }
                if zeros > 0 {
                    let digits = (frac * 10f64.powi(zeros)).round() as u64;
                    out.push_str(&format!("{digits:0width$}", width = zeros as usize));
                }
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn starts_ci(chars: &[char], at: usize, text: &str) -> bool {
    for (offset, c) in text.chars().enumerate() {
        let i = at + offset;
        if i >= chars.len() || !chars[i].eq_ignore_ascii_case(&c) {
            return false;
        }
    }
    true
}

fn run(chars: &[char], i: &mut usize, c: char) -> usize {
    let mut n = 0;
    while *i < chars.len() && chars[*i].eq_ignore_ascii_case(&c) {
        *i += 1;
        n += 1;
    }
    n
}

fn next_letter(chars: &[char], mut i: usize) -> Option<char> {
    let mut quote = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '"' => quote = !quote,
            '\\' if !quote => i += 1,
            '[' if !quote => {
                while i < chars.len() && chars[i] != ']' {
                    i += 1;
                }
            }
            _ if !quote && matches!(c.to_ascii_lowercase(), 'y' | 'm' | 'd' | 'h' | 's') => {
                return Some(c.to_ascii_lowercase());
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn pad(n: i64, width: usize) -> String {
    if width <= 1 {
        n.to_string()
    } else {
        format!("{n:0width$}")
    }
}

/// The civil date for an Excel serial day in the workbook's date system.
fn serial_date(day: i64, date1904: bool) -> (i64, i64, i64) {
    if !date1904 && day == 60 {
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
    }
}

/// 0 is Sunday.
fn weekday(day: i64, date1904: bool) -> usize {
    let base = if date1904 { 0 } else { 1 };
    // 1900-01-01 was a Monday in Excel's 1900 system.
    ((day - base + 1).rem_euclid(7)) as usize
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

/// Parses `2024-03-05`, `2024年3月5日` and the date part of a time into a
/// serial. Kept here so the editor and the loader share one date system.
pub(crate) fn date_serial(s: &str, date1904: bool) -> Option<f64> {
    let cleaned = s.replace(['年', '月'], "-").replace('日', "");
    let mut parts = cleaned.split('-');
    let y = parts.next()?.trim().parse().ok()?;
    let m = parts.next()?.trim().parse().ok()?;
    let d = parts.next()?.trim().parse().ok()?;
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

const INDEXED: [u32; 64] = [
    0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x000000,
    0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0xFFFF00, 0xFF00FF, 0x00FFFF, 0x800000, 0x008000,
    0x000080, 0x808000, 0x800080, 0x008080, 0xC0C0C0, 0x808080, 0x9999FF, 0x993366, 0xFFFFCC,
    0xCCFFFF, 0x660066, 0xFF8080, 0x0066CC, 0xCCCCFF, 0x000080, 0xFF00FF, 0xFFFF00, 0x00FFFF,
    0x800080, 0x800000, 0x008080, 0x0000FF, 0x00CCFF, 0xCCFFFF, 0xCCFFCC, 0xFFFF99, 0x99CCFF,
    0xFF99CC, 0xCC99FF, 0xFFCC99, 0x3366FF, 0x33CCCC, 0x99CC00, 0xFFCC00, 0xFF9900, 0xFF6600,
    0x666699, 0x969696, 0x003366, 0x339966, 0x003300, 0x333300, 0x993300, 0x993366, 0x333399,
    0x333333,
];

pub(crate) fn indexed_color(index: u32) -> Option<Rgb> {
    match index {
        0..=63 => Some(Rgb(INDEXED[index as usize])),
        64 => Some(Rgb(0x000000)),
        65 => Some(Rgb(0xFFFFFF)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(n: f64, code: &str) -> String {
        format_value(&Value::Number(n), code, false).text
    }

    #[test]
    fn common_number_formats() {
        assert_eq!(f(1234.5, "General"), "1234.5");
        assert_eq!(f(1234.5, "0"), "1235");
        assert_eq!(f(1234.5, "0.00"), "1234.50");
        assert_eq!(f(1234.5, "#,##0.00"), "1,234.50");
        assert_eq!(f(0.125, "0.00%"), "12.50%");
        assert_eq!(f(1234.5, "0.00E+00"), "1.23E+03");
        assert_eq!(f(1.25, "# ?/?"), "1 1/4");
        assert_eq!(f(-1234.0, "#,##0"), "-1,234");
        assert_eq!(f(-1234.0, "#,##0;[Red](#,##0)"), "(1,234)");
    }

    #[test]
    fn sections_and_colours() {
        let negative = format_value(&Value::Number(-12.5), "0.00;[Red]-0.00", false);
        assert_eq!(negative.text, "-12.50");
        assert_eq!(negative.color, Some(Rgb(0xFF0000)));
        assert_eq!(f(0.0, "0.00;\"-\";\"zero\""), "zero");
    }

    #[test]
    fn dates_in_both_systems() {
        assert_eq!(f(45356.0, "yyyy\"年\"m\"月\"d\"日\""), "2024年3月5日");
        assert_eq!(f(45356.625, "h:mm AM/PM"), "3:00 PM");
        assert_eq!(f(45356.625, "上午/下午h\"时\"mm\"分\""), "下午3时00分");
        assert_eq!(
            f(45356.0, "[$-804]yyyy\"年\"m\"月\"d\"日\""),
            "2024年3月5日"
        );
        assert_eq!(f(45356.625, builtin_format(55).unwrap()), "下午3时00分");
        assert_eq!(f(60.0, "yyyy-mm-dd"), "1900-02-29");
        assert_eq!(
            format_value(&Value::Number(0.0), "yyyy-mm-dd", true).text,
            "1904-01-01"
        );
    }

    #[test]
    fn text_and_escaped_literals() {
        assert_eq!(
            format_value(&Value::Text("abc".into()), "@\"!\"", false).text,
            "abc!"
        );
        assert_eq!(f(12.0, "\"¥\"#,##0.00"), "¥12.00");
        assert_eq!(f(0.5, "0.0\"kg\""), "0.5kg");
    }

    #[test]
    fn builtins_cover_chinese_ids() {
        assert_eq!(builtin_format(27), Some("yyyy\"年\"m\"月\""));
        assert_eq!(builtin_format(31), Some("yyyy\"年\"m\"月\"d\"日\""));
        assert_eq!(builtin_format(55), Some("上午/下午h\"时\"mm\"分\""));
        assert!(builtin_format(58).is_some());
    }
}
