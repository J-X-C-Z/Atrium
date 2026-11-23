//! Read a DrawingML chart from its cached values.

use crate::look::{Chart, ChartKind, Series};
use crate::slides_theme::{A, AS, ColorMap, Theme, attr, color};
use crate::xml::{XmlNode, XmlPart};

const C: &str = "http://schemas.openxmlformats.org/drawingml/2006/chart";
const CS: &str = "http://purl.oclc.org/ooxml/drawingml/chart";

fn n<'a>(node: &'a XmlNode, ns: &str, name: &str) -> Option<&'a XmlNode> {
    node.children
        .iter()
        .find(|c| c.is(ns, name) || c.is(CS, name))
}

pub(crate) fn read(part: &XmlPart, theme: &Theme, cmap: &ColorMap) -> Chart {
    let root = part.root();
    let chart = n(root, C, "chart");
    let Some(chart) = chart else {
        return Chart::default();
    };
    let mut out = Chart {
        title: chart
            .children
            .iter()
            .find(|c| c.is(C, "title") || c.is(CS, "title"))
            .map(title)
            .filter(|t| !t.trim().is_empty()),
        ..Chart::default()
    };
    if let Some(legend) = n(chart, C, "legend") {
        let deleted = attr(legend, "delete").is_some_and(|v| matches!(v, "1" | "true"))
            || n(legend, C, "delete")
                .and_then(|d| attr(d, "val"))
                .is_some_and(|v| matches!(v, "1" | "true"));
        out.legend = !deleted;
    }
    let Some(plot) = n(chart, C, "plotArea") else {
        return out;
    };
    for kind in &plot.children {
        match kind.local_name.as_str() {
            "barChart" => {
                out.kind = match n(kind, C, "barDir").and_then(|d| attr(d, "val")) {
                    Some("bar") => ChartKind::Bar,
                    _ => ChartKind::Column,
                };
                grouping(&mut out, kind);
                out.series = series(kind, theme, cmap, false);
                out.categories = categories(kind);
                out.data_labels = data_labels(kind);
                return out;
            }
            "lineChart" => {
                out.kind = ChartKind::Line;
                grouping(&mut out, kind);
                out.series = series(kind, theme, cmap, false);
                out.categories = categories(kind);
                out.data_labels = data_labels(kind);
                return out;
            }
            "areaChart" => {
                out.kind = ChartKind::Area;
                grouping(&mut out, kind);
                out.series = series(kind, theme, cmap, false);
                out.categories = categories(kind);
                out.data_labels = data_labels(kind);
                return out;
            }
            "pieChart" => {
                out.kind = ChartKind::Pie;
                out.series = series(kind, theme, cmap, false);
                out.categories = categories(kind);
                out.data_labels = data_labels(kind);
                return out;
            }
            "doughnutChart" => {
                out.kind = ChartKind::Doughnut;
                out.series = series(kind, theme, cmap, false);
                out.categories = categories(kind);
                out.data_labels = data_labels(kind);
                return out;
            }
            "scatterChart" => {
                out.kind = ChartKind::Scatter;
                out.series = series(kind, theme, cmap, true);
                out.categories = categories(kind);
                out.data_labels = data_labels(kind);
                return out;
            }
            other => {
                if other.ends_with("Chart") {
                    out.kind = ChartKind::Other(other.to_owned());
                    return out;
                }
            }
        }
    }
    out
}

fn title(n: &XmlNode) -> String {
    let mut text = String::new();
    fn walk(n: &XmlNode, out: &mut String) {
        if n.local_name == "t" {
            out.push_str(&n.text);
        }
        for child in &n.children {
            walk(child, out);
        }
    }
    walk(n, &mut text);
    text
}

fn grouping(out: &mut Chart, chart: &XmlNode) {
    match n(chart, C, "grouping").and_then(|g| attr(g, "val")) {
        Some("stacked") => out.stacked = true,
        Some("percentStacked") => {
            out.stacked = true;
            out.percent = true;
        }
        _ => {}
    }
}

