//! A workbook's sheet drawn as Excel draws it: column widths and row
//! heights, frozen panes, fills, borders, fonts and alignment from each
//! cell's style, edited in place. Only the cells on screen are drawn.

use std::cell::Cell as StdCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use atrium_layout::FontKey;
use atrium_office::look::{Border, HAlign, Rgb, VAlign};
use atrium_office::{Office, Op, SheetOp, Value, Workbook, a1};
use ely_gpui_component::{
    forms::{Input, InputEvent, TextInput},
    theme::{ActiveTheme, ControlSize, Radius, TextSize},
};
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, Entity, FocusHandle, Focusable, Hsla,
    InteractiveElement, IntoElement, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement, Pixels, ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled,
    Subscription, TextAlign, Window, actions, canvas, div, point, prelude::*, px, rgb, size,
};

use super::document::{DocEvent, DocView};
use super::pages::GpuiShaper;
use crate::i18n::t;

actions!(
    doc_grid,
    [
        GridLeft,
        GridRight,
        GridUp,
        GridDown,
        GridSelectLeft,
        GridSelectRight,
        GridSelectUp,
        GridSelectDown,
        GridHome,
        GridTop,
        GridPageUp,
        GridPageDown,
        GridNext,
        GridPrevious,
        GridEnter,
        GridEnterUp,
        GridEdit,
        GridCancel,
        GridClear,
        GridSelectAll,
        GridUndo,
        GridRedo,
        GridCopy,
        GridCut,
        GridPaste,
    ]
);

const CONTEXT: &str = "DocGrid";
/// Points to pixels at 100 %: 96 dpi.
const PX_PER_PT: f32 = 96.0 / 72.0;
const ROW_HEADER: f32 = 36.0;
const COL_HEADER: f32 = 18.0;
/// Rows and columns past the last used one that can be scrolled to.
const SPARE_ROWS: u32 = 200;
const SPARE_COLS: u32 = 30;
const MAX_ROW: u32 = 1_048_575;
const MAX_COL: u32 = 16_383;

pub fn bind_keys(cx: &mut App) {
    let cmd = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let c = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("left", GridLeft, c),
        KeyBinding::new("right", GridRight, c),
        KeyBinding::new("up", GridUp, c),
        KeyBinding::new("down", GridDown, c),
        KeyBinding::new("shift-left", GridSelectLeft, c),
        KeyBinding::new("shift-right", GridSelectRight, c),
        KeyBinding::new("shift-up", GridSelectUp, c),
        KeyBinding::new("shift-down", GridSelectDown, c),
        KeyBinding::new("home", GridHome, c),
        KeyBinding::new(&format!("{cmd}-home"), GridTop, c),
        KeyBinding::new("pageup", GridPageUp, c),
        KeyBinding::new("pagedown", GridPageDown, c),
        KeyBinding::new("tab", GridNext, c),
        KeyBinding::new("shift-tab", GridPrevious, c),
        KeyBinding::new("enter", GridEnter, c),
        KeyBinding::new("shift-enter", GridEnterUp, c),
        KeyBinding::new("f2", GridEdit, c),
        KeyBinding::new("escape", GridCancel, c),
        KeyBinding::new("delete", GridClear, c),
        KeyBinding::new("backspace", GridClear, c),
        KeyBinding::new(&format!("{cmd}-a"), GridSelectAll, c),
        KeyBinding::new(&format!("{cmd}-z"), GridUndo, c),
        KeyBinding::new(&format!("{cmd}-shift-z"), GridRedo, c),
        KeyBinding::new("ctrl-y", GridRedo, c),
        KeyBinding::new(&format!("{cmd}-c"), GridCopy, c),
        KeyBinding::new(&format!("{cmd}-x"), GridCut, c),
        KeyBinding::new(&format!("{cmd}-v"), GridPaste, c),
    ]);
}

/// Sizes along one axis: a default, sizes set by the file, hidden lines.
pub struct Axis {
    default: f32,
    sizes: BTreeMap<u32, f32>,
    hidden: BTreeSet<u32>,
}

impl Axis {
    pub fn new(default: f32, sizes: BTreeMap<u32, f32>, hidden: BTreeSet<u32>) -> Self {
        Self {
            default: default.max(1.0),
            sizes,
            hidden,
        }
    }

    pub fn size(&self, i: u32) -> f32 {
        if self.hidden.contains(&i) {
            0.0
        } else {
            self.sizes.get(&i).copied().unwrap_or(self.default)
        }
    }

    /// Where line `i` starts, in points.
    pub fn offset(&self, i: u32) -> f32 {
        let mut at = i as f32 * self.default;
        for (k, s) in self.sizes.range(..i) {
            if !self.hidden.contains(k) {
                at += s - self.default;
            }
        }
        for k in self.hidden.range(..i) {
            at -= self.sizes.get(k).copied().unwrap_or(self.default);
        }
        at
    }

    /// The line at `at` points, among the first `count`.
    pub fn at(&self, at: f32, count: u32) -> u32 {
        let (mut lo, mut hi) = (0u32, count.max(1));
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.offset(mid) <= at {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }
}

/// Characters of Calibri 11, as files give column widths, to points.
pub fn chars_to_points(chars: f32) -> f32 {
    let digit = 7.0;
    ((chars * digit + 5.0) / digit * 256.0).trunc() / 256.0 * digit * 0.75
}

struct Editing {
    at: (u32, u32),
    input: Entity<TextInput>,
    _events: Subscription,
}

pub struct Grid {
    focus: FocusHandle,
    pub active: (u32, u32),
    anchor: (u32, u32),
    /// Scrolled past the frozen panes, in points.
    scroll: (f32, f32),
    pub zoom: f32,
    editing: Option<Editing>,
    undo: Vec<Vec<Op>>,
    redo: Vec<Vec<Op>>,
    shaper: Rc<GpuiShaper>,
    /// The cell area on screen as last painted.
    frame: Rc<StdCell<Bounds<Pixels>>>,
    dragging: bool,
}

impl Grid {
    pub fn new(window: &Window, cx: &mut Context<DocView>) -> Self {
        Self {
            focus: cx.focus_handle(),
            active: (0, 0),
            anchor: (0, 0),
            scroll: (0.0, 0.0),
            zoom: 1.0,
            editing: None,
            undo: Vec::new(),
            redo: Vec::new(),
            shaper: Rc::new(GpuiShaper::new(window.text_system().clone())),
            frame: Rc::new(StdCell::new(Bounds::default())),
            dragging: false,
        }
    }

