//! Draws a chart from its cached values: axes, ticks, gridlines, bars,
//! lines, areas, pie wedges, scatter points and the legend.

use crate::{FontKey, Item, Pen, Rect, Seg, Shaper};
use atrium_office::look::{Chart, ChartKind, Rgb};

const PALETTE: [u32; 8] = [
    0x4472C4, 0xED7D31, 0xA5A5A5, 0xFFC000, 0x5B9BD5, 0x70AD47, 0x264478, 0x9E480E,
];
const AXIS: Rgb = Rgb(0x7F7F7F);
const TEXT: Rgb = Rgb(0x404040);

fn color(index: usize, chart: &Chart, series: usize) -> Rgb {
    chart
        .series
        .get(series)
        .and_then(|s| s.color)
        .unwrap_or(Rgb(PALETTE[index % PALETTE.len()]))
}

fn text(x: f32, baseline: f32, s: impl Into<String>, size: f32, color: Rgb) -> Item {
    Item::Text {
        x,
        baseline,
        text: s.into(),
        font: FontKey {
            family: String::new(),
            size,
            bold: false,
            italic: false,
            cjk: true,
        },
        color,
        rotation: 0.0,
        center: (0.0, 0.0),
    }
}

fn line(from: (f32, f32), to: (f32, f32), width: f32, color: Rgb) -> Item {
    Item::Rule {
        from,
        to,
        width,
        color,
    }
}

fn path(segments: Vec<Seg>, fill: Option<Rgb>, stroke: Option<Pen>) -> Item {
    Item::Path {
        segments,
        fill: fill.map(|c| (c, 255)),
        stroke,
        rotation: 0.0,
        center: (0.0, 0.0),
    }
}

fn width(text: &str, shaper: &dyn Shaper, size: f32) -> f32 {
    let font = FontKey {
        family: String::new(),
        size,
        bold: false,
        italic: false,
        cjk: true,
    };
    shaper.shape(text, &font).iter().map(|c| c.advance).sum()
}

fn nice_step(range: f64, ticks: usize) -> f64 {
    if range <= 0.0 {
        return 1.0;
    }
    let rough = range / ticks.max(1) as f64;
    let power = 10f64.powf(rough.log10().floor());
    let fraction = rough / power;
    let nice = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    (nice * power).max(1e-9)
}

fn bounds(chart: &Chart) -> (f64, f64) {
    if chart.percent {
        return (0.0, 1.0);
    }
    let mut min = 0.0f64;
    let mut max = 0.0f64;
    if chart.stacked {
        let count = chart
            .series
            .iter()
            .map(|s| s.values.len())
            .max()
            .unwrap_or(0);
        for i in 0..count {
            let mut positive = 0.0;
            let mut negative = 0.0;
            for s in &chart.series {
                if let Some(Some(v)) = s.values.get(i) {
                    if *v >= 0.0 {
                        positive += *v;
                    } else {
                        negative += *v;
                    }
                }
            }
            max = max.max(positive);
            min = min.min(negative);
        }
    } else {
        for v in chart.series.iter().flat_map(|s| s.values.iter().flatten()) {
            max = max.max(*v);
            min = min.min(*v);
        }
    }
    if max == min {
        max += 1.0;
    }
    {
        let pad = (max - min) * 0.05;
        (min - pad, max + pad)
    }
}

fn y_of(v: f64, min: f64, max: f64, plot: Rect) -> f32 {
    plot.y + plot.height - ((v - min) / (max - min)) as f32 * plot.height
}

pub(crate) fn draw(c: &Chart, rect: Rect, shaper: &dyn Shaper) -> Vec<Item> {
    let mut out = Vec::new();
    let mut top = rect.y + 2.0;
    if let Some(title) = &c.title {
        let w = width(title, shaper, 12.0);
        out.push(text(
            rect.x + (rect.width - w) / 2.0,
            top + 11.0,
            title.clone(),
            12.0,
            TEXT,
        ));
        top += 18.0;
    }
    let legend_width = if c.legend && !c.series.is_empty() {
        let longest = c
            .series
            .iter()
            .map(|s| width(&s.name, shaper, 9.0))
            .fold(0.0f32, f32::max);
        (longest + 18.0).min(rect.width * 0.35).max(20.0)
    } else {
        0.0
    };
    let plot = Rect {
        x: rect.x + 32.0,
        y: top + 4.0,
        width: (rect.width - 38.0 - legend_width).max(1.0),
        height: (rect.y + rect.height - top - 24.0).max(1.0),
    };
    let horizontal = matches!(c.kind, ChartKind::Bar);
    match &c.kind {
        ChartKind::Pie | ChartKind::Doughnut => {
            pie(c, plot, &mut out, shaper);
            legend(c, rect, top, legend_width, &mut out, shaper);
            return out;
        }
        ChartKind::Scatter => {
            scatter(c, plot, &mut out, shaper);
            axes(c, plot, &mut out, shaper);
        }
        ChartKind::Line | ChartKind::Area => line_series(c, plot, &mut out, shaper),
        ChartKind::Column | ChartKind::Bar => bars(c, plot, &mut out, shaper),
        ChartKind::Other(name) => {
            out.push(text(plot.x, plot.y + 14.0, name.clone(), 12.0, TEXT));
            legend(c, rect, top, legend_width, &mut out, shaper);
            return out;
        }
    }
    if horizontal {
        horizontal_axes(c, plot, &mut out, shaper);
    } else {
        axes(c, plot, &mut out, shaper);
    }
    legend(c, rect, top, legend_width, &mut out, shaper);
    out
}

