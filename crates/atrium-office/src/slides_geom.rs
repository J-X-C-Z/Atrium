//! Preset DrawingML outlines, from ECMA-376's `presetShapeDefinitions.xml`.
//!
//! Coordinates are twips inside the shape's frame. Curves use the standard
//! cubic approximation where the exact formula is a circle or ellipse.

use crate::look::PathCmd;

const K: f64 = 0.552_284_749_8;

fn gd(av: &[(String, i32)], name: &str, default: i32) -> f64 {
    av.iter()
        .find(|(n, _)| n == name)
        .map_or(default, |(_, v)| *v) as f64
        / 100_000.0
}

fn p(v: f64) -> i32 {
    v.round() as i32
}

pub(crate) fn preset(name: &str, w: i32, h: i32, av: &[(String, i32)]) -> Vec<PathCmd> {
    let (wf, hf) = (w as f64, h as f64);
    let ss = wf.min(hf);
    let half = |a: f64, b: f64| (a + b) / 2.0;
    match name {
        "rect" => vec![
            PathCmd::Move(0, 0),
            PathCmd::Line(w, 0),
            PathCmd::Line(w, h),
            PathCmd::Line(0, h),
            PathCmd::Close,
        ],
        "roundRect" => {
            let r = (ss * gd(av, "adj", 16667)).min(ss / 2.0);
            let (c, k) = (r * (1.0 - K), r * K);
            vec![
                PathCmd::Move(p(r), 0),
                PathCmd::Line(p(wf - r), 0),
                PathCmd::Cubic(p(wf - c), 0, w, p(k), w, p(r)),
                PathCmd::Line(w, p(hf - r)),
                PathCmd::Cubic(w, p(hf - c), p(wf - k), h, p(wf - r), h),
                PathCmd::Line(p(r), h),
                PathCmd::Cubic(p(c), h, 0, p(hf - k), 0, p(hf - r)),
                PathCmd::Line(0, p(r)),
                PathCmd::Cubic(0, p(c), p(k), 0, p(r), 0),
                PathCmd::Close,
            ]
        }
        "ellipse" => ellipse(0.0, 0.0, wf, hf, true),
        "triangle" => vec![
            PathCmd::Move(p(wf / 2.0), 0),
            PathCmd::Line(w, h),
            PathCmd::Line(0, h),
            PathCmd::Close,
        ],
        "rtTriangle" => vec![
            PathCmd::Move(0, 0),
            PathCmd::Line(w, h),
            PathCmd::Line(0, h),
            PathCmd::Close,
        ],
        "diamond" => vec![
            PathCmd::Move(p(wf / 2.0), 0),
            PathCmd::Line(w, p(hf / 2.0)),
            PathCmd::Line(p(wf / 2.0), h),
            PathCmd::Line(0, p(hf / 2.0)),
            PathCmd::Close,
        ],
        "parallelogram" => {
            let x = ss * gd(av, "adj", 25000);
            vec![
                PathCmd::Move(p(x), 0),
                PathCmd::Line(w, 0),
                PathCmd::Line(p(wf - x), h),
                PathCmd::Line(0, h),
                PathCmd::Close,
            ]
        }
        "trapezoid" => {
            let x = ss * gd(av, "adj", 25000);
            vec![
                PathCmd::Move(p(x), 0),
                PathCmd::Line(p(wf - x), 0),
                PathCmd::Line(w, h),
                PathCmd::Line(0, h),
                PathCmd::Close,
            ]
        }
        "pentagon" => vec![
            PathCmd::Move(p(wf / 2.0), 0),
            PathCmd::Line(w, p(hf * 0.62)),
            PathCmd::Line(p(wf * 0.82), h),
            PathCmd::Line(p(wf * 0.18), h),
            PathCmd::Line(0, p(hf * 0.62)),
            PathCmd::Close,
        ],
        "hexagon" => {
            let x = ss * gd(av, "adj", 25000);
            vec![
                PathCmd::Move(p(x), 0),
                PathCmd::Line(p(wf - x), 0),
                PathCmd::Line(w, p(hf / 2.0)),
                PathCmd::Line(p(wf - x), h),
                PathCmd::Line(p(x), h),
                PathCmd::Line(0, p(hf / 2.0)),
                PathCmd::Close,
            ]
        }
        "octagon" => {
            let x = ss * gd(av, "adj", 29289);
            let y = ss * gd(av, "adj", 29289);
            vec![
                PathCmd::Move(p(x), 0),
                PathCmd::Line(p(wf - x), 0),
                PathCmd::Line(w, p(y)),
                PathCmd::Line(w, p(hf - y)),
                PathCmd::Line(p(wf - x), h),
                PathCmd::Line(p(x), h),
                PathCmd::Line(0, p(hf - y)),
                PathCmd::Line(0, p(y)),
                PathCmd::Close,
            ]
        }
        "chevron" => {
            let x = (ss * gd(av, "adj", 50000)).min(wf / 2.0);
            vec![
                PathCmd::Move(0, 0),
                PathCmd::Line(p(wf - x), 0),
                PathCmd::Line(w, p(hf / 2.0)),
                PathCmd::Line(p(wf - x), h),
                PathCmd::Line(0, h),
                PathCmd::Line(p(x), p(hf / 2.0)),
                PathCmd::Close,
            ]
        }
        "homePlate" => {
            let x = (ss * gd(av, "adj", 50000)).min(wf);
            vec![
                PathCmd::Move(0, 0),
                PathCmd::Line(p(wf - x), 0),
                PathCmd::Line(w, p(hf / 2.0)),
                PathCmd::Line(p(wf - x), h),
                PathCmd::Line(0, h),
                PathCmd::Close,
            ]
        }
        "rightArrow" => {
            let body = hf * gd(av, "adj1", 50000);
            let head = (ss * gd(av, "adj2", 50000)).min(wf);
            let (y0, y1) = ((hf - body) / 2.0, (hf + body) / 2.0);
            vec![
                PathCmd::Move(0, p(y0)),
                PathCmd::Line(p(wf - head), p(y0)),
                PathCmd::Line(p(wf - head), 0),
                PathCmd::Line(w, p(hf / 2.0)),
                PathCmd::Line(p(wf - head), h),
                PathCmd::Line(p(wf - head), p(y1)),
                PathCmd::Line(0, p(y1)),
                PathCmd::Close,
            ]
        }
        "leftArrow" => {
            let body = hf * gd(av, "adj1", 50000);
            let head = (ss * gd(av, "adj2", 50000)).min(wf);
            let (y0, y1) = ((hf - body) / 2.0, (hf + body) / 2.0);
            vec![
                PathCmd::Move(w, p(y0)),
                PathCmd::Line(p(head), p(y0)),
                PathCmd::Line(p(head), 0),
                PathCmd::Line(0, p(hf / 2.0)),
                PathCmd::Line(p(head), h),
                PathCmd::Line(p(head), p(y1)),
                PathCmd::Line(w, p(y1)),
                PathCmd::Close,
            ]
        }
        "upArrow" => {
            let body = wf * gd(av, "adj1", 50000);
            let head = (ss * gd(av, "adj2", 50000)).min(hf);
            let (x0, x1) = ((wf - body) / 2.0, (wf + body) / 2.0);
            vec![
                PathCmd::Move(p(x0), h),
                PathCmd::Line(p(x0), p(head)),
                PathCmd::Line(0, p(head)),
                PathCmd::Line(p(wf / 2.0), 0),
                PathCmd::Line(w, p(head)),
                PathCmd::Line(p(x1), p(head)),
                PathCmd::Line(p(x1), h),
                PathCmd::Close,
            ]
        }
        "downArrow" => {
            let body = wf * gd(av, "adj1", 50000);
            let head = (ss * gd(av, "adj2", 50000)).min(hf);
            let (x0, x1) = ((wf - body) / 2.0, (wf + body) / 2.0);
            vec![
                PathCmd::Move(p(x0), 0),
                PathCmd::Line(p(x0), p(hf - head)),
                PathCmd::Line(0, p(hf - head)),
                PathCmd::Line(p(wf / 2.0), h),
                PathCmd::Line(w, p(hf - head)),
                PathCmd::Line(p(x1), p(hf - head)),
                PathCmd::Line(p(x1), 0),
                PathCmd::Close,
            ]
        }
        "leftRightArrow" => {
            let body = hf * gd(av, "adj1", 50000);
            let head = (ss * gd(av, "adj2", 50000)).min(wf / 2.0);
            let (y0, y1) = ((hf - body) / 2.0, (hf + body) / 2.0);
            vec![
                PathCmd::Move(0, p(hf / 2.0)),
                PathCmd::Line(p(head), 0),
                PathCmd::Line(p(head), p(y0)),
                PathCmd::Line(p(wf - head), p(y0)),
                PathCmd::Line(p(wf - head), 0),
                PathCmd::Line(w, p(hf / 2.0)),
                PathCmd::Line(p(wf - head), h),
                PathCmd::Line(p(wf - head), p(y1)),
                PathCmd::Line(p(head), p(y1)),
                PathCmd::Line(p(head), h),
                PathCmd::Close,
            ]
        }
        "wedgeRectCallout" => callout(w, h, av, false),
        "wedgeRoundRectCallout" => callout(w, h, av, true),
        "wedgeEllipseCallout" => {
            let mut path = ellipse(0.0, 0.0, wf, hf, false);
            let (tx, ty) = tail(wf, hf, av);
            path.push(PathCmd::Move(p(half(wf, tx)), h));
            path.push(PathCmd::Line(p(tx), p(ty)));
            path.push(PathCmd::Line(p(half(wf, tx)), h));
            path
        }
        "cloud" => cloud(w, h),
        "star5" => star(w, h, 5, gd(av, "adj", 19098)),
        "plus" => {
            let t = ss * gd(av, "adj", 25000);
            let (x0, x1) = ((wf - t) / 2.0, (wf + t) / 2.0);
            let (y0, y1) = ((hf - t) / 2.0, (hf + t) / 2.0);
            vec![
                PathCmd::Move(p(x0), 0),
                PathCmd::Line(p(x1), 0),
                PathCmd::Line(p(x1), p(y0)),
                PathCmd::Line(w, p(y0)),
                PathCmd::Line(w, p(y1)),
                PathCmd::Line(p(x1), p(y1)),
                PathCmd::Line(p(x1), h),
                PathCmd::Line(p(x0), h),
                PathCmd::Line(p(x0), p(y1)),
                PathCmd::Line(0, p(y1)),
                PathCmd::Line(0, p(y0)),
                PathCmd::Line(p(x0), p(y0)),
                PathCmd::Close,
            ]
        }
        "line" | "straightConnector1" => vec![PathCmd::Move(0, 0), PathCmd::Line(w, h)],
        "bentConnector2" => vec![
            PathCmd::Move(0, 0),
            PathCmd::Line(w, 0),
            PathCmd::Line(w, h),
        ],
        "bentConnector3" => vec![
            PathCmd::Move(0, 0),
            PathCmd::Line(p(wf / 2.0), 0),
            PathCmd::Line(p(wf / 2.0), h),
            PathCmd::Line(w, h),
        ],
        "curvedConnector3" => vec![
            PathCmd::Move(0, 0),
            PathCmd::Cubic(p(wf / 2.0), 0, p(wf / 2.0), h, w, h),
        ],
        _ => vec![
            PathCmd::Move(0, 0),
            PathCmd::Line(w, 0),
            PathCmd::Line(w, h),
            PathCmd::Line(0, h),
            PathCmd::Close,
        ],
    }
}

