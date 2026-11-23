//! A chart from its cached values. Shared by slides and sheets.

use crate::{
    OpenError,
    look::{Chart, ChartKind, Rgb, Series},
    package::Package,
    sheet_styles::{Theme, read_color},
    xml::XmlNode,
};

const C: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
const CS: &str = "http://purl.oclc.org/ooxml/drawingml/chart";
const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const AS: &str = "http://purl.oclc.org/ooxml/drawingml/main";

fn c_is(n: &XmlNode, name: &str) -> bool {
    matches!(n.namespace.as_deref(), Some(C | CS)) && n.local_name == name
}

fn a_is(n: &XmlNode, name: &str) -> bool {
    matches!(n.namespace.as_deref(), Some(A | AS)) && n.local_name == name
}

fn child<'a>(n: &'a XmlNode, name: &str) -> Option<&'a XmlNode> {
    n.children.iter().find(|e| c_is(e, name))
}

fn a_child<'a>(n: &'a XmlNode, name: &str) -> Option<&'a XmlNode> {
    n.children.iter().find(|e| a_is(e, name))
}

fn attr<'a>(n: &'a XmlNode, name: &str) -> Option<&'a str> {
    n.attr(None, name)
}

fn bad(s: &str) -> OpenError {
    OpenError::Damaged(s.into())
}

pub(crate) fn read(package: &Package, path: &str, theme: &Theme) -> Result<Chart, OpenError> {
    let xml = package.part(path)?;
    let root = xml.root();
    if !c_is(root, "chartSpace") {
        return Err(bad("not a chart part"));
    }
    let Some(chart) = child(root, "chart") else {
        return Ok(Chart::default());
    };
    let title = child(chart, "title").and_then(title_text);
    let legend = child(chart, "legend").is_some();
    let Some(plot) = child(chart, "plotArea") else {
        return Ok(Chart {
            title,
            legend,
            ..Chart::default()
        });
    };
    let mut out = Chart {
        title,
        legend,
        ..Chart::default()
    };
    for n in &plot.children {
        let (kind, stacked, percent) = match n.local_name.as_str() {
            "barChart" if c_is(n, "barChart") => {
                let dir = child(n, "barDir")
                    .and_then(|d| attr(d, "val"))
                    .unwrap_or("col");
                let grouping = child(n, "grouping")
                    .and_then(|g| attr(g, "val"))
                    .unwrap_or("clustered");
                (
                    if dir == "bar" {
                        ChartKind::Bar
                    } else {
                        ChartKind::Column
                    },
                    grouping == "stacked",
                    grouping == "percentStacked",
                )
            }
            "lineChart" if c_is(n, "lineChart") => {
                let grouping = child(n, "grouping")
                    .and_then(|g| attr(g, "val"))
                    .unwrap_or("standard");
                (
                    ChartKind::Line,
                    grouping == "stacked",
                    grouping == "percentStacked",
                )
            }
            "areaChart" if c_is(n, "areaChart") => {
                let grouping = child(n, "grouping")
                    .and_then(|g| attr(g, "val"))
                    .unwrap_or("standard");
                (
                    ChartKind::Area,
                    grouping == "stacked",
                    grouping == "percentStacked",
                )
            }
            "pieChart" if c_is(n, "pieChart") => (ChartKind::Pie, false, false),
            "doughnutChart" if c_is(n, "doughnutChart") => (ChartKind::Doughnut, false, false),
            "scatterChart" if c_is(n, "scatterChart") => (ChartKind::Scatter, false, false),
            _ => continue,
        };
        out.kind = kind;
        out.stacked = stacked;
        out.percent = percent;
        for ser in n.children.iter().filter(|s| c_is(s, "ser")) {
            let (series, cats) = read_series(ser, theme);
            if out.categories.is_empty() && !cats.is_empty() {
                out.categories = cats;
            }
            out.series.push(series);
        }
        break;
    }
    Ok(out)
}