fn horizontal_axes(c: &Chart, plot: Rect, out: &mut Vec<Item>, shaper: &dyn Shaper) {
    let (min, max) = bounds(c);
    let step = nice_step(max - min, 4);
    let start = (min / step).floor() * step;
    let mut v = start;
    while v <= max + step * 0.5 {
        let x = plot.x + ((v - min) / (max - min)) as f32 * plot.width;
        out.push(line(
            (x, plot.y),
            (x, plot.y + plot.height),
            0.5,
            Rgb(0xE0E0E0),
        ));
        let label = if c.percent {
            format!("{:.0}%", v * 100.0)
        } else {
            format!("{v:.0}")
        };
        let w = width(&label, shaper, 8.0);
        out.push(text(
            x - w / 2.0,
            plot.y + plot.height + 11.0,
            label,
            8.0,
            TEXT,
        ));
        v += step;
    }
    out.push(line(
        (plot.x, plot.y),
        (plot.x, plot.y + plot.height),
        1.0,
        AXIS,
    ));
    out.push(line(
        (plot.x, plot.y + plot.height),
        (plot.x + plot.width, plot.y + plot.height),
        1.0,
        AXIS,
    ));
    let categories = c
        .categories
        .len()
        .max(c.series.iter().map(|s| s.values.len()).max().unwrap_or(0));
    if categories > 0 {
        let slot = plot.height / categories as f32;
        for (i, label) in c.categories.iter().enumerate() {
            if i >= categories {
                break;
            }
            let w = width(label, shaper, 8.0);
            out.push(text(
                plot.x - w - 4.0,
                plot.y + slot * (i as f32 + 0.5) + 3.0,
                label.clone(),
                8.0,
                TEXT,
            ));
        }
    }
}

fn axes(c: &Chart, plot: Rect, out: &mut Vec<Item>, shaper: &dyn Shaper) {
    let (min, max) = bounds(c);
    let step = nice_step(max - min, 4);
    let start = (min / step).floor() * step;
    let mut v = start;
    while v <= max + step * 0.5 {
        let y = y_of(v, min, max, plot);
        out.push(line(
            (plot.x, y),
            (plot.x + plot.width, y),
            0.5,
            Rgb(0xE0E0E0),
        ));
        let label = if c.percent {
            format!("{:.0}%", v * 100.0)
        } else {
            format!("{v:.0}")
        };
        let w = width(&label, shaper, 8.0);
        out.push(text(plot.x - w - 4.0, y + 3.0, label, 8.0, TEXT));
        v += step;
    }
    out.push(line(
        (plot.x, plot.y),
        (plot.x, plot.y + plot.height),
        1.0,
        AXIS,
    ));
    out.push(line(
        (plot.x, plot.y + plot.height),
        (plot.x + plot.width, plot.y + plot.height),
        1.0,
        AXIS,
    ));
    let categories = c
        .categories
        .len()
        .max(c.series.iter().map(|s| s.values.len()).max().unwrap_or(0));
    if categories > 0 {
        let slot = plot.width / categories as f32;
        for (i, label) in c.categories.iter().enumerate() {
            if i >= categories {
                break;
            }
            let w = width(label, shaper, 8.0);
            out.push(text(
                plot.x + slot * (i as f32 + 0.5) - w / 2.0,
                plot.y + plot.height + 11.0,
                label.clone(),
                8.0,
                TEXT,
            ));
        }
    }
}

