//! Layout for the things drawn over a sheet: pictures, charts and shapes
//! placed from their cell anchors. The grid itself is painted by the UI from
//! `Workbook::shown`.

use atrium_office::Sheet;
use atrium_office::look::{Anchored, Drawing, SheetLook};

use crate::{Item, Rect, Shaper, chart, engine};

/// A column's width in points, zero for a hidden column.
pub fn column_width(look: &SheetLook, column: u32) -> f32 {
    if look.hidden_columns.contains(&column) {
        0.0
    } else {
        look.columns
            .get(&column)
            .copied()
            .unwrap_or(look.default_width)
            .max(0.0)
    }
}

/// A row's height in points, zero for a hidden row.
pub fn row_height(look: &SheetLook, row: u32) -> f32 {
    if look.hidden_rows.contains(&row) {
        0.0
    } else {
        look.rows
            .get(&row)
            .copied()
            .unwrap_or(look.default_height)
            .max(0.0)
    }
}

/// The top-left corner of a cell, from the sheet's top-left corner.
pub fn cell_origin(look: &SheetLook, row: u32, column: u32) -> (f32, f32) {
    let x = (0..column).map(|c| column_width(look, c)).sum::<f32>();
    let y = (0..row).map(|r| row_height(look, r)).sum::<f32>();
    (x, y)
}

/// The rectangle an anchor covers.
pub fn anchored_rect(look: &SheetLook, anchored: &Anchored) -> Rect {
    let (fx, fy) = cell_origin(look, anchored.from.0, anchored.from.1);
    let x = fx + anchored.from.2;
    let y = fy + anchored.from.3;
    let (width, height) = if let Some((w, h)) = anchored.ext {
        (w, h)
    } else {
        let (tx, ty) = cell_origin(look, anchored.to.0, anchored.to.1);
        (tx + anchored.to.2 - x, ty + anchored.to.3 - y)
    };
    Rect {
        x,
        y,
        width: width.max(0.0),
        height: height.max(0.0),
    }
}

/// Items for every drawing over `sheet`, in the file's order, so the grid can
/// paint them over the cells. Pictures are placed from their anchors.
pub fn layout_sheet_drawings(sheet: &Sheet, shaper: &dyn Shaper) -> Vec<Item> {
    let mut items = Vec::new();
    for anchored in &sheet.look.drawings {
        let rect = anchored_rect(&sheet.look, anchored);
        match &anchored.what {
            Drawing::Picture(path) => items.push(Item::Picture {
                rect,
                path: (!path.as_os_str().is_empty()).then(|| path.clone()),
            }),
            Drawing::Chart(c) => items.extend(chart(c, rect, shaper)),
            Drawing::Shape(shape) => items.extend(engine::shape_at(shape, rect, shaper)),
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use atrium_office::look::{Anchored, Chart, ChartKind, Drawing, Series, ShapeLook};
    use atrium_office::{Para, ParaStyle, Shape};
    use std::path::PathBuf;

    #[test]
    fn anchors_use_column_widths_and_row_heights() {
        let mut look = SheetLook::default();
        look.columns.insert(0, 50.0);
        look.columns.insert(1, 100.0);
        look.rows.insert(0, 20.0);
        look.rows.insert(1, 30.0);
        look.hidden_columns.insert(2);
        look.hidden_rows.insert(3);
        let mut sheet = Sheet::new("drawings");
        sheet.look = look;
        sheet.look.drawings = vec![
            Anchored {
                from: (0, 0, 5.0, 5.0),
                to: (1, 1, 0.0, 0.0),
                ext: None,
                what: Drawing::Picture(PathBuf::from("/tmp/a.png")),
            },
            Anchored {
                from: (1, 0, 0.0, 0.0),
                to: (1, 0, 0.0, 0.0),
                ext: Some((40.0, 20.0)),
                what: Drawing::Chart(Chart {
                    kind: ChartKind::Column,
                    series: vec![Series {
                        values: vec![Some(1.0)],
                        ..Series::default()
                    }],
                    ..Chart::default()
                }),
            },
            Anchored {
                from: (0, 0, 0.0, 0.0),
                to: (1, 1, 0.0, 0.0),
                ext: None,
                what: Drawing::Shape(Box::new(Shape {
                    id: 1,
                    frame: [0, 0, 10, 10],
                    paras: vec![Para::new(1, ParaStyle::Normal, "备注")],
                    look: ShapeLook::default(),
                    kind: atrium_office::look::ShapeKind::Text,
                })),
            },
        ];
        assert_eq!(column_width(&sheet.look, 0), 50.0);
        assert_eq!(column_width(&sheet.look, 2), 0.0);
        assert_eq!(row_height(&sheet.look, 3), 0.0);
        assert_eq!(cell_origin(&sheet.look, 1, 1), (50.0, 20.0));

        let items = layout_sheet_drawings(&sheet, &crate::FixedShaper);
        let picture = items
            .iter()
            .find_map(|item| match item {
                Item::Picture { rect, .. } => Some(*rect),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            picture,
            Rect {
                x: 5.0,
                y: 5.0,
                width: 45.0,
                height: 15.0
            }
        );
        assert!(items.iter().any(|item| matches!(
            item,
            Item::Fill { rect, .. } if rect.y >= 20.0 && rect.x < 50.0
        )));
        let text: String = items
            .iter()
            .filter_map(|item| match item {
                Item::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "备注");
    }

    #[test]
    fn a_one_cell_anchor_uses_its_extent() {
        let mut look = SheetLook::default();
        look.columns.insert(0, 20.0);
        look.rows.insert(0, 10.0);
        let anchored = Anchored {
            from: (0, 0, 0.0, 0.0),
            to: (0, 0, 0.0, 0.0),
            ext: Some((30.0, 12.0)),
            what: Drawing::Picture(PathBuf::from("")),
        };
        assert_eq!(
            anchored_rect(&look, &anchored),
            Rect {
                x: 0.0,
                y: 0.0,
                width: 30.0,
                height: 12.0
            }
        );
    }
}