    pub fn focus(&self) -> &FocusHandle {
        &self.focus
    }

    fn range(&self) -> (u32, u32, u32, u32) {
        (
            self.anchor.0.min(self.active.0),
            self.anchor.1.min(self.active.1),
            self.anchor.0.max(self.active.0),
            self.anchor.1.max(self.active.1),
        )
    }
}

/// The sheet's geometry for one frame.
struct Geo {
    cols: Axis,
    rows: Axis,
    frozen: (u32, u32),
    rows_count: u32,
    cols_count: u32,
    z: f32,
    scroll: (f32, f32),
}

impl Geo {
    fn of(wb: &Workbook, sheet: usize, grid: &Grid) -> Option<Self> {
        let s = wb.sheets().get(sheet)?;
        let look = &s.look;
        let mut widths: BTreeMap<u32, f32> = s
            .widths
            .iter()
            .map(|(c, w)| (*c, chars_to_points(*w)))
            .collect();
        widths.extend(look.columns.iter().map(|(c, w)| (*c, *w)));
        let (rows_used, cols_used) = s.extent();
        Some(Self {
            cols: Axis::new(look.default_width, widths, look.hidden_columns.clone()),
            rows: Axis::new(
                look.default_height,
                look.rows.clone(),
                look.hidden_rows.clone(),
            ),
            frozen: look.frozen,
            rows_count: (rows_used + SPARE_ROWS)
                .max(grid.active.0 + SPARE_ROWS)
                .min(MAX_ROW + 1),
            cols_count: (cols_used + SPARE_COLS)
                .max(grid.active.1 + SPARE_COLS)
                .min(MAX_COL + 1),
            z: PX_PER_PT * grid.zoom * look.zoom.max(10) as f32 / 100.0,
            scroll: grid.scroll,
        })
    }

    /// A column's left edge on screen, from the cell area's left, in pixels.
    fn x(&self, c: u32) -> f32 {
        let at = if c < self.frozen.1 {
            self.cols.offset(c)
        } else {
            self.cols.offset(c) - self.scroll.0
        };
        at * self.z
    }

    fn y(&self, r: u32) -> f32 {
        let at = if r < self.frozen.0 {
            self.rows.offset(r)
        } else {
            self.rows.offset(r) - self.scroll.1
        };
        at * self.z
    }

    fn frozen_px(&self) -> (f32, f32) {
        (
            self.cols.offset(self.frozen.1) * self.z,
            self.rows.offset(self.frozen.0) * self.z,
        )
    }

    /// The visible columns: frozen ones, then the scrolled ones that fit.
    fn visible_cols(&self, width: f32) -> Vec<u32> {
        let mut out: Vec<u32> = (0..self.frozen.1).collect();
        let start = self
            .cols
            .at(
                self.cols.offset(self.frozen.1) + self.scroll.0,
                self.cols_count,
            )
            .max(self.frozen.1);
        let mut c = start;
        while c < self.cols_count && self.x(c) < width {
            out.push(c);
            c += 1;
        }
        out
    }

    fn visible_rows(&self, height: f32) -> Vec<u32> {
        let mut out: Vec<u32> = (0..self.frozen.0).collect();
        let start = self
            .rows
            .at(
                self.rows.offset(self.frozen.0) + self.scroll.1,
                self.rows_count,
            )
            .max(self.frozen.0);
        let mut r = start;
        while r < self.rows_count && self.y(r) < height {
            out.push(r);
            r += 1;
        }
        out
    }

    /// The cell under a point in the cell area, in pixels.
    fn cell_at(&self, x: f32, y: f32) -> (u32, u32) {
        let (fx, fy) = self.frozen_px();
        let col_pt = if x < fx {
            x / self.z
        } else {
            x / self.z + self.scroll.0
        };
        let row_pt = if y < fy {
            y / self.z
        } else {
            y / self.z + self.scroll.1
        };
        (
            self.rows.at(row_pt.max(0.0), self.rows_count),
            self.cols.at(col_pt.max(0.0), self.cols_count),
        )
    }
}

fn hsla(c: Rgb) -> Hsla {
    rgb(c.0).into()
}

fn merged_at(merged: &[(u32, u32, u32, u32)], r: u32, c: u32) -> Option<(u32, u32, u32, u32)> {
    merged
        .iter()
        .copied()
        .find(|(r0, c0, r1, c1)| (*r0..=*r1).contains(&r) && (*c0..=*c1).contains(&c))
}

/// One cell's drawing, computed in render and painted in the canvas.
struct Paint {
    rect: Bounds<Pixels>,
    /// Where the text may spill: wider than the cell for left-aligned text
    /// with empty neighbours.
    clip: Bounds<Pixels>,
    fill: Option<Hsla>,
    bar: Option<(f32, Hsla)>,
    text: SharedString,
    font: gpui::Font,
    size: f32,
    color: Hsla,
    underline: bool,
    strike: bool,
    h: HAlign,
    v: VAlign,
    wrap: bool,
    borders: [Option<Border>; 4],
    /// Which quadrant: frozen rows, frozen columns.
    pane: (bool, bool),
}

impl DocView {
    fn workbook(&self) -> Option<&Workbook> {
        match &self.doc.office {
            Office::Sheet(wb) => Some(wb),
            _ => None,
        }
    }

    fn grid_mut(&mut self) -> Option<&mut Grid> {
        self.grid.as_mut()
    }

    fn select(&mut self, at: (u32, u32), extend: bool, cx: &mut Context<Self>) {
        let Some(g) = self.grid_mut() else { return };
        let at = (at.0.min(MAX_ROW), at.1.min(MAX_COL));
        g.active = at;
        if !extend {
            g.anchor = at;
        }
        self.reveal(cx);
        cx.notify();
    }

    fn step(&mut self, dr: i64, dc: i64, extend: bool, cx: &mut Context<Self>) {
        let Some(g) = self.grid.as_ref() else { return };
        let (r, c) = g.active;
        // Skip hidden rows and columns.
        let Some(wb) = self.workbook() else { return };
        let Some(s) = wb.sheets().get(self.sheet) else {
            return;
        };
        let mut nr = r as i64;
        let mut nc = c as i64;
        loop {
            nr = (nr + dr).clamp(0, MAX_ROW as i64);
            nc = (nc + dc).clamp(0, MAX_COL as i64);
            let hidden = s.look.hidden_rows.contains(&(nr as u32))
                || s.look.hidden_columns.contains(&(nc as u32));
            if !hidden || nr == 0 && nc == 0 || (dr == 0 && dc == 0) {
                break;
            }
        }
        self.select((nr as u32, nc as u32), extend, cx);
    }