fn bars(c: &Chart, plot: Rect, out: &mut Vec<Item>, shaper: &dyn Shaper) {
    let chart = c;
    let (min, max) = bounds(c);
    let count = c
        .series
        .iter()
        .map(|s| s.values.len())
        .max()
        .unwrap_or(0)
        .max(1);
    let groups = c.series.len().max(1);
    let horizontal = matches!(c.kind, ChartKind::Bar);
    let slot = if horizontal {
        plot.height / count as f32
    } else {
        plot.width / count as f32
    };
    let bar = slot * 0.68 / groups as f32;
    for (si, s) in c.series.iter().enumerate() {
        let c = color(si, chart, si);
        let mut stack_pos = 0.0;
        let mut stack_neg = 0.0;
        for (k, v) in s.values.iter().enumerate() {
            let Some(v) = v else { continue };
            let mut value = *v;
            if chart.percent {
                let total: f64 = chart
                    .series
                    .iter()
                    .filter_map(|s| s.values.get(k).copied().flatten())
                    .sum::<f64>()
                    .max(f64::EPSILON);
                value /= total;
            }
            let (base, top) = if chart.stacked {
                if value >= 0.0 {
                    let b = stack_pos;
                    stack_pos += value;
                    (b, stack_pos)
                } else {
                    let b = stack_neg;
                    stack_neg += value;
                    (stack_neg, b)
                }
            } else {
                (0.0, value)
            };
            let offset = if chart.stacked { 0.0 } else { bar * si as f32 };
            let thickness = if chart.stacked {
                bar * groups as f32
            } else {
                bar
            };
            if horizontal {
                let x0 = plot.x;
                let x1 = plot.x + ((base - min) / (max - min)) as f32 * plot.width;
                let x2 = plot.x + ((top - min) / (max - min)) as f32 * plot.width;
                let y = plot.y + slot * k as f32 + slot * 0.16 + offset;
                out.push(Item::Fill {
                    rect: Rect {
                        x: x0.min(x1).min(x2),
                        y,
                        width: (x2 - x1).abs().max(1.0),
                        height: thickness,
                    },
                    color: c,
                });
            } else {
                let y1 = y_of(base, min, max, plot);
                let y2 = y_of(top, min, max, plot);
                let x = plot.x + slot * k as f32 + slot * 0.16 + offset;
                out.push(Item::Fill {
                    rect: Rect {
                        x,
                        y: y1.min(y2),
                        width: thickness,
                        height: (y2 - y1).abs().max(1.0),
                    },
                    color: c,
                });
            }
            if chart.data_labels {
                let label = if chart.percent {
                    format!("{:.0}%", value * 100.0)
                } else {
                    format!("{value:.0}")
                };
                let lw = width(&label, shaper, 8.0);
                let x = if horizontal {
                    plot.x + ((top - min) / (max - min)) as f32 * plot.width + 2.0
                } else {
                    plot.x + slot * (k as f32 + 0.5) - lw / 2.0
                };
                let y = if horizontal {
                    plot.y + slot * (k as f32 + 0.5) + 3.0
                } else {
                    y_of(top, min, max, plot) - 3.0
                };
                out.push(text(x, y, label, 8.0, TEXT));
            }
        }
    }
}

fn line_series(c: &Chart, plot: Rect, out: &mut Vec<Item>, _shaper: &dyn Shaper) {
    let chart = c;
    let (min, max) = bounds(c);
    let count = c
        .series
        .iter()
        .map(|s| s.values.len())
        .max()
        .unwrap_or(0)
        .max(1);
    let step = plot.width / count as f32;
    for (si, s) in c.series.iter().enumerate() {
        let c = color(si, chart, si);
        let mut points = Vec::new();
        for (k, v) in s.values.iter().enumerate() {
            let Some(v) = v else { continue };
            points.push((plot.x + step * (k as f32 + 0.5), y_of(*v, min, max, plot)));
        }
        if matches!(chart.kind, ChartKind::Area) {
            let mut segments = Vec::new();
            if let Some((x, _)) = points.first() {
                segments.push(Seg::Move(*x, plot.y + plot.height));
            }
            for (x, y) in &points {
                segments.push(Seg::Line(*x, *y));
            }
            if let Some((x, _)) = points.last() {
                segments.push(Seg::Line(*x, plot.y + plot.height));
                segments.push(Seg::Close);
            }
            out.push(path(segments, Some(c), None));
        }
        let mut segments = Vec::new();
        for (k, (x, y)) in points.iter().enumerate() {
            segments.push(if k == 0 {
                Seg::Move(*x, *y)
            } else {
                Seg::Line(*x, *y)
            });
            out.push(Item::Fill {
                rect: Rect {
                    x: x - 2.0,
                    y: y - 2.0,
                    width: 4.0,
                    height: 4.0,
                },
                color: c,
            });
        }
        out.push(path(
            segments,
            None,
            Some(Pen {
                width: 2.0,
                color: c,
                alpha: 255,
                dash: Default::default(),
            }),
        ));
    }
}