fn categories(node: &XmlNode) -> Vec<String> {
    let Some(cat) = n(node, C, "cat") else {
        return Vec::new();
    };
    for kind in ["strRef", "numRef", "multiLvlStrRef"] {
        if let Some(reference) = n(cat, C, kind) {
            let cache = n(reference, C, "strCache").or_else(|| n(reference, C, "numCache"));
            if let Some(cache) = cache {
                return points(cache);
            }
            if kind == "multiLvlStrRef"
                && let Some(cache) = n(reference, C, "multiLvlStrCache")
            {
                return points(cache);
            }
        }
    }
    Vec::new()
}

fn points(cache: &XmlNode) -> Vec<String> {
    let mut out: Vec<(usize, String)> = cache
        .children
        .iter()
        .filter(|n| n.is(C, "pt") || n.is(CS, "pt"))
        .filter_map(|pt| {
            let idx = attr(pt, "idx")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let value = pt
                .children
                .iter()
                .find(|c| c.local_name == "v")
                .map(|v| v.text.clone())?;
            Some((idx, value))
        })
        .collect();
    out.sort_by_key(|(idx, _)| *idx);
    if out.is_empty() {
        return Vec::new();
    }
    let mut values = vec![String::new(); out.last().map_or(0, |(i, _)| i + 1)];
    for (idx, value) in out {
        values[idx] = value;
    }
    values
}

fn numbers(cache: &XmlNode) -> Vec<Option<f64>> {
    let mut out: Vec<(usize, Option<f64>)> = cache
        .children
        .iter()
        .filter(|n| n.is(C, "pt") || n.is(CS, "pt"))
        .map(|pt| {
            let idx = attr(pt, "idx")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let value = pt
                .children
                .iter()
                .find(|c| c.local_name == "v")
                .and_then(|v| v.text.trim().parse::<f64>().ok());
            (idx, value)
        })
        .collect();
    if out.is_empty() {
        // A cache without point indexes is one value per child.
        out = cache
            .children
            .iter()
            .filter(|n| n.is(C, "pt") || n.is(CS, "pt"))
            .enumerate()
            .map(|(i, _)| (i, None))
            .collect();
    }
    if out.is_empty() {
        return Vec::new();
    }
    out.sort_by_key(|(idx, _)| *idx);
    let mut values = vec![None; out.last().map_or(0, |(i, _)| i + 1)];
    for (idx, value) in out {
        values[idx] = value;
    }
    values
}

fn series(n: &XmlNode, theme: &Theme, cmap: &ColorMap, scatter: bool) -> Vec<Series> {
    n.children
        .iter()
        .filter(|s| s.is(C, "ser") || s.is(CS, "ser"))
        .map(|s| {
            let name = s
                .children
                .iter()
                .find(|c| c.is(C, "tx") || c.is(CS, "tx"))
                .map(title)
                .unwrap_or_default();
            let x = if scatter {
                axis_values(s, "xVal")
            } else {
                Vec::new()
            };
            let values = if scatter {
                axis_values(s, "yVal")
            } else {
                axis_values(s, "val")
            };
            Series {
                name,
                values,
                x,
                color: s
                    .children
                    .iter()
                    .find(|c| c.is(C, "spPr") || c.is(CS, "spPr"))
                    .and_then(|p| {
                        p.children
                            .iter()
                            .find(|c| c.is(A, "solidFill") || c.is(AS, "solidFill"))
                    })
                    .and_then(|f| f.children.first())
                    .and_then(|c| color(c, theme, cmap, None))
                    .map(|(c, _)| c),
            }
        })
        .collect()
}

fn axis_values(ser: &XmlNode, name: &str) -> Vec<Option<f64>> {
    let Some(axis) = n(ser, C, name) else {
        return Vec::new();
    };
    for reference in ["numRef", "numLit"] {
        if let Some(refn) = n(axis, C, reference)
            && let Some(cache) = n(refn, C, "numCache").or(Some(refn))
        {
            return numbers(cache);
        }
    }
    Vec::new()
}

fn data_labels(node: &XmlNode) -> bool {
    let Some(labels) = n(node, C, "dLbls") else {
        return false;
    };
    if let Some(delete) = n(labels, C, "delete")
        && matches!(attr(delete, "val"), Some("1" | "true"))
    {
        return false;
    }
    n(labels, C, "showVal")
        .and_then(|v| attr(v, "val"))
        .is_none_or(|v| matches!(v, "1" | "true"))
}