    /// Scrolls so the active cell is in view.
    fn reveal(&mut self, _cx: &mut Context<Self>) {
        let Some(wb) = self.workbook() else { return };
        let Some(g) = self.grid.as_ref() else { return };
        let Some(geo) = Geo::of(wb, self.sheet, g) else {
            return;
        };
        let frame = g.frame.get();
        let (w, h) = (f32::from(frame.size.width), f32::from(frame.size.height));
        let (fx, fy) = geo.frozen_px();
        let (r, c) = g.active;
        let mut scroll = g.scroll;
        if c >= geo.frozen.1 && w > 0.0 {
            let left = geo.cols.offset(c) - geo.cols.offset(geo.frozen.1);
            let right = left + geo.cols.size(c);
            let view = (w - fx) / geo.z;
            if left < scroll.0 {
                scroll.0 = left;
            } else if right > scroll.0 + view {
                scroll.0 = (right - view).max(0.0);
            }
        }
        if r >= geo.frozen.0 && h > 0.0 {
            let top = geo.rows.offset(r) - geo.rows.offset(geo.frozen.0);
            let bottom = top + geo.rows.size(r);
            let view = (h - fy) / geo.z;
            if top < scroll.1 {
                scroll.1 = top;
            } else if bottom > scroll.1 + view {
                scroll.1 = (bottom - view).max(0.0);
            }
        }
        if let Some(g) = self.grid_mut() {
            g.scroll = scroll;
        }
    }

    fn apply_ops(&mut self, ops: Vec<Op>, cx: &mut Context<Self>) -> bool {
        match crate::pageedit::apply_all(&mut self.doc, ops) {
            Ok(undo) => {
                if let Some(g) = self.grid_mut() {
                    g.undo.push(undo);
                    g.redo.clear();
                }
                cx.emit(DocEvent::Edited);
                cx.notify();
                true
            }
            Err(why) => {
                cx.emit(DocEvent::Refused(why));
                cx.notify();
                false
            }
        }
    }

    fn set_cells(&mut self, cells: Vec<(u32, u32, String)>, cx: &mut Context<Self>) {
        let sheet = self.sheet;
        let ops = cells
            .into_iter()
            .map(|(r, c, input)| Op::Sheet(SheetOp::SetCell { sheet, r, c, input }))
            .collect();
        self.apply_ops(ops, cx);
    }

    fn grid_history(&mut self, redo: bool, cx: &mut Context<Self>) {
        let Some(g) = self.grid_mut() else { return };
        let Some(ops) = (if redo { g.redo.pop() } else { g.undo.pop() }) else {
            return;
        };
        match crate::pageedit::apply_all(&mut self.doc, ops) {
            Ok(back) => {
                if let Some(g) = self.grid_mut() {
                    if redo {
                        g.undo.push(back);
                    } else {
                        g.redo.push(back);
                    }
                }
                cx.emit(DocEvent::Edited);
            }
            Err(why) => cx.emit(DocEvent::Refused(why)),
        }
        cx.notify();
    }

    /// The cell's text as editing starts from it: its formula or value.
    fn cell_input(&self, r: u32, c: u32) -> String {
        self.workbook()
            .and_then(|wb| wb.sheets().get(self.sheet))
            .and_then(|s| s.cells.get(&(r, c)))
            .map(|cell| cell.input())
            .unwrap_or_default()
    }

    pub(super) fn start_edit(
        &mut self,
        with: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(g) = self.grid.as_ref() else { return };
        if g.editing.is_some() {
            return;
        }
        let at = g.active;
        let locked = self
            .workbook()
            .and_then(|wb| wb.sheets().get(self.sheet))
            .is_some_and(|s| {
                s.locked.iter().any(|(r0, c0, r1, c1)| {
                    (*r0..=*r1).contains(&at.0) && (*c0..=*c1).contains(&at.1)
                })
            });
        if locked {
            cx.emit(DocEvent::Refused(
                t("This part is shown as it is and can't be edited here.").into(),
            ));
            return;
        }
        let text = match with {
            Some(text) => text.to_string(),
            None => self.cell_input(at.0, at.1),
        };
        let input = cx.new(|cx| {
            let mut i = TextInput::new(window, cx);
            i.set_text(text.clone(), cx);
            i
        });
        let events = cx.subscribe_in(&input, window, |d, _, event, window, cx| match event {
            InputEvent::Submit => {
                d.finish_edit(true, window, cx);
                d.step(1, 0, false, cx);
            }
            InputEvent::Blur => d.finish_edit(true, window, cx),
            _ => {}
        });
        let focus = input.read(cx).focus_handle(cx);
        focus.focus(window, cx);
        if let Some(g) = self.grid_mut() {
            g.editing = Some(Editing {
                at,
                input,
                _events: events,
            });
        }
        cx.notify();
    }

    fn finish_edit(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = self.grid_mut() else { return };
        let Some(editing) = g.editing.take() else {
            return;
        };
        let focus = g.focus.clone();
        let text = editing.input.read(cx).text().to_string();
        let (r, c) = editing.at;
        if keep && text != self.cell_input(r, c) {
            self.set_cells(vec![(r, c, text)], cx);
        }
        focus.focus(window, cx);
        cx.notify();
    }

    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        let Some(g) = self.grid.as_ref() else { return };
        let (r0, c0, r1, c1) = g.range();
        let Some(s) = self.workbook().and_then(|wb| wb.sheets().get(self.sheet)) else {
            return;
        };
        let cells: Vec<(u32, u32, String)> = s
            .cells
            .range((r0, c0)..=(r1, c1))
            .filter(|((r, c), cell)| {
                (c0..=c1).contains(c) && (r0..=r1).contains(r) && cell.value != Value::Empty
                    || cell.formula.is_some()
            })
            .map(|((r, c), _)| (*r, *c, String::new()))
            .collect();
        if !cells.is_empty() {
            self.set_cells(cells, cx);
        }
    }

    fn copy_cells(&mut self, cut: bool, cx: &mut Context<Self>) {
        let Some(g) = self.grid.as_ref() else { return };
        let (r0, c0, r1, c1) = g.range();
        let Some(wb) = self.workbook() else { return };
        let mut out = String::new();
        for r in r0..=r1 {
            let row: Vec<String> = (c0..=c1).map(|c| wb.shown(self.sheet, r, c).text).collect();
            out.push_str(&row.join("\t"));
            if r < r1 {
                out.push('\n');
            }
        }
        cx.write_to_clipboard(ClipboardItem::new_string(out));
        if cut {
            self.clear_selection(cx);
        }
    }