fn ellipse(x: f64, y: f64, w: f64, h: f64, close: bool) -> Vec<PathCmd> {
    let rx = w / 2.0;
    let ry = h / 2.0;
    let cx = x + rx;
    let cy = y + ry;
    let (kx, ky) = (rx * K, ry * K);
    let mut out = vec![
        PathCmd::Move(p(cx), p(y)),
        PathCmd::Cubic(p(cx + kx), p(y), p(x + w), p(cy - ky), p(x + w), p(cy)),
        PathCmd::Cubic(p(x + w), p(cy + ky), p(cx + kx), p(y + h), p(cx), p(y + h)),
        PathCmd::Cubic(p(cx - kx), p(y + h), p(x), p(cy + ky), p(x), p(cy)),
        PathCmd::Cubic(p(x), p(cy - ky), p(cx - kx), p(y), p(cx), p(y)),
    ];
    if close {
        out.push(PathCmd::Close);
    }
    out
}

fn tail(w: f64, h: f64, av: &[(String, i32)]) -> (f64, f64) {
    let a1 = gd(av, "adj1", -20833);
    let a2 = gd(av, "adj2", 62500);
    (
        (w * (0.5 + a1)).clamp(0.0, w),
        (h * (0.5 + a2)).clamp(0.0, h),
    )
}