fn scatter(c: &Chart, plot: Rect, out: &mut Vec<Item>, _shaper: &dyn Shaper) {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for s in &c.series {
        for (x, y) in s.x.iter().zip(&s.values) {
            if let (Some(x), Some(y)) = (x, y) {
                min_x = min_x.min(*x);
                max_x = max_x.max(*x);
                min_y = min_y.min(*y);
                max_y = max_y.max(*y);
            }
        }
    }
    if !min_x.is_finite() {
        return;
    }
    if max_x == min_x {
        max_x += 1.0;
    }
    if max_y == min_y {
        max_y += 1.0;
    }
    for (si, s) in c.series.iter().enumerate() {
        let c = color(si, c, si);
        for (x, y) in s.x.iter().zip(&s.values) {
            let (Some(x), Some(y)) = (x, y) else { continue };
            let px = plot.x + ((x - min_x) / (max_x - min_x)) as f32 * plot.width;
            let py = plot.y + plot.height - ((y - min_y) / (max_y - min_y)) as f32 * plot.height;
            out.push(Item::Fill {
                rect: Rect {
                    x: px - 2.5,
                    y: py - 2.5,
                    width: 5.0,
                    height: 5.0,
                },
                color: c,
            });
        }
    }
}

fn pie(c: &Chart, plot: Rect, out: &mut Vec<Item>, _shaper: &dyn Shaper) {
    let Some(series) = c.series.first() else {
        return;
    };
    let values: Vec<f64> = series.values.iter().map(|v| v.unwrap_or(0.0)).collect();
    let total: f64 = values.iter().sum();
    if total <= 0.0 {
        return;
    }
    let radius = plot.width.min(plot.height) / 2.0;
    let cx = plot.x + plot.width / 2.0;
    let cy = plot.y + plot.height / 2.0;
    let inner = if matches!(c.kind, ChartKind::Doughnut) {
        radius * 0.55
    } else {
        0.0
    };
    let mut start = -90.0f32;
    for (i, value) in values.iter().enumerate() {
        if *value <= 0.0 {
            continue;
        }
        let sweep = (*value / total) as f32 * 360.0;
        let end = start + sweep;
        let c = color(i, c, 0);
        let mut segments = Vec::new();
        if inner <= 0.0 {
            segments.push(Seg::Move(cx, cy));
            segments.push(Seg::Line(
                cx + radius * cos(start),
                cy + radius * sin(start),
            ));
        } else {
            segments.push(Seg::Move(cx + inner * cos(start), cy + inner * sin(start)));
            segments.push(Seg::Line(
                cx + radius * cos(start),
                cy + radius * sin(start),
            ));
        }
        arc(&mut segments, cx, cy, radius, start, end);
        if inner <= 0.0 {
            segments.push(Seg::Close);
        } else {
            segments.push(Seg::Line(cx + inner * cos(end), cy + inner * sin(end)));
            arc(&mut segments, cx, cy, inner, end, start);
            segments.push(Seg::Close);
        }
        out.push(path(
            segments,
            Some(c),
            Some(Pen {
                width: 1.0,
                color: Rgb::WHITE,
                alpha: 255,
                dash: Default::default(),
            }),
        ));
        start = end;
    }
}

fn cos(degrees: f32) -> f32 {
    degrees.to_radians().cos()
}

fn sin(degrees: f32) -> f32 {
    degrees.to_radians().sin()
}

fn arc(segments: &mut Vec<Seg>, cx: f32, cy: f32, r: f32, start: f32, end: f32) {
    let sweep = end - start;
    let steps = ((sweep.abs() / 90.0).ceil() as usize).max(1);
    let step = sweep / steps as f32;
    for k in 0..steps {
        let a0 = start + step * k as f32;
        let a1 = a0 + step;
        let kappa = (step.abs().to_radians() / 2.0).tan() * 4.0 / 3.0;
        let p0 = (cx + r * cos(a0), cy + r * sin(a0));
        let p1 = (cx + r * cos(a1), cy + r * sin(a1));
        let c0 = (p0.0 - kappa * r * sin(a0), p0.1 + kappa * r * cos(a0));
        let c1 = (p1.0 + kappa * r * sin(a1), p1.1 - kappa * r * cos(a1));
        segments.push(Seg::Cubic(c0.0, c0.1, c1.0, c1.1, p1.0, p1.1));
    }
}

fn legend(c: &Chart, rect: Rect, top: f32, width: f32, out: &mut Vec<Item>, shaper: &dyn Shaper) {
    if !c.legend || width <= 0.0 {
        return;
    }
    let x = rect.x + rect.width - width + 4.0;
    let mut y = top + 8.0;
    for (i, s) in c.series.iter().enumerate() {
        out.push(Item::Fill {
            rect: Rect {
                x,
                y: y - 7.0,
                width: 8.0,
                height: 8.0,
            },
            color: color(i, c, i),
        });
        out.push(text(x + 12.0, y, s.name.clone(), 9.0, TEXT));
        y += 14.0;
        if y > rect.y + rect.height - 6.0 {
            break;
        }
    }
    let _ = shaper;
}