    fn paste_cells(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else {
            return;
        };
        let Some(g) = self.grid.as_ref() else { return };
        let (r0, c0) = (g.range().0, g.range().1);
        let mut cells = Vec::new();
        for (dr, line) in text.trim_end_matches(['\n', '\r']).lines().enumerate() {
            for (dc, value) in line.split('\t').enumerate() {
                cells.push((r0 + dr as u32, c0 + dc as u32, value.to_string()));
            }
        }
        self.set_cells(cells, cx);
    }

    fn page_rows(&self) -> i64 {
        let Some(g) = self.grid.as_ref() else {
            return 20;
        };
        let h = f32::from(g.frame.get().size.height);
        ((h / (20.0 * PX_PER_PT * g.zoom)) as i64).max(1)
    }

    /// Typing on the grid starts editing the active cell with that text.
    pub(super) fn grid_typed(
        &mut self,
        text: &str,
        marked: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.grid.as_ref().is_some_and(|g| g.editing.is_some()) {
            return;
        }
        self.start_edit(Some(""), window, cx);
        let Some(input) = self
            .grid
            .as_ref()
            .and_then(|g| g.editing.as_ref())
            .map(|e| e.input.clone())
        else {
            return;
        };
        input.update(cx, |i, cx| {
            use gpui::EntityInputHandler;
            if marked {
                i.replace_and_mark_text_in_range(None, text, None, window, cx);
            } else {
                i.replace_text_in_range(None, text, window, cx);
            }
        });
    }

    fn grid_mouse(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(g) = self.grid.as_ref() else { return };
        let frame = g.frame.get();
        let local = ev.position - frame.origin;
        let (x, y) = (f32::from(local.x), f32::from(local.y));
        let Some(wb) = self.workbook() else { return };
        let Some(geo) = Geo::of(wb, self.sheet, g) else {
            return;
        };
        let merged = wb
            .sheets()
            .get(self.sheet)
            .map(|s| s.merged.clone())
            .unwrap_or_default();
        let focus = g.focus.clone();
        if g.editing.is_some() {
            self.finish_edit(true, window, cx);
        }
        window.focus(&focus, cx);
        // Headers select whole rows and columns.
        if x < ROW_HEADER && y < COL_HEADER {
            if let Some(g) = self.grid_mut() {
                g.anchor = (0, 0);
                g.active = (MAX_ROW, MAX_COL);
            }
            cx.notify();
            return;
        }
        if y < COL_HEADER {
            let (_, c) = geo.cell_at(x - ROW_HEADER, 0.0);
            if let Some(g) = self.grid_mut() {
                g.anchor = (0, c);
                g.active = (MAX_ROW, c);
            }
            cx.notify();
            return;
        }
        if x < ROW_HEADER {
            let (r, _) = geo.cell_at(0.0, y - COL_HEADER);
            if let Some(g) = self.grid_mut() {
                g.anchor = (r, 0);
                g.active = (r, MAX_COL);
            }
            cx.notify();
            return;
        }
        let mut at = geo.cell_at(x - ROW_HEADER, y - COL_HEADER);
        if let Some((r0, c0, _, _)) = merged_at(&merged, at.0, at.1) {
            at = (r0, c0);
        }
        if let Some(g) = self.grid_mut() {
            g.dragging = true;
        }
        self.select(at, ev.modifiers.shift, cx);
        if ev.click_count >= 2 {
            self.start_edit(None, window, cx);
        }
    }

    fn grid_drag(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(g) = self.grid.as_ref() else { return };
        if !g.dragging || ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let frame = g.frame.get();
        let local = ev.position - frame.origin;
        let Some(wb) = self.workbook() else { return };
        let Some(geo) = Geo::of(wb, self.sheet, g) else {
            return;
        };
        let at = geo.cell_at(
            f32::from(local.x) - ROW_HEADER,
            f32::from(local.y) - COL_HEADER,
        );
        if at != g.active {
            if let Some(g) = self.grid_mut() {
                g.active = at;
            }
            cx.notify();
        }
    }

    fn grid_scroll(&mut self, ev: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(g) = self.grid_mut() else { return };
        let d = ev.delta.pixel_delta(px(20.0));
        let z = PX_PER_PT * g.zoom;
        let (mut dx, mut dy) = (-f32::from(d.x) / z, -f32::from(d.y) / z);
        if ev.modifiers.shift && dx == 0.0 {
            (dx, dy) = (dy, 0.0);
        }
        g.scroll.0 = (g.scroll.0 + dx).max(0.0);
        g.scroll.1 = (g.scroll.1 + dy).max(0.0);
        cx.notify();
    }