fn callout(w: i32, h: i32, av: &[(String, i32)], round: bool) -> Vec<PathCmd> {
    let (wf, hf) = (w as f64, h as f64);
    let ss = wf.min(hf);
    let (tx, ty) = tail(wf, hf, av);
    let mut path = if round {
        let r = ss * 0.16;
        let (c, k) = (r * (1.0 - K), r * K);
        vec![
            PathCmd::Move(p(r), 0),
            PathCmd::Line(p(wf - r), 0),
            PathCmd::Cubic(p(wf - c), 0, w, p(k), w, p(r)),
            PathCmd::Line(w, p(hf - r)),
            PathCmd::Cubic(w, p(hf - c), p(wf - k), h, p(wf - r), h),
            PathCmd::Line(p(r), h),
            PathCmd::Cubic(p(c), h, 0, p(hf - k), 0, p(hf - r)),
            PathCmd::Line(0, p(r)),
            PathCmd::Cubic(0, p(c), p(k), 0, p(r), 0),
            PathCmd::Close,
        ]
    } else {
        vec![
            PathCmd::Move(0, 0),
            PathCmd::Line(w, 0),
            PathCmd::Line(w, h),
            PathCmd::Line(0, h),
            PathCmd::Close,
        ]
    };
    path.push(PathCmd::Move(p(tx - ss * 0.1), h));
    path.push(PathCmd::Line(p(tx), p(ty)));
    path.push(PathCmd::Line(p(tx + ss * 0.1), h));
    path
}