fn read_series(n: &XmlNode, theme: &Theme) -> (Series, Vec<String>) {
    let mut s = Series::default();
    if let Some(tx) = child(n, "tx") {
        s.name = cache_text(tx).unwrap_or_default();
    }
    if let Some(sp) = child(n, "spPr") {
        s.color = a_child(sp, "solidFill")
            .and_then(|fill| fill.children.iter().find(|c| a_is(c, "srgbClr")))
            .and_then(|c| read_color(c, theme));
    }
    let cats = child(n, "cat").and_then(cache_texts).unwrap_or_default();
    s.values = child(n, "val").map(cache_numbers).unwrap_or_default();
    if let Some(xv) = child(n, "xVal") {
        s.x = cache_numbers(xv);
    }
    (s, cats)
}

fn cache_text(n: &XmlNode) -> Option<String> {
    if let Some(v) = child(n, "v") {
        return Some(v.text.clone());
    }
    for kind in ["strRef", "numRef"] {
        if let Some(r) = child(n, kind) {
            let cache = child(
                r,
                if kind == "strRef" {
                    "strCache"
                } else {
                    "numCache"
                },
            )?;
            return points(cache).into_iter().next().map(|(_, v)| v);
        }
    }
    let rich = child(n, "rich")?;
    Some(text_body(rich))
}

fn cache_texts(n: &XmlNode) -> Option<Vec<String>> {
    for kind in ["strRef", "numRef"] {
        if let Some(r) = child(n, kind) {
            let cache = child(
                r,
                if kind == "strRef" {
                    "strCache"
                } else {
                    "numCache"
                },
            )?;
            return Some(points(cache).into_iter().map(|(_, v)| v).collect());
        }
    }
    None
}

fn cache_numbers(n: &XmlNode) -> Vec<Option<f64>> {
    for kind in ["numRef", "numLit"] {
        if let Some(r) = child(n, kind) {
            let Some(cache) = child(
                r,
                if kind == "numRef" {
                    "numCache"
                } else {
                    "numLit"
                },
            ) else {
                continue;
            };
            let points = points(cache);
            let len = points.iter().map(|(i, _)| *i + 1).max().unwrap_or(0);
            let mut out = vec![None; len];
            for (i, v) in points {
                if let Some(slot) = out.get_mut(i) {
                    *slot = v.parse::<f64>().ok().filter(|x| x.is_finite());
                }
            }
            return out;
        }
    }
    Vec::new()
}

fn points(cache: &XmlNode) -> Vec<(usize, String)> {
    cache
        .children
        .iter()
        .filter(|n| c_is(n, "pt"))
        .filter_map(|pt| {
            let i = attr(pt, "idx")?.parse().ok()?;
            let v = child(pt, "v").map(|v| v.text.clone()).unwrap_or_default();
            Some((i, v))
        })
        .collect()
}

fn title_text(n: &XmlNode) -> Option<String> {
    if let Some(tx) = child(n, "tx") {
        if let Some(rich) = child(tx, "rich") {
            return Some(text_body(rich));
        }
        if let Some(s) = cache_text(tx) {
            return Some(s);
        }
    }
    None
}

fn text_body(n: &XmlNode) -> String {
    let mut out = String::new();
    for p in n.children.iter().filter(|n| a_is(n, "p")) {
        for r in &p.children {
            if a_is(r, "r") {
                if let Some(t) = a_child(r, "t") {
                    out.push_str(&t.text);
                }
            } else if a_is(r, "br") {
                out.push('\n');
            }
        }
    }
    out
}

/// Theme colours are resolved by the caller; this keeps the import used when
/// no series has an explicit colour.
#[allow(dead_code)]
fn default_color(i: usize) -> Rgb {
    const PALETTE: [u32; 6] = [0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47];
    Rgb(PALETTE[i % PALETTE.len()])
}