    /// Everything to draw for the visible cells.
    fn grid_paints(&self, geo: &Geo, width: f32, height: f32) -> (Vec<Paint>, Vec<u32>, Vec<u32>) {
        let Some(wb) = self.workbook() else {
            return (Vec::new(), Vec::new(), Vec::new());
        };
        let Some(s) = wb.sheets().get(self.sheet) else {
            return (Vec::new(), Vec::new(), Vec::new());
        };
        let g = self.grid.as_ref().expect("grid");
        let cols = geo.visible_cols(width - ROW_HEADER);
        let rows = geo.visible_rows(height - COL_HEADER);
        let mut out = Vec::new();
        let mut done_merges = BTreeSet::new();
        for &r in &rows {
            for &c in &cols {
                let (mut r0, mut c0, mut r1, mut c1) = (r, c, r, c);
                if let Some(m) = merged_at(&s.merged, r, c) {
                    if !done_merges.insert(m) {
                        continue;
                    }
                    (r0, c0, r1, c1) = m;
                }
                let shown = wb.shown(self.sheet, r0, c0);
                let cell = s.cells.get(&(r0, c0));
                let style = &shown.style;
                if shown.text.is_empty()
                    && style.fill.is_none()
                    && style.borders == Default::default()
                    && r0 == r1
                    && c0 == c1
                {
                    continue;
                }
                let x0 = geo.x(c0) + ROW_HEADER;
                let y0 = geo.y(r0) + COL_HEADER;
                let x1 = geo.x(c1) + geo.cols.size(c1) * geo.z + ROW_HEADER;
                let y1 = geo.y(r1) + geo.rows.size(r1) * geo.z + COL_HEADER;
                let rect = Bounds::new(point(px(x0), px(y0)), size(px(x1 - x0), px(y1 - y0)));
                let number = matches!(cell.map(|c| &c.value), Some(Value::Number(_)));
                let h = match style.h_align {
                    HAlign::General if number => HAlign::Right,
                    HAlign::General
                        if matches!(
                            cell.map(|c| &c.value),
                            Some(Value::Bool(_) | Value::Error(_))
                        ) =>
                    {
                        HAlign::Center
                    }
                    other => other,
                };
                // Left-aligned text spills right over empty cells, as in Excel.
                let mut clip = rect;
                if matches!(h, HAlign::General | HAlign::Left) && !style.wrap && !number && r0 == r1
                {
                    let mut c = c1 + 1;
                    let mut right = x1;
                    while c < geo.cols_count
                        && !s
                            .cells
                            .get(&(r0, c))
                            .is_some_and(|n| n.value != Value::Empty)
                        && merged_at(&s.merged, r0, c).is_none()
                        && right < width
                    {
                        right += geo.cols.size(c) * geo.z;
                        c += 1;
                    }
                    clip.size.width = px(right - x0);
                }
                let font = &style.font;
                let cjk = shown.text.chars().any(|ch| (ch as u32) >= 0x2E80);
                let key = FontKey {
                    family: if cjk {
                        font.fonts.east_asia.clone()
                    } else {
                        font.fonts.latin.clone()
                    },
                    size: font.size.map(|s| s as f32 / 20.0).unwrap_or(11.0),
                    bold: font.bold || cell.is_some_and(|c| c.bold),
                    italic: font.italic,
                    cjk,
                };
                out.push(Paint {
                    rect,
                    clip,
                    fill: style.fill.map(hsla),
                    bar: shown.bar.map(|(f, c)| (f, hsla(c))),
                    text: shown.text.clone().into(),
                    font: g.shaper.font(&key),
                    size: key.size * geo.z,
                    color: font.color.map(hsla).unwrap_or(Hsla::black()),
                    underline: font.underline,
                    strike: font.strike,
                    h,
                    v: style.v_align,
                    wrap: style.wrap,
                    borders: [
                        style.borders.top,
                        style.borders.right,
                        style.borders.bottom,
                        style.borders.left,
                    ],
                    pane: (r0 < geo.frozen.0, c0 < geo.frozen.1),
                });
            }
        }
        (out, rows, cols)
    }

    pub(super) fn grid_body(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if self.grid.is_none() {
            self.grid = Some(Grid::new(window, cx));
        }
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let small = theme.text_size(TextSize::Sm);
        let me = cx.entity();
        let Some(wb) = self.workbook() else {
            return div().into_any_element();
        };
        let sheets: Vec<(usize, String, Option<Rgb>)> = wb
            .sheets()
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.hidden)
            .map(|(i, s)| (i, s.name.clone(), s.look.tab_color))
            .collect();
        let current = self.sheet.min(wb.sheets().len().saturating_sub(1));
        let Some(sheet) = wb.sheets().get(current) else {
            return div().into_any_element();
        };
        let gridlines = sheet.look.gridlines;
        let merged = sheet.merged.clone();
        let g = self.grid.as_ref().expect("made above");
        let Some(geo) = Geo::of(wb, current, g) else {
            return div().into_any_element();
        };
        let frame = g.frame.clone();
        let bounds = frame.get();
        let (width, height) = if bounds.size.width > px(0.0) {
            (f32::from(bounds.size.width), f32::from(bounds.size.height))
        } else {
            (800.0, 600.0)
        };
        let (paints, rows, cols) = self.grid_paints(&geo, width, height);
        let g = self.grid.as_ref().expect("made above");
        let (r0, c0, r1, c1) = g.range();
        let active = g.active;
        let name = if (r0, c0) != (r1, c1) {
            format!("{}:{}", a1(r0, c0), a1(r1.min(MAX_ROW), c1.min(MAX_COL)))
        } else {
            a1(active.0, active.1)
        };
        let formula = self.cell_input(active.0, active.1);
        let note = wb
            .sheets()
            .get(current)
            .and_then(|s| s.cells.get(&active))
            .is_some_and(|c| c.formula.is_some() && !c.edited);

        // Where each visible row and column is, for headers and lines.
        let col_lines: Vec<(u32, f32, f32)> = cols
            .iter()
            .map(|c| (*c, geo.x(*c) + ROW_HEADER, geo.cols.size(*c) * geo.z))
            .collect();
        let row_lines: Vec<(u32, f32, f32)> = rows
            .iter()
            .map(|r| (*r, geo.y(*r) + COL_HEADER, geo.rows.size(*r) * geo.z))
            .collect();
        let (fx, fy) = geo.frozen_px();
        let selection = {
            let x0 = geo.x(c0) + ROW_HEADER;
            let y0 = geo.y(r0) + COL_HEADER;
            let x1 = if c1 >= MAX_COL {
                width
            } else {
                geo.x(c1) + geo.cols.size(c1) * geo.z + ROW_HEADER
            };
            let y1 = if r1 >= MAX_ROW {
                height
            } else {
                geo.y(r1) + geo.rows.size(r1) * geo.z + COL_HEADER
            };
            Bounds::new(point(px(x0), px(y0)), size(px(x1 - x0), px(y1 - y0)))
        };
        let active_rect = {
            let (ar0, ac0, ar1, ac1) = merged_at(&merged, active.0, active.1)
                .unwrap_or((active.0, active.1, active.0, active.1));
            let x0 = geo.x(ac0) + ROW_HEADER;
            let y0 = geo.y(ar0) + COL_HEADER;
            let x1 = geo.x(ac1) + geo.cols.size(ac1) * geo.z + ROW_HEADER;
            let y1 = geo.y(ar1) + geo.rows.size(ar1) * geo.z + COL_HEADER;
            Bounds::new(point(px(x0), px(y0)), size(px(x1 - x0), px(y1 - y0)))
        };
        let header_bg = colors.sunken;
        let header_fg = colors.fg_muted;
        let line = colors.border;
        let accent = colors.accent;
        let paper: Hsla = rgb(0xffffff).into();
        let grid_line: Hsla = rgb(0xd9d9d9).into();
        let editing = g.editing.as_ref().map(|e| (e.input.clone(), active_rect));
        let focus = g.focus.clone();
        let input_target = me.clone();
        let sel_rows = (r0, r1);
        let sel_cols = (c0, c1);
        let frozen = geo.frozen;