fn cloud(w: i32, h: i32) -> Vec<PathCmd> {
    let (wf, hf) = (w as f64, h as f64);
    let mut path = vec![PathCmd::Move(p(wf * 0.18), p(hf * 0.55))];
    for (x, y, cx, cy, ex, ey) in [
        (0.05, 0.42, 0.02, 0.25, 0.18, 0.20),
        (0.18, 0.20, 0.28, 0.02, 0.43, 0.08),
        (0.43, 0.08, 0.57, 0.00, 0.68, 0.12),
        (0.68, 0.12, 0.85, 0.05, 0.92, 0.25),
        (0.92, 0.25, 0.99, 0.45, 0.95, 0.58),
        (0.95, 0.58, 0.97, 0.78, 0.82, 0.88),
        (0.82, 0.88, 0.66, 1.00, 0.50, 0.92),
        (0.50, 0.92, 0.36, 1.00, 0.22, 0.90),
        (0.22, 0.90, 0.06, 0.82, 0.04, 0.65),
        (0.04, 0.65, 0.02, 0.60, 0.18, 0.55),
    ] {
        path.push(PathCmd::Cubic(
            p(wf * x),
            p(hf * y),
            p(wf * cx),
            p(hf * cy),
            p(wf * ex),
            p(hf * ey),
        ));
    }
    path.push(PathCmd::Close);
    normalize(path, w, h)
}

fn normalize(path: Vec<PathCmd>, w: i32, h: i32) -> Vec<PathCmd> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for cmd in &path {
        for (x, y) in match *cmd {
            PathCmd::Move(x, y) | PathCmd::Line(x, y) => vec![(x, y)],
            PathCmd::Quad(a, b, c, d) => vec![(a, b), (c, d)],
            PathCmd::Cubic(a, b, c, d, e, f) => vec![(a, b), (c, d), (e, f)],
            PathCmd::Close => Vec::new(),
        } {
            min_x = min_x.min(x as f64);
            min_y = min_y.min(y as f64);
            max_x = max_x.max(x as f64);
            max_y = max_y.max(y as f64);
        }
    }
    let sx = if max_x > min_x {
        w as f64 / (max_x - min_x)
    } else {
        1.0
    };
    let sy = if max_y > min_y {
        h as f64 / (max_y - min_y)
    } else {
        1.0
    };
    let map = |x: i32, y: i32| {
        (
            ((x as f64 - min_x) * sx).round() as i32,
            ((y as f64 - min_y) * sy).round() as i32,
        )
    };
    path.into_iter()
        .map(|cmd| match cmd {
            PathCmd::Move(x, y) => {
                let (x, y) = map(x, y);
                PathCmd::Move(x, y)
            }
            PathCmd::Line(x, y) => {
                let (x, y) = map(x, y);
                PathCmd::Line(x, y)
            }
            PathCmd::Quad(a, b, c, d) => {
                let (a, b) = map(a, b);
                let (c, d) = map(c, d);
                PathCmd::Quad(a, b, c, d)
            }
            PathCmd::Cubic(a, b, c, d, e, f) => {
                let (a, b) = map(a, b);
                let (c, d) = map(c, d);
                let (e, f) = map(e, f);
                PathCmd::Cubic(a, b, c, d, e, f)
            }
            PathCmd::Close => PathCmd::Close,
        })
        .collect()
}

fn star(w: i32, h: i32, points: usize, inner: f64) -> Vec<PathCmd> {
    let (wf, hf) = (w as f64, h as f64);
    let mut pts = Vec::new();
    for k in 0..points * 2 {
        let angle = -std::f64::consts::FRAC_PI_2 + k as f64 * std::f64::consts::PI / points as f64;
        let radius = if k % 2 == 0 { 1.0 } else { inner.max(0.05) };
        pts.push((
            wf / 2.0 + angle.cos() * wf / 2.0 * radius,
            hf / 2.0 + angle.sin() * hf / 2.0 * radius,
        ));
    }
    // Stretch the star's bounding box onto the frame, as PowerPoint does.
    let min_x = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let max_x = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let min_y = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let max_y = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let scale_x = if max_x > min_x {
        wf / (max_x - min_x)
    } else {
        1.0
    };
    let scale_y = if max_y > min_y {
        hf / (max_y - min_y)
    } else {
        1.0
    };
    let mut out = Vec::new();
    for (k, (x, y)) in pts.into_iter().enumerate() {
        let x = ((x - min_x) * scale_x).round() as i32;
        let y = ((y - min_y) * scale_y).round() as i32;
        if k == 0 {
            out.push(PathCmd::Move(x, y));
        } else {
            out.push(PathCmd::Line(x, y));
        }
    }
    out.push(PathCmd::Close);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRESETS: &[&str] = &[
        "rect",
        "roundRect",
        "ellipse",
        "triangle",
        "rtTriangle",
        "diamond",
        "parallelogram",
        "trapezoid",
        "pentagon",
        "hexagon",
        "octagon",
        "chevron",
        "homePlate",
        "rightArrow",
        "leftArrow",
        "upArrow",
        "downArrow",
        "leftRightArrow",
        "wedgeRectCallout",
        "wedgeRoundRectCallout",
        "wedgeEllipseCallout",
        "cloud",
        "star5",
        "plus",
        "line",
        "straightConnector1",
        "bentConnector2",
        "bentConnector3",
        "curvedConnector3",
    ];

    fn bounds(path: &[PathCmd]) -> (i32, i32, i32, i32) {
        let mut out = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for cmd in path {
            for (x, y) in match *cmd {
                PathCmd::Move(x, y) | PathCmd::Line(x, y) => vec![(x, y)],
                PathCmd::Quad(a, b, c, d) => vec![(a, b), (c, d)],
                PathCmd::Cubic(a, b, c, d, e, f) => vec![(a, b), (c, d), (e, f)],
                PathCmd::Close => vec![],
            } {
                out.0 = out.0.min(x);
                out.1 = out.1.min(y);
                out.2 = out.2.max(x);
                out.3 = out.3.max(y);
            }
        }
        out
    }

    #[test]
    fn every_preset_fills_its_frame_and_adjustments_move_points() {
        for preset in PRESETS {
            let path = self::preset(preset, 200, 120, &[]);
            let (x0, y0, x1, y1) = bounds(&path);
            assert_eq!((x0, y0, x1, y1), (0, 0, 200, 120), "{preset}");
        }
        let square = self::preset("roundRect", 200, 120, &[("adj".into(), 50000)]);
        let small = self::preset("roundRect", 200, 120, &[("adj".into(), 10000)]);
        assert_ne!(square[0], small[0]);
    }
}