        let painter = canvas(
            move |bounds, _, _| bounds,
            move |bounds, _, window, cx| {
                if frame.get() != bounds {
                    frame.set(bounds);
                    input_target.update(cx, |_, cx| cx.notify());
                }
                window.handle_input(
                    &focus,
                    gpui::ElementInputHandler::new(bounds, input_target.clone()),
                    cx,
                );
                let o = bounds.origin;
                let at = |b: Bounds<Pixels>| Bounds::new(o + b.origin, b.size);
                let full = Bounds::new(o, bounds.size);
                window.paint_quad(gpui::fill(full, paper));
                let area = |frozen_r: bool, frozen_c: bool| {
                    let x = if frozen_c {
                        ROW_HEADER
                    } else {
                        ROW_HEADER + fx
                    };
                    let y = if frozen_r {
                        COL_HEADER
                    } else {
                        COL_HEADER + fy
                    };
                    let w = if frozen_c {
                        fx
                    } else {
                        width - ROW_HEADER - fx
                    };
                    let h = if frozen_r {
                        fy
                    } else {
                        height - COL_HEADER - fy
                    };
                    Bounds::new(
                        o + point(px(x), px(y)),
                        size(px(w.max(0.0)), px(h.max(0.0))),
                    )
                };
                for pane in [(false, false), (true, false), (false, true), (true, true)] {
                    let mask = area(pane.0, pane.1);
                    if mask.size.width <= px(0.0) || mask.size.height <= px(0.0) {
                        continue;
                    }
                    window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
                        window.paint_quad(gpui::fill(mask, paper));
                        if gridlines {
                            for (c, x, w) in &col_lines {
                                if (*c < frozen.1) == pane.1 {
                                    let b = Bounds::new(
                                        o + point(px(x + w - 1.0), mask.origin.y - o.y),
                                        size(px(1.0), mask.size.height),
                                    );
                                    window.paint_quad(gpui::fill(b, grid_line));
                                }
                            }
                            for (r, y, h) in &row_lines {
                                if (*r < frozen.0) != pane.0 {
                                    continue;
                                }
                                let b = Bounds::new(
                                    o + point(mask.origin.x - o.x, px(y + h - 1.0)),
                                    size(mask.size.width, px(1.0)),
                                );
                                window.paint_quad(gpui::fill(b, grid_line));
                            }
                        }
                        for p in paints.iter().filter(|p| p.pane == pane) {
                            let rect = at(p.rect);
                            if let Some(fill) = p.fill {
                                window.paint_quad(gpui::fill(rect, fill));
                            } else if p.rect.size.width > px(0.0)
                                && (p.rect.size.height > px(30.0) || p.text.is_empty())
                            {
                                // A merged cell hides the lines inside it.
                                let inner = Bounds::new(
                                    rect.origin,
                                    size(rect.size.width - px(1.0), rect.size.height - px(1.0)),
                                );
                                window.paint_quad(gpui::fill(inner, paper));
                            }
                            if let Some((f, color)) = p.bar {
                                let b = Bounds::new(
                                    rect.origin + point(px(2.0), px(3.0)),
                                    size(
                                        (rect.size.width - px(4.0)) * f,
                                        rect.size.height - px(6.0),
                                    ),
                                );
                                window.paint_quad(gpui::fill(b, color.opacity(0.6)));
                            }
                        }
                        for p in paints.iter().filter(|p| p.pane == pane) {
                            if p.text.is_empty() {
                                continue;
                            }
                            let rect = at(p.rect);
                            let clip = at(p.clip);
                            let run = gpui::TextRun {
                                len: p.text.len(),
                                font: p.font.clone(),
                                color: p.color,
                                background_color: None,
                                underline: p.underline.then(|| gpui::UnderlineStyle {
                                    thickness: px(1.0),
                                    color: Some(p.color),
                                    wavy: false,
                                }),
                                strikethrough: p.strike.then(|| gpui::StrikethroughStyle {
                                    thickness: px(1.0),
                                    color: Some(p.color),
                                }),
                            };
                            let pad = px(3.0);
                            let line_h = px(p.size * 1.25);
                            let inner_w = rect.size.width - pad * 2.0;
                            let lines = window.text_system().shape_text(
                                p.text.clone(),
                                px(p.size),
                                &[run],
                                p.wrap.then_some(inner_w),
                                None,
                            );
                            let Ok(lines) = lines else { continue };
                            let count: usize =
                                lines.iter().map(|l| l.wrap_boundaries().len() + 1).sum();
                            let text_h = line_h * count as f32;
                            let top = match p.v {
                                VAlign::Top => rect.origin.y + px(1.0),
                                VAlign::Center => rect.origin.y + (rect.size.height - text_h) / 2.0,
                                VAlign::Bottom => {
                                    rect.origin.y + rect.size.height - text_h - px(1.0)
                                }
                            };
                            let (align, x, w) = match p.h {
                                HAlign::Right => (TextAlign::Right, rect.origin.x + pad, inner_w),
                                HAlign::Center | HAlign::CenterAcross | HAlign::Distributed => {
                                    (TextAlign::Center, rect.origin.x + pad, inner_w)
                                }
                                _ => (
                                    TextAlign::Left,
                                    rect.origin.x + pad,
                                    clip.size.width - pad * 2.0,
                                ),
                            };
                            window.with_content_mask(
                                Some(ContentMask { bounds: clip }),
                                |window| {
                                    let mut y = top;
                                    for l in lines.iter() {
                                        let b = Bounds::new(point(x, y), size(w, line_h));
                                        let _ = l.paint(
                                            point(x, y),
                                            line_h,
                                            align,
                                            Some(b),
                                            window,
                                            cx,
                                        );
                                        y += line_h * (l.wrap_boundaries().len() + 1) as f32;
                                    }
                                },
                            );
                        }
                        for p in paints.iter().filter(|p| p.pane == pane) {
                            let r = at(p.rect);
                            let edge =
                                |b: &Border| px((b.width as f32 / 20.0 * PX_PER_PT).max(1.0));
                            let sides = [
                                p.borders[0].map(|b| {
                                    (Bounds::new(r.origin, size(r.size.width, edge(&b))), b)
                                }),
                                p.borders[1].map(|b| {
                                    (
                                        Bounds::new(
                                            point(r.origin.x + r.size.width - edge(&b), r.origin.y),
                                            size(edge(&b), r.size.height),
                                        ),
                                        b,
                                    )
                                }),
                                p.borders[2].map(|b| {
                                    (
                                        Bounds::new(
                                            point(
                                                r.origin.x,
                                                r.origin.y + r.size.height - edge(&b),
                                            ),
                                            size(r.size.width, edge(&b)),
                                        ),
                                        b,
                                    )
                                }),
                                p.borders[3].map(|b| {
                                    (Bounds::new(r.origin, size(edge(&b), r.size.height)), b)
                                }),
                            ];
                            for (b, border) in sides.into_iter().flatten() {
                                window.paint_quad(gpui::fill(b, hsla(border.color)));
                            }
                        }
                    });
                }
                // The selection, then the active cell.
                let body = Bounds::new(
                    o + point(px(ROW_HEADER), px(COL_HEADER)),
                    size(px(width - ROW_HEADER), px(height - COL_HEADER)),
                );
                window.with_content_mask(Some(ContentMask { bounds: body }), |window| {
                    let sel = at(selection);
                    window.paint_quad(gpui::fill(sel, accent.opacity(0.1)));
                    let act = at(active_rect);
                    window.paint_quad(gpui::outline(sel, accent, gpui::BorderStyle::Solid));
                    window.paint_quad(
                        gpui::outline(act, accent, gpui::BorderStyle::Solid).border_widths(px(2.0)),
                    );
                });
                // Frozen pane dividers.
                if fx > 0.0 {
                    let b = Bounds::new(
                        o + point(px(ROW_HEADER + fx - 1.0), px(0.0)),
                        size(px(1.5), px(height)),
                    );
                    window.paint_quad(gpui::fill(b, line));
                }
                if fy > 0.0 {
                    let b = Bounds::new(
                        o + point(px(0.0), px(COL_HEADER + fy - 1.0)),
                        size(px(width), px(1.5)),
                    );
                    window.paint_quad(gpui::fill(b, line));
                }
                // Headers.
                let head_font = gpui::Font {
                    family: ".SystemUIFont".into(),
                    features: Default::default(),
                    fallbacks: None,
                    weight: gpui::FontWeight::NORMAL,
                    style: gpui::FontStyle::Normal,
                };
                let label = |window: &mut Window,
                             cx: &mut App,
                             text: String,
                             b: Bounds<Pixels>,
                             on: bool| {
                    window.paint_quad(gpui::fill(
                        b,
                        if on { accent.opacity(0.18) } else { header_bg },
                    ));
                    let run = gpui::TextRun {
                        len: text.len(),
                        font: head_font.clone(),
                        color: if on { accent } else { header_fg },
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    };
                    let shaped =
                        window
                            .text_system()
                            .shape_line(text.into(), px(11.0), &[run], None);
                    let h = shaped.ascent + shaped.descent;
                    let origin = point(b.origin.x, b.origin.y + (b.size.height - h) / 2.0);
                    let _ =
                        shaped.paint(origin, h, TextAlign::Center, Some(b.size.width), window, cx);
                };
                let head_row = Bounds::new(o, size(px(width), px(COL_HEADER)));
                window.paint_quad(gpui::fill(head_row, header_bg));
                for (c, x, w) in &col_lines {
                    if *w <= 0.0 {
                        continue;
                    }
                    let b = Bounds::new(o + point(px(*x), px(0.0)), size(px(*w), px(COL_HEADER)));
                    let on = (sel_cols.0..=sel_cols.1).contains(c);
                    let name = a1(0, *c);
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: Bounds::new(
                                o + point(px(ROW_HEADER), px(0.0)),
                                size(px(width - ROW_HEADER), px(COL_HEADER)),
                            ),
                        }),
                        |window| label(window, cx, name.trim_end_matches('1').to_string(), b, on),
                    );
                    window.paint_quad(gpui::fill(
                        Bounds::new(
                            b.origin + point(px(*w - 1.0), px(0.0)),
                            size(px(1.0), px(COL_HEADER)),
                        ),
                        line,
                    ));
                }
                let head_col = Bounds::new(o, size(px(ROW_HEADER), px(height)));
                window.paint_quad(gpui::fill(head_col, header_bg));
                for (r, y, h) in &row_lines {
                    if *h <= 0.0 {
                        continue;
                    }
                    let b = Bounds::new(o + point(px(0.0), px(*y)), size(px(ROW_HEADER), px(*h)));
                    let on = (sel_rows.0..=sel_rows.1).contains(r);
                    window.with_content_mask(
                        Some(ContentMask {
                            bounds: Bounds::new(
                                o + point(px(0.0), px(COL_HEADER)),
                                size(px(ROW_HEADER), px(height - COL_HEADER)),
                            ),
                        }),
                        |window| label(window, cx, (r + 1).to_string(), b, on),
                    );
                    window.paint_quad(gpui::fill(
                        Bounds::new(
                            b.origin + point(px(0.0), px(*h - 1.0)),
                            size(px(ROW_HEADER), px(1.0)),
                        ),
                        line,
                    ));
                }
                window.paint_quad(gpui::fill(
                    Bounds::new(o, size(px(ROW_HEADER), px(COL_HEADER))),
                    header_bg,
                ));
                let edge_r = Bounds::new(
                    o + point(px(ROW_HEADER - 1.0), px(0.0)),
                    size(px(1.0), px(height)),
                );
                window.paint_quad(gpui::fill(edge_r, line));
                let edge_c = Bounds::new(
                    o + point(px(0.0), px(COL_HEADER - 1.0)),
                    size(px(width), px(1.0)),
                );
                window.paint_quad(gpui::fill(edge_c, line));
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let (down, drag, wheel, up) = (me.clone(), me.clone(), me.clone(), me.clone());
        let g = self.grid.as_ref().expect("made above");
        let mut area = div()
            .id("doc-grid")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .track_focus(&g.focus)
            .key_context(CONTEXT)
            .cursor_default()
            .child(painter)
            .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, cx| {
                down.update(cx, |d, cx| d.grid_mouse(ev, window, cx))
            })
            .on_mouse_move(move |ev: &MouseMoveEvent, _, cx| {
                drag.update(cx, |d, cx| d.grid_drag(ev, cx))
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                up.update(cx, |d, _| {
                    if let Some(g) = d.grid_mut() {
                        g.dragging = false;
                    }
                })
            })
            .on_scroll_wheel(move |ev: &ScrollWheelEvent, _, cx| {
                wheel.update(cx, |d, cx| d.grid_scroll(ev, cx))
            })
            .on_action(cx.listener(|d, _: &GridLeft, _, cx| d.step(0, -1, false, cx)))
            .on_action(cx.listener(|d, _: &GridRight, _, cx| d.step(0, 1, false, cx)))
            .on_action(cx.listener(|d, _: &GridUp, _, cx| d.step(-1, 0, false, cx)))
            .on_action(cx.listener(|d, _: &GridDown, _, cx| d.step(1, 0, false, cx)))
            .on_action(cx.listener(|d, _: &GridSelectLeft, _, cx| d.step(0, -1, true, cx)))
            .on_action(cx.listener(|d, _: &GridSelectRight, _, cx| d.step(0, 1, true, cx)))
            .on_action(cx.listener(|d, _: &GridSelectUp, _, cx| d.step(-1, 0, true, cx)))
            .on_action(cx.listener(|d, _: &GridSelectDown, _, cx| d.step(1, 0, true, cx)))
            .on_action(cx.listener(|d, _: &GridHome, _, cx| {
                let r = d.grid.as_ref().map(|g| g.active.0).unwrap_or(0);
                d.select((r, 0), false, cx)
            }))
            .on_action(cx.listener(|d, _: &GridTop, _, cx| d.select((0, 0), false, cx)))
            .on_action(cx.listener(|d, _: &GridPageUp, _, cx| {
                let n = d.page_rows();
                d.step(-n, 0, false, cx)
            }))
            .on_action(cx.listener(|d, _: &GridPageDown, _, cx| {
                let n = d.page_rows();
                d.step(n, 0, false, cx)
            }))
            .on_action(cx.listener(|d, _: &GridNext, window, cx| {
                d.finish_edit(true, window, cx);
                d.step(0, 1, false, cx)
            }))
            .on_action(cx.listener(|d, _: &GridPrevious, window, cx| {
                d.finish_edit(true, window, cx);
                d.step(0, -1, false, cx)
            }))
            .on_action(cx.listener(|d, _: &GridEnter, _, cx| d.step(1, 0, false, cx)))
            .on_action(cx.listener(|d, _: &GridEnterUp, _, cx| d.step(-1, 0, false, cx)))
            .on_action(cx.listener(|d, _: &GridEdit, window, cx| d.start_edit(None, window, cx)))
            .on_action(
                cx.listener(|d, _: &GridCancel, window, cx| d.finish_edit(false, window, cx)),
            )
            .on_action(cx.listener(|d, _: &GridClear, _, cx| d.clear_selection(cx)))
            .on_action(cx.listener(|d, _: &GridSelectAll, _, cx| {
                if let Some(g) = d.grid_mut() {
                    g.anchor = (0, 0);
                    g.active = (MAX_ROW, MAX_COL);
                }
                cx.notify()
            }))
            .on_action(cx.listener(|d, _: &GridUndo, _, cx| d.grid_history(false, cx)))
            .on_action(cx.listener(|d, _: &GridRedo, _, cx| d.grid_history(true, cx)))
            .on_action(cx.listener(|d, _: &GridCopy, _, cx| d.copy_cells(false, cx)))
            .on_action(cx.listener(|d, _: &GridCut, _, cx| d.copy_cells(true, cx)))
            .on_action(cx.listener(|d, _: &GridPaste, _, cx| d.paste_cells(cx)));
        if let Some((input, rect)) = editing {
            area = area.child(
                div()
                    .absolute()
                    .left(rect.origin.x - px(1.0))
                    .top(rect.origin.y - px(1.0))
                    .min_w(rect.size.width + px(2.0))
                    .w(rect.size.width.max(px(160.0)))
                    .child(Input::new(&input).size(ControlSize::Sm)),
            );
        }

        // The cell name and its contents above; the sheets below.
        let bar = div()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(colors.border)
            .text_size(small)
            .child(
                div()
                    .min_w_16()
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius(Radius::Sm))
                    .bg(colors.sunken)
                    .child(name),
            )
            .child(
                div()
                    .text_color(colors.fg_muted)
                    .child(gpui::SharedString::from("fx")),
            )
            .child(
                div()
                    .id("grid-formula")
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .cursor_text()
                    .child(formula)
                    .on_click(cx.listener(|d, _, window, cx| d.start_edit(None, window, cx))),
            );
        let mut tabs = div()
            .flex()
            .items_center()
            .gap_0p5()
            .px_2()
            .py_0p5()
            .border_t_1()
            .border_color(colors.border)
            .bg(colors.sunken);
        for (i, name, tab) in sheets {
            let on = i == current;
            tabs = tabs.child(
                div()
                    .id(SharedString::from(format!("grid-sheet-{i}")))
                    .px_3()
                    .py_0p5()
                    .rounded(theme.radius(Radius::Sm))
                    .text_size(small)
                    .cursor_pointer()
                    .when(on, |el| el.bg(colors.surface).text_color(colors.fg))
                    .when(!on, |el| {
                        el.text_color(colors.fg_muted)
                            .hover(|el| el.bg(colors.hover))
                    })
                    .when_some(tab, |el, c| el.border_b_2().border_color(hsla(c)))
                    .on_click(cx.listener(move |d, _, window, cx| {
                        d.finish_edit(true, window, cx);
                        d.sheet = i;
                        if let Some(g) = d.grid_mut() {
                            g.active = (0, 0);
                            g.anchor = (0, 0);
                            g.scroll = (0.0, 0.0);
                        }
                        cx.notify();
                    }))
                    .child(name),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(bar)
            .when(note, |el| {
                el.child(
                    div()
                        .px_3()
                        .pb_1()
                        .text_size(theme.text_size(TextSize::Xs))
                        .text_color(colors.fg_muted)
                        .child(t("Formulas keep their last results here; Excel recalculates them when it opens the file.").to_string()),
                )
            })
            .child(area)
            .child(tabs)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_offsets_and_lookup_with_custom_and_hidden_lines() {
        let sizes = BTreeMap::from([(2, 30.0), (5, 5.0)]);
        let hidden = BTreeSet::from([3]);
        let a = Axis::new(10.0, sizes, hidden);
        assert_eq!(a.offset(0), 0.0);
        assert_eq!(a.offset(2), 20.0);
        assert_eq!(a.offset(3), 50.0);
        // Line 3 is hidden: 4 starts where 3 would.
        assert_eq!(a.offset(4), 50.0);
        assert_eq!(a.offset(6), 65.0);
        assert_eq!(a.at(0.0, 100), 0);
        assert_eq!(a.at(25.0, 100), 2);
        assert_eq!(a.at(55.0, 100), 4);
        assert_eq!(a.at(61.0, 100), 5);
        assert_eq!(a.at(1000.0, 100), 99);
    }

    #[test]
    fn excel_character_widths_become_points() {
        // Excel's default 8.43 characters is 64 px, 48 pt.
        assert!((chars_to_points(8.43) - 48.0).abs() < 0.5);
    }
}
