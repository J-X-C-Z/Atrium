//! Word and PowerPoint edited on their pages: atrium-layout lays the
//! document out, this file paints the pages with GPUI's text system and
//! turns clicks, keys and the input method into edits.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use atrium_layout::{FontKey, Item, Layout, Metrics, Pos, Shaper};
use atrium_office::look::Rgb;
use atrium_office::{Align, Inline, Marks, Office, Op, WordOp};
use ely_gpui_component::{
    buttons::{ButtonVariant, IconButton},
    primitives::IconName,
    theme::{ActiveTheme, ControlSize, Elevation},
};
use gpui::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    Focusable, Font, FontFallbacks, FontStyle, FontWeight, Hsla, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point,
    SharedString, StatefulInteractiveElement, Styled, TextAlign, TextRun, UTF16Selection, Window,
    WindowTextSystem, actions, canvas, div, img, point, prelude::*, px, rgb, size,
};

use super::document::{DocEvent, DocView};
use crate::i18n::t;
use crate::pageedit::{self, from_utf16, to_utf16};

actions!(
    doc_pages,
    [
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        LineStart,
        LineEnd,
        SelectLineStart,
        SelectLineEnd,
        DocStart,
        DocEnd,
        SelectAll,
        Backspace,
        Delete,
        Enter,
        LineBreak,
        Tab,
        Undo,
        Redo,
        Bold,
        Italic,
        Underline,
        Copy,
        Cut,
        Paste,
    ]
);

const CONTEXT: &str = "DocPages";
/// Space around and between pages, in pixels.
const GAP: f32 = 16.0;

pub fn bind_keys(cx: &mut App) {
    let cmd = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let c = Some(CONTEXT);
    let mut keys = vec![
        KeyBinding::new("left", Left, c),
        KeyBinding::new("right", Right, c),
        KeyBinding::new("up", Up, c),
        KeyBinding::new("down", Down, c),
        KeyBinding::new("shift-left", SelectLeft, c),
        KeyBinding::new("shift-right", SelectRight, c),
        KeyBinding::new("shift-up", SelectUp, c),
        KeyBinding::new("shift-down", SelectDown, c),
        KeyBinding::new("home", LineStart, c),
        KeyBinding::new("end", LineEnd, c),
        KeyBinding::new("shift-home", SelectLineStart, c),
        KeyBinding::new("shift-end", SelectLineEnd, c),
        KeyBinding::new("backspace", Backspace, c),
        KeyBinding::new("delete", Delete, c),
        KeyBinding::new("enter", Enter, c),
        KeyBinding::new("shift-enter", LineBreak, c),
        KeyBinding::new("tab", Tab, c),
        KeyBinding::new(&format!("{cmd}-a"), SelectAll, c),
        KeyBinding::new(&format!("{cmd}-z"), Undo, c),
        KeyBinding::new(&format!("{cmd}-shift-z"), Redo, c),
        KeyBinding::new(&format!("{cmd}-b"), Bold, c),
        KeyBinding::new(&format!("{cmd}-i"), Italic, c),
        KeyBinding::new(&format!("{cmd}-u"), Underline, c),
        KeyBinding::new(&format!("{cmd}-c"), Copy, c),
        KeyBinding::new(&format!("{cmd}-x"), Cut, c),
        KeyBinding::new(&format!("{cmd}-v"), Paste, c),
    ];
    if cfg!(target_os = "macos") {
        keys.extend([
            KeyBinding::new("cmd-left", LineStart, c),
            KeyBinding::new("cmd-right", LineEnd, c),
            KeyBinding::new("cmd-up", DocStart, c),
            KeyBinding::new("cmd-down", DocEnd, c),
            KeyBinding::new("cmd-shift-left", SelectLineStart, c),
            KeyBinding::new("cmd-shift-right", SelectLineEnd, c),
        ]);
    } else {
        keys.extend([
            KeyBinding::new("ctrl-home", DocStart, c),
            KeyBinding::new("ctrl-end", DocEnd, c),
            KeyBinding::new("ctrl-y", Redo, c),
        ]);
    }
    cx.bind_keys(keys);
}

/// Families for the common Chinese font names, best first, so a document
/// that asks for 宋体 on a Mac or Linux gets a Song face that is installed.
const SONG: &[&str] = &[
    "SimSun",
    "宋体",
    "Songti SC",
    "STSong",
    "Noto Serif CJK SC",
    "Noto Serif SC",
    "Source Han Serif SC",
    "AR PL UMing CN",
];
const HEI: &[&str] = &[
    "Microsoft YaHei",
    "微软雅黑",
    "SimHei",
    "黑体",
    "PingFang SC",
    "Heiti SC",
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "WenQuanYi Micro Hei",
    "WenQuanYi Zen Hei",
    "Droid Sans Fallback",
];
const FANGSONG: &[&str] = &["FangSong", "仿宋", "FangSong_GB2312", "STFangsong"];
const KAI: &[&str] = &[
    "KaiTi",
    "楷体",
    "KaiTi_GB2312",
    "Kaiti SC",
    "STKaiti",
    "AR PL UKai CN",
];
const SERIF: &[&str] = &[
    "Times New Roman",
    "Liberation Serif",
    "Tinos",
    "DejaVu Serif",
    "Noto Serif",
];
const SANS: &[&str] = &[
    "Calibri",
    "Carlito",
    "Arial",
    "Helvetica",
    "Liberation Sans",
    "Arimo",
    "DejaVu Sans",
    "Noto Sans",
];

/// Measures and paints with GPUI's text system, picking installed faces
/// for the families a document asks for.
pub struct GpuiShaper {
    system: Arc<WindowTextSystem>,
    installed: HashSet<String>,
    fonts: RefCell<HashMap<(String, bool, bool, bool), Font>>,
}

impl GpuiShaper {
    pub fn new(system: Arc<WindowTextSystem>) -> Self {
        let installed = system.all_font_names().into_iter().collect();
        Self {
            system,
            installed,
            fonts: RefCell::new(HashMap::new()),
        }
    }

    fn first(&self, names: &[&str]) -> Option<String> {
        names
            .iter()
            .find(|n| self.installed.contains(**n))
            .map(|n| n.to_string())
    }

    fn family(&self, key: &FontKey) -> String {
        let wanted = key.family.as_str();
        if !wanted.is_empty() && self.installed.contains(wanted) {
            return wanted.to_string();
        }
        let lower = wanted.to_lowercase();
        let class: &[&str] = if wanted.contains("仿宋") || lower.contains("fangsong") {
            FANGSONG
        } else if wanted.contains('楷') || lower.contains("kai") {
            KAI
        } else if wanted.contains('宋') || lower.contains("song") || lower.contains("ming") {
            SONG
        } else if wanted.contains('黑')
            || wanted.contains("雅黑")
            || lower.contains("hei")
            || lower.contains("yahei")
            || lower.contains("dengxian")
            || wanted.contains("等线")
            || key.cjk
        {
            HEI
        } else if lower.contains("times")
            || lower.contains("serif")
            || lower.contains("georgia")
            || lower.contains("cambria")
        {
            SERIF
        } else {
            SANS
        };
        self.first(class)
            .or_else(|| self.first(if key.cjk { SONG } else { SANS }))
            .or_else(|| self.first(HEI))
            .unwrap_or_else(|| ".SystemUIFont".into())
    }

    pub fn font(&self, key: &FontKey) -> Font {
        let id = (key.family.clone(), key.bold, key.italic, key.cjk);
        if let Some(font) = self.fonts.borrow().get(&id) {
            return font.clone();
        }
        let mut fallbacks: Vec<String> = Vec::new();
        for list in [HEI, SONG] {
            if let Some(f) = self.first(list) {
                fallbacks.push(f);
            }
        }
        let font = Font {
            family: self.family(key).into(),
            features: Default::default(),
            fallbacks: Some(FontFallbacks::from_fonts(fallbacks)),
            weight: if key.bold {
                FontWeight::BOLD
            } else {
                FontWeight::NORMAL
            },
            style: if key.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
        };
        self.fonts.borrow_mut().insert(id, font.clone());
        font
    }

    fn run(&self, key: &FontKey, len: usize, color: Hsla) -> TextRun {
        TextRun {
            len,
            font: self.font(key),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }
}

impl Shaper for GpuiShaper {
    fn shape(&self, text: &str, key: &FontKey) -> Vec<atrium_layout::Cluster> {
        if text.is_empty() {
            return Vec::new();
        }
        let run = self.run(key, text.len(), Hsla::black());
        let line = self.system.layout_line(text, px(key.size), &[run], None);
        // Where each character starts: the first glyph that begins at it.
        let mut x_at: HashMap<usize, f32> = HashMap::new();
        for r in &line.runs {
            for g in &r.glyphs {
                x_at.entry(g.index).or_insert(f32::from(g.position.x));
            }
        }
        let starts: Vec<(usize, f32)> = text
            .char_indices()
            .filter_map(|(i, _)| x_at.get(&i).map(|x| (i, *x)))
            .collect();
        let mut starts = if starts.first().map(|s| s.0) == Some(0) {
            starts
        } else {
            let mut v = vec![(0, 0.0)];
            v.extend(starts);
            v
        };
        starts.dedup_by_key(|s| s.0);
        let width = f32::from(line.width);
        (0..starts.len())
            .map(|k| atrium_layout::Cluster {
                start: starts[k].0,
                advance: starts.get(k + 1).map(|n| n.1).unwrap_or(width) - starts[k].1,
            })
            .collect()
    }

    fn metrics(&self, key: &FontKey) -> Metrics {
        let id = self.system.resolve_font(&self.font(key));
        let size = px(key.size);
        Metrics {
            ascent: f32::from(self.system.ascent(id, size)).abs(),
            descent: f32::from(self.system.descent(id, size)).abs(),
            line_gap: 0.0,
        }
    }
}

/// One edit as undo keeps it.
struct Step {
    ops: Vec<Op>,
    /// The caret and anchor before the edit.
    before: (Pos, Pos),
    /// Typed characters, and where the caret ended: typing in a row undoes as one step.
    typed: Option<(Pos, std::time::Instant)>,
}

/// Text the input method is still composing.
struct Compose {
    para: u64,
    /// The paragraph before composing began.
    base: Vec<Inline>,
    at: usize,
    /// The composed text's byte length.
    len: usize,
    /// Undoes what composing replaced, such as a selection.
    undo: Vec<Op>,
    before: (Pos, Pos),
}

pub struct Pages {
    pub layout: Layout,
    shaper: Rc<GpuiShaper>,
    pub caret: Pos,
    pub anchor: Pos,
    goal_x: Option<f32>,
    /// The zoom you picked; pages fit the width until then.
    pub zoom: Option<f32>,
    focus: FocusHandle,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// Marks for the next typed text, set by a format key without a selection.
    typing: Option<Marks>,
    compose: Option<Compose>,
    /// The edit in progress is plain typing.
    typing_now: bool,
    /// Each page's bounds on screen as last painted.
    frames: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    width: Rc<Cell<f32>>,
    dragging: bool,
}

fn color(c: Rgb) -> Hsla {
    rgb(c.0).into()
}

fn lay(office: &Office, shaper: &dyn Shaper) -> Layout {
    match office {
        Office::Word(doc) => atrium_layout::layout(doc, shaper),
        Office::Slides(deck) => atrium_layout::layout_deck(deck, shaper),
        Office::Sheet(_) => Layout::default(),
    }
}

impl Pages {
    pub fn new(office: &Office, window: &Window, cx: &mut Context<DocView>) -> Self {
        let shaper = Rc::new(GpuiShaper::new(window.text_system().clone()));
        let layout = lay(office, shaper.as_ref());
        let start = layout.ends().map(|(a, _)| a).unwrap_or(Pos::new(0, 0));
        Self {
            layout,
            shaper,
            caret: start,
            anchor: start,
            goal_x: None,
            zoom: None,
            focus: cx.focus_handle(),
            undo: Vec::new(),
            redo: Vec::new(),
            typing: None,
            compose: None,
            typing_now: false,
            frames: Rc::new(RefCell::new(Vec::new())),
            width: Rc::new(Cell::new(0.0)),
            dragging: false,
        }
    }

    fn scale(&self) -> f32 {
        if let Some(z) = self.zoom {
            return z;
        }
        let widest = self
            .layout
            .pages
            .iter()
            .map(|p| p.width)
            .fold(0.0, f32::max);
        let avail = self.width.get() - GAP * 2.0;
        if widest <= 0.0 || avail <= 0.0 {
            1.0
        } else {
            (avail / widest).clamp(0.25, 2.0)
        }
    }

    fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }
}

impl DocView {
    fn pages_mut(&mut self) -> Option<&mut Pages> {
        self.pages.as_mut()
    }

    fn relayout(&mut self) {
        let Some(p) = self.pages.as_mut() else { return };
        p.layout = lay(&self.doc.office, p.shaper.as_ref());
    }

    /// Records an edit's undo, lays out again and tells the panel.
    fn landed(&mut self, ops: Vec<Op>, before: (Pos, Pos), caret: Pos, cx: &mut Context<Self>) {
        self.relayout();
        if let Some(p) = self.pages_mut() {
            if !ops.is_empty() {
                let typing = std::mem::take(&mut p.typing_now);
                let now = std::time::Instant::now();
                match p.undo.last_mut() {
                    // Typing on from where the last typing stopped joins that step.
                    Some(last)
                        if typing
                            && last.typed.is_some_and(|(at, when)| {
                                at == before.0
                                    && before.0 == before.1
                                    && now.duration_since(when).as_secs() < 3
                            }) =>
                    {
                        let mut joined = ops;
                        joined.append(&mut last.ops);
                        last.ops = joined;
                        last.typed = Some((caret, now));
                    }
                    _ => p.undo.push(Step {
                        ops,
                        before,
                        typed: typing.then_some((caret, now)),
                    }),
                }
                p.redo.clear();
            }
            p.caret = caret;
            p.anchor = caret;
            p.goal_x = None;
        }
        cx.emit(DocEvent::Edited);
        cx.notify();
    }

    fn refuse(&mut self, why: String, cx: &mut Context<Self>) {
        cx.emit(DocEvent::Refused(why));
        cx.notify();
    }

    /// Replaces the selection with paragraphs of inlines.
    fn put(&mut self, insert: Vec<Vec<Inline>>, cx: &mut Context<Self>) {
        let Some(p) = self.pages.as_ref() else { return };
        let (a, b, layout) = (p.anchor, p.caret, p.layout.clone());
        let before = (p.caret, p.anchor);
        match pageedit::replace(&mut self.doc, &layout, a, b, insert) {
            Ok((caret, ops)) => self.landed(ops, before, caret, cx),
            Err(why) => self.refuse(why, cx),
        }
    }

    fn typing_marks(&self) -> Marks {
        let Some(p) = self.pages.as_ref() else {
            return Marks::default();
        };
        if let Some(m) = &p.typing {
            return m.clone();
        }
        let (from, _) = ordered(&p.layout, p.anchor, p.caret);
        pageedit::find(&self.doc.office, from.para)
            .map(|(para, _)| pageedit::marks_at(&para.inlines, from.offset))
            .unwrap_or_default()
    }

    pub fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        let marks = self.typing_marks();
        if let Some(p) = self.pages_mut() {
            // Letters and spaces group for undo; a new line starts a new step.
            p.typing_now = !text.contains('\n') && text.chars().count() <= 2;
        }
        self.put(pageedit::pieces(text, &marks), cx);
        if let Some(p) = self.pages_mut() {
            p.typing = None;
        }
    }

    fn move_to(&mut self, to: Option<Pos>, select: bool, keep_goal: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages_mut() else { return };
        let Some(to) = to else { return };
        p.caret = to;
        if !select {
            p.anchor = to;
        }
        if !keep_goal {
            p.goal_x = None;
        }
        p.typing = None;
        cx.notify();
    }

    fn arrow(&mut self, forward: bool, select: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages.as_ref() else { return };
        // Without shift, an arrow collapses a selection to its side.
        if p.has_selection() && !select {
            let (from, to) = ordered(&p.layout, p.anchor, p.caret);
            self.move_to(Some(if forward { to } else { from }), false, false, cx);
            return;
        }
        let to = p.layout.step(p.caret, forward);
        self.move_to(to, select, false, cx);
    }

    fn vertical(&mut self, down: bool, select: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages_mut() else { return };
        let goal = p
            .goal_x
            .or_else(|| p.layout.caret(p.caret).map(|c| c.x))
            .unwrap_or(0.0);
        p.goal_x = Some(goal);
        let to = p.layout.vertical(p.caret, down, goal);
        self.move_to(to, select, true, cx);
    }

    fn erase(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages_mut() else { return };
        if !p.has_selection() {
            let Some(to) = p.layout.step(p.caret, forward) else {
                return;
            };
            p.anchor = to;
        }
        self.put(vec![vec![]], cx);
    }

    fn history(&mut self, redo: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages_mut() else { return };
        let step = if redo { p.redo.pop() } else { p.undo.pop() };
        let Some(step) = step else { return };
        let now = (p.caret, p.anchor);
        match pageedit::apply_all(&mut self.doc, step.ops) {
            Ok(inverse) => {
                self.relayout();
                if let Some(p) = self.pages_mut() {
                    let back = Step {
                        ops: inverse,
                        before: now,
                        typed: None,
                    };
                    if redo {
                        p.undo.push(back);
                    } else {
                        p.redo.push(back);
                    }
                    p.caret = step.before.0;
                    p.anchor = step.before.1;
                    if p.layout.line_of(p.caret).is_none() {
                        let start = p.layout.ends().map(|e| e.0);
                        if let Some(s) = start {
                            p.caret = s;
                            p.anchor = s;
                        }
                    }
                }
                cx.emit(DocEvent::Edited);
                cx.notify();
            }
            Err(why) => self.refuse(why, cx),
        }
    }

    fn toggle(&mut self, which: fn(&mut Marks) -> &mut bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages.as_ref() else { return };
        if !p.has_selection() {
            let mut marks = self.typing_marks();
            let flag = which(&mut marks);
            *flag = !*flag;
            if let Some(p) = self.pages_mut() {
                p.typing = Some(marks);
            }
            cx.notify();
            return;
        }
        let (a, b, layout) = (p.anchor, p.caret, p.layout.clone());
        let before = (p.caret, p.anchor);
        let on = !pageedit::all_between(&self.doc.office, &layout, a, b, &|m| {
            let mut m = m.clone();
            *which(&mut m)
        });
        match pageedit::restyle_between(&mut self.doc, &layout, a, b, &|m| *which(m) = on) {
            Ok(ops) => {
                self.landed(ops, before, b, cx);
                if let Some(p) = self.pages_mut() {
                    p.anchor = a;
                }
            }
            Err(why) => self.refuse(why, cx),
        }
    }

    fn align(&mut self, align: Align, cx: &mut Context<Self>) {
        let Some(p) = self.pages.as_ref() else { return };
        if !matches!(self.doc.office, Office::Word(_)) {
            return;
        }
        let paras = {
            let (from, to) = ordered(&p.layout, p.anchor, p.caret);
            if from.para == to.para {
                vec![from.para]
            } else {
                p.layout.paras_between(from, to)
            }
        };
        let before = (p.caret, p.anchor);
        let (caret, anchor) = (p.caret, p.anchor);
        let ops = paras
            .into_iter()
            .map(|para| Op::Word(WordOp::SetAlign { para, align }))
            .collect();
        match pageedit::apply_all(&mut self.doc, ops) {
            Ok(undo) => {
                self.landed(undo, before, caret, cx);
                if let Some(p) = self.pages_mut() {
                    p.anchor = anchor;
                }
            }
            Err(why) => self.refuse(why, cx),
        }
    }

    fn copy(&mut self, cut: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages.as_ref() else { return };
        if !p.has_selection() {
            return;
        }
        let text = pageedit::text_between(&self.doc.office, &p.layout, p.anchor, p.caret);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        if cut {
            self.put(vec![vec![]], cx);
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.type_text(&text, cx);
        }
    }

    fn click(
        &mut self,
        page: usize,
        at: Point<Pixels>,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(p) = self.pages_mut() else { return };
        window.focus(&p.focus, cx);
        let Some(pos) = hit(p, page, at) else { return };
        if ev.click_count >= 2 {
            // Double click: the run of letters or the Chinese phrase around it.
            let (a, b) = word_around(&self.doc.office, pos, ev.click_count >= 3);
            if let Some(p) = self.pages_mut() {
                p.anchor = a;
                p.caret = b;
            }
            cx.notify();
            return;
        }
        let select = ev.modifiers.shift;
        if let Some(p) = self.pages_mut() {
            p.dragging = true;
        }
        self.move_to(Some(pos), select, false, cx);
    }

    fn drag(&mut self, page: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(p) = self.pages_mut() else { return };
        if !p.dragging {
            return;
        }
        if let Some(pos) = hit(p, page, at)
            && pos != p.caret
        {
            p.caret = pos;
            cx.notify();
        }
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let me = cx.entity();
        let word = matches!(self.doc.office, Office::Word(_));
        let button = |id: &'static str, icon: IconName, tip: &'static str, on: bool| {
            IconButton::new(id, icon)
                .variant(if on {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                })
                .size(ControlSize::Sm)
                .tooltip(t(tip))
        };
        let marks = self.typing_marks();
        let selection_all = |test: fn(&Marks) -> bool| {
            let Some(p) = self.pages.as_ref() else {
                return false;
            };
            if p.has_selection() {
                pageedit::all_between(&self.doc.office, &p.layout, p.anchor, p.caret, &test)
            } else {
                test(&marks)
            }
        };
        let (b, i, u) = (
            selection_all(|m| m.bold),
            selection_all(|m| m.italic),
            selection_all(|m| m.underline),
        );
        let act = |f: fn(&mut DocView, &mut Context<DocView>)| {
            let me = me.clone();
            move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                me.update(cx, |d, cx| {
                    f(d, cx);
                    if let Some(p) = d.pages.as_ref() {
                        window.focus(&p.focus, cx);
                    }
                })
            }
        };
        let can_undo = self.pages.as_ref().is_some_and(|p| !p.undo.is_empty());
        let can_redo = self.pages.as_ref().is_some_and(|p| !p.redo.is_empty());
        let zoom = self.pages.as_ref().map(|p| p.scale()).unwrap_or(1.0);
        div()
            .flex()
            .items_center()
            .gap_0p5()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(colors.border)
            .child(
                button("pg-undo", IconName::Undo2, "Undo", false)
                    .disabled(!can_undo)
                    .on_click(act(|d, cx| d.history(false, cx))),
            )
            .child(
                button("pg-redo", IconName::Redo2, "Redo", false)
                    .disabled(!can_redo)
                    .on_click(act(|d, cx| d.history(true, cx))),
            )
            .child(div().w_px().h_4().mx_1().bg(colors.border))
            .child(
                button("pg-bold", IconName::Bold, "Bold", b)
                    .on_click(act(|d, cx| d.toggle(|m| &mut m.bold, cx))),
            )
            .child(
                button("pg-italic", IconName::Italic, "Italic", i)
                    .on_click(act(|d, cx| d.toggle(|m| &mut m.italic, cx))),
            )
            .child(
                button("pg-underline", IconName::Underline, "Underline", u)
                    .on_click(act(|d, cx| d.toggle(|m| &mut m.underline, cx))),
            )
            .child(
                button(
                    "pg-strike",
                    IconName::Strikethrough,
                    "Strikethrough",
                    selection_all(|m| m.strike),
                )
                .on_click(act(|d, cx| d.toggle(|m| &mut m.strike, cx))),
            )
            .when(word, |el| {
                el.child(div().w_px().h_4().mx_1().bg(colors.border))
                    .child(
                        button("pg-left", IconName::AlignLeft, "Align left", false)
                            .on_click(act(|d, cx| d.align(Align::Left, cx))),
                    )
                    .child(
                        button("pg-center", IconName::AlignCenter, "Center", false)
                            .on_click(act(|d, cx| d.align(Align::Center, cx))),
                    )
                    .child(
                        button("pg-right", IconName::AlignRight, "Align right", false)
                            .on_click(act(|d, cx| d.align(Align::Right, cx))),
                    )
                    .child(
                        button("pg-justify", IconName::AlignJustify, "Justify", false)
                            .on_click(act(|d, cx| d.align(Align::Justify, cx))),
                    )
            })
            .child(div().flex_1())
            .child({
                let me = me.clone();
                button("pg-outline", IconName::List, "Outline", false).on_click(
                    move |_, window, cx| me.update(cx, |d, cx| d.set_outline(true, window, cx)),
                )
            })
            .child(
                button("pg-zoom-out", IconName::ZoomOut, "Zoom out", false)
                    .on_click(act(|d, cx| d.zoom_by(1.0 / 1.25, cx))),
            )
            .child(
                div()
                    .min_w_10()
                    .text_center()
                    .text_size(theme.text_size(ely_gpui_component::theme::TextSize::Xs))
                    .text_color(colors.fg_muted)
                    .child(format!("{:.0}%", zoom * 100.0)),
            )
            .child(
                button("pg-zoom-in", IconName::ZoomIn, "Zoom in", false)
                    .on_click(act(|d, cx| d.zoom_by(1.25, cx))),
            )
            .child(
                button("pg-fit", IconName::Maximize2, "Fit to width", false).on_click(act(
                    |d, cx| {
                        if let Some(p) = d.pages_mut() {
                            p.zoom = None;
                        }
                        cx.notify();
                    },
                )),
            )
    }

    fn zoom_by(&mut self, factor: f32, cx: &mut Context<Self>) {
        if let Some(p) = self.pages_mut() {
            p.zoom = Some((p.scale() * factor).clamp(0.25, 4.0));
        }
        cx.notify();
    }

    /// The pages, the toolbar above them, and the editing handlers.
    pub fn pages_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(p) = self.pages.as_ref() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let colors = theme.colors.clone();
        let shadow = theme.elevation(Elevation::Raised);
        let z = p.scale();
        let me = cx.entity();
        let focused = p.focus.is_focused(window);
        let caret = (focused && !p.has_selection())
            .then(|| p.layout.caret(p.caret))
            .flatten();
        let selection = p.layout.selection(p.anchor, p.caret);
        let marked = p.compose.as_ref().map(|c| {
            p.layout
                .selection(Pos::new(c.para, c.at), Pos::new(c.para, c.at + c.len))
        });
        let frames = p.frames.clone();
        frames
            .borrow_mut()
            .resize(p.layout.pages.len(), Bounds::default());
        let width = p.width.clone();
        let focus = p.focus.clone();
        let shaper = p.shaper.clone();
        let accent = colors.accent;
        let select_color = colors.accent.opacity(0.25);

        let mut list = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(GAP))
            .py(px(GAP));
        for (i, page) in p.layout.pages.iter().enumerate() {
            let page = page.clone();
            let frames = frames.clone();
            let shaper = shaper.clone();
            let sel: Vec<_> = selection.iter().filter(|s| s.0 == i).map(|s| s.1).collect();
            let marks: Vec<_> = marked
                .iter()
                .flatten()
                .filter(|s| s.0 == i)
                .map(|s| s.1)
                .collect();
            let caret_here = caret.filter(|c| c.page == i);
            let pictures: Vec<(atrium_layout::Rect, Option<std::path::PathBuf>)> = page
                .items
                .iter()
                .filter_map(|it| match it {
                    Item::Picture { rect, path } => Some((*rect, path.clone())),
                    _ => None,
                })
                .collect();
            let bg = page.background.map(color).unwrap_or(colors.paper);
            let painter = canvas(
                move |bounds, _, _| bounds,
                move |bounds, _, window, cx| {
                    if let Some(slot) = frames.borrow_mut().get_mut(i) {
                        *slot = bounds;
                    }
                    let o = bounds.origin;
                    let at = |x: f32, y: f32| point(o.x + px(x * z), o.y + px(y * z));
                    let rect = |r: &atrium_layout::Rect| {
                        Bounds::new(at(r.x, r.y), size(px(r.width * z), px(r.height * z)))
                    };
                    for r in &sel {
                        window.paint_quad(gpui::fill(rect(r), select_color));
                    }
                    for item in &page.items {
                        match item {
                            Item::Fill { rect: r, color: c } => {
                                window.paint_quad(gpui::fill(rect(r), color(*c)))
                            }
                            Item::Rule {
                                from,
                                to,
                                width,
                                color: c,
                            } => {
                                let w = (width * z).max(1.0);
                                let (x0, y0) = (from.0.min(to.0), from.1.min(to.1));
                                let (x1, y1) = (from.0.max(to.0), from.1.max(to.1));
                                let b = Bounds::new(
                                    at(x0, y0),
                                    size(px(((x1 - x0) * z).max(w)), px(((y1 - y0) * z).max(w))),
                                );
                                window.paint_quad(gpui::fill(b, color(*c)));
                            }
                            Item::Text {
                                x,
                                baseline,
                                text,
                                font,
                                color: c,
                                ..
                            } => {
                                let run = shaper.run(font, text.len(), color(*c));
                                let size = px(font.size * z);
                                let line = window.text_system().shape_line(
                                    SharedString::from(text.clone()),
                                    size,
                                    &[run],
                                    None,
                                );
                                let ascent = line.ascent;
                                let height = ascent + line.descent;
                                let origin =
                                    point(at(*x, *baseline).x, at(*x, *baseline).y - ascent);
                                let _ =
                                    line.paint(origin, height, TextAlign::Left, None, window, cx);
                            }
                            Item::Picture { .. } => {}
                            Item::Path {
                                segments,
                                fill,
                                stroke,
                                ..
                            } => {
                                let build = |mut b: gpui::PathBuilder| {
                                    for seg in segments {
                                        match *seg {
                                            atrium_layout::Seg::Move(x, y) => b.move_to(at(x, y)),
                                            atrium_layout::Seg::Line(x, y) => b.line_to(at(x, y)),
                                            atrium_layout::Seg::Quad(cx_, cy, x, y) => {
                                                b.curve_to(at(x, y), at(cx_, cy))
                                            }
                                            atrium_layout::Seg::Cubic(a, b2, c, d, x, y) => {
                                                b.cubic_bezier_to(at(x, y), at(a, b2), at(c, d))
                                            }
                                            atrium_layout::Seg::Close => b.close(),
                                        }
                                    }
                                    b.build().ok()
                                };
                                if let Some((c, a)) = fill
                                    && let Some(path) = build(gpui::PathBuilder::fill())
                                {
                                    window.paint_path(path, color(*c).opacity(*a as f32 / 255.0));
                                }
                                if let Some(pen) = stroke
                                    && let Some(path) = build(gpui::PathBuilder::stroke(px((pen
                                        .width
                                        * z)
                                        .max(1.0))))
                                {
                                    window.paint_path(
                                        path,
                                        color(pen.color).opacity(pen.alpha as f32 / 255.0),
                                    );
                                }
                            }
                            Item::Gradient {
                                rect: r,
                                angle,
                                stops,
                            } => {
                                if let (Some(first), Some(last)) = (stops.first(), stops.last()) {
                                    let stop = |s: &(f32, Rgb, u8)| {
                                        gpui::linear_color_stop(
                                            color(s.1).opacity(s.2 as f32 / 255.0),
                                            s.0,
                                        )
                                    };
                                    // GPUI's angle runs from bottom to top; the model's from left to right.
                                    let bg = gpui::linear_gradient(
                                        angle + 90.0,
                                        stop(first),
                                        stop(last),
                                    );
                                    window.paint_quad(gpui::fill(rect(r), bg));
                                }
                            }
                        }
                    }
                    for r in &marks {
                        let b = rect(r);
                        let under = Bounds::new(
                            point(b.origin.x, b.origin.y + b.size.height - px(2.0)),
                            size(b.size.width, px(1.5)),
                        );
                        window.paint_quad(gpui::fill(under, accent));
                    }
                    if let Some(c) = caret_here {
                        let b = Bounds::new(at(c.x, c.top), size(px(2.0), px(c.height * z)));
                        window.paint_quad(gpui::fill(b, accent));
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full();
            let (down, mv) = (me.clone(), me.clone());
            let frames_down = p.frames.clone();
            let frames_move = p.frames.clone();
            let mut sheet = div()
                .id(("doc-page", i))
                .relative()
                .flex_none()
                .w(px(page.width * z))
                .h(px(page.height * z))
                .bg(bg)
                .shadow(shadow.clone())
                .cursor_text()
                .child(painter)
                .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, window, cx| {
                    let local = local(&frames_down, i, ev.position, z);
                    down.update(cx, |d, cx| d.click(i, local, ev, window, cx));
                })
                .on_mouse_move(move |ev: &MouseMoveEvent, _, cx| {
                    if ev.pressed_button == Some(MouseButton::Left) {
                        let local = local(&frames_move, i, ev.position, z);
                        mv.update(cx, |d, cx| d.drag(i, local, cx));
                    }
                });
            for (r, path) in pictures {
                let frame = div()
                    .absolute()
                    .left(px(r.x * z))
                    .top(px(r.y * z))
                    .w(px(r.width * z))
                    .h(px(r.height * z));
                sheet = sheet.child(match path {
                    Some(path) => frame.child(img(path).size_full()).into_any_element(),
                    None => frame
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.hover)
                        .into_any_element(),
                });
            }
            list = list.child(sheet);
        }

        let input = me.clone();
        let measure = canvas(
            move |bounds, _, _| bounds,
            move |bounds, _, window, cx| {
                let w = f32::from(bounds.size.width);
                if (width.get() - w).abs() > 0.5 {
                    width.set(w);
                    input.update(cx, |_, cx| cx.notify());
                }
                window.handle_input(&focus, ElementInputHandler::new(bounds, input.clone()), cx);
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let p = self.pages.as_ref().expect("checked above");
        let up = me.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.toolbar(cx))
            .child(
                div()
                    .id("doc-pages")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .bg(colors.sunken)
                    .track_focus(&p.focus)
                    .key_context(CONTEXT)
                    .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                        up.update(cx, |d, _| {
                            if let Some(p) = d.pages_mut() {
                                p.dragging = false;
                            }
                        })
                    })
                    .on_action(cx.listener(|d, _: &Left, _, cx| d.arrow(false, false, cx)))
                    .on_action(cx.listener(|d, _: &Right, _, cx| d.arrow(true, false, cx)))
                    .on_action(cx.listener(|d, _: &SelectLeft, _, cx| d.arrow(false, true, cx)))
                    .on_action(cx.listener(|d, _: &SelectRight, _, cx| d.arrow(true, true, cx)))
                    .on_action(cx.listener(|d, _: &Up, _, cx| d.vertical(false, false, cx)))
                    .on_action(cx.listener(|d, _: &Down, _, cx| d.vertical(true, false, cx)))
                    .on_action(cx.listener(|d, _: &SelectUp, _, cx| d.vertical(false, true, cx)))
                    .on_action(cx.listener(|d, _: &SelectDown, _, cx| d.vertical(true, true, cx)))
                    .on_action(cx.listener(|d, _: &LineStart, _, cx| d.line_edge(false, false, cx)))
                    .on_action(cx.listener(|d, _: &LineEnd, _, cx| d.line_edge(true, false, cx)))
                    .on_action(
                        cx.listener(|d, _: &SelectLineStart, _, cx| d.line_edge(false, true, cx)),
                    )
                    .on_action(
                        cx.listener(|d, _: &SelectLineEnd, _, cx| d.line_edge(true, true, cx)),
                    )
                    .on_action(cx.listener(|d, _: &DocStart, _, cx| d.doc_edge(false, cx)))
                    .on_action(cx.listener(|d, _: &DocEnd, _, cx| d.doc_edge(true, cx)))
                    .on_action(cx.listener(|d, _: &SelectAll, _, cx| d.select_all(cx)))
                    .on_action(cx.listener(|d, _: &Backspace, _, cx| d.erase(false, cx)))
                    .on_action(cx.listener(|d, _: &Delete, _, cx| d.erase(true, cx)))
                    .on_action(cx.listener(|d, _: &Enter, _, cx| d.type_text("\n", cx)))
                    .on_action(
                        cx.listener(|d, _: &LineBreak, _, cx| d.put(vec![vec![Inline::Break]], cx)),
                    )
                    .on_action(cx.listener(|d, _: &Tab, _, cx| d.type_text("\t", cx)))
                    .on_action(cx.listener(|d, _: &Undo, _, cx| d.history(false, cx)))
                    .on_action(cx.listener(|d, _: &Redo, _, cx| d.history(true, cx)))
                    .on_action(cx.listener(|d, _: &Bold, _, cx| d.toggle(|m| &mut m.bold, cx)))
                    .on_action(cx.listener(|d, _: &Italic, _, cx| d.toggle(|m| &mut m.italic, cx)))
                    .on_action(
                        cx.listener(|d, _: &Underline, _, cx| d.toggle(|m| &mut m.underline, cx)),
                    )
                    .on_action(cx.listener(|d, _: &Copy, _, cx| d.copy(false, cx)))
                    .on_action(cx.listener(|d, _: &Cut, _, cx| d.copy(true, cx)))
                    .on_action(cx.listener(|d, _: &Paste, _, cx| d.paste(cx)))
                    .child(measure)
                    .child(list),
            )
            .into_any_element()
    }

    fn line_edge(&mut self, end: bool, select: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages.as_ref() else { return };
        let to = if end {
            p.layout.line_end(p.caret)
        } else {
            p.layout.line_start(p.caret)
        };
        self.move_to(Some(to), select, false, cx);
    }

    fn doc_edge(&mut self, end: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pages.as_ref() else { return };
        let to = p.layout.ends().map(|(a, b)| if end { b } else { a });
        self.move_to(to, false, false, cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.pages_mut() else { return };
        if let Some((a, b)) = p.layout.ends() {
            p.anchor = a;
            p.caret = b;
        }
        cx.notify();
    }

    /// The caret's paragraph text, for the input method.
    fn ime_text(&self) -> Option<(u64, String)> {
        let p = self.pages.as_ref()?;
        let (para, _) = pageedit::find(&self.doc.office, p.caret.para)?;
        Some((para.id, para.text()))
    }

    fn compose_end(&mut self, text: Option<&str>, cx: &mut Context<Self>) {
        let Some(p) = self.pages_mut() else { return };
        let Some(c) = p.compose.take() else { return };
        let marks = pageedit::marks_at(&c.base, c.at);
        let insert: Vec<Inline> = match text {
            Some(text) => pageedit::pieces(text, &marks)
                .into_iter()
                .flatten()
                .collect(),
            None => pageedit::find(&self.doc.office, c.para)
                .map(|(para, _)| pageedit::slice(&para.inlines, c.at, c.at + c.len))
                .unwrap_or_default(),
        };
        let len = pageedit::text_len(&insert);
        let inlines = pageedit::splice(&c.base, c.at, c.at, insert);
        let mut edit = pageedit::Edit::new(&mut self.doc);
        if let Err(why) = edit.set_content(c.para, inlines) {
            self.refuse(why, cx);
            return;
        }
        drop(edit.finish());
        // Undo puts back the paragraph as it was, then what composing replaced.
        let mut ops = vec![if matches!(self.doc.office, Office::Slides(_)) {
            Op::Slides(atrium_office::SlideOp::SetContent {
                para: c.para,
                inlines: c.base.clone(),
            })
        } else {
            Op::Word(WordOp::SetContent {
                para: c.para,
                inlines: c.base.clone(),
            })
        }];
        ops.extend(c.undo);
        self.landed(ops, c.before, Pos::new(c.para, c.at + len), cx);
    }
}

fn ordered(layout: &Layout, a: Pos, b: Pos) -> (Pos, Pos) {
    if layout.compare(a, b) == std::cmp::Ordering::Greater {
        (b, a)
    } else {
        (a, b)
    }
}

/// A point in window pixels as page points.
fn local(
    frames: &Rc<RefCell<Vec<Bounds<Pixels>>>>,
    page: usize,
    at: Point<Pixels>,
    z: f32,
) -> Point<Pixels> {
    let origin = frames
        .borrow()
        .get(page)
        .map(|b| b.origin)
        .unwrap_or_default();
    point(
        px(f32::from(at.x - origin.x) / z),
        px(f32::from(at.y - origin.y) / z),
    )
}

fn hit(p: &Pages, page: usize, at: Point<Pixels>) -> Option<Pos> {
    p.layout.hit(page, f32::from(at.x), f32::from(at.y))
}

/// The word around `pos`: letters and digits, or a run of Chinese
/// characters; with `paragraph`, the whole paragraph.
fn word_around(office: &Office, pos: Pos, paragraph: bool) -> (Pos, Pos) {
    let Some((para, _)) = pageedit::find(office, pos.para) else {
        return (pos, pos);
    };
    let text = para.text();
    if paragraph {
        return (Pos::new(pos.para, 0), Pos::new(pos.para, text.len()));
    }
    let class = |c: char| {
        if c.is_alphanumeric() && c.is_ascii() {
            1
        } else if (c as u32) >= 0x3400 && c.is_alphanumeric() {
            2
        } else {
            0
        }
    };
    let at = pos.offset.min(text.len());
    let here = text[at..]
        .chars()
        .next()
        .or_else(|| text[..at].chars().last())
        .map(class)
        .unwrap_or(0);
    if here == 0 {
        return (pos, pos);
    }
    let start = text[..at]
        .char_indices()
        .rev()
        .take_while(|(_, c)| class(*c) == here)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(at);
    let end = text[at..]
        .char_indices()
        .take_while(|(_, c)| class(*c) == here)
        .last()
        .map(|(i, c)| at + i + c.len_utf8())
        .unwrap_or(at);
    (Pos::new(pos.para, start), Pos::new(pos.para, end))
}

impl Focusable for DocView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if let Some(g) = &self.grid {
            return g.focus().clone();
        }
        self.pages
            .as_ref()
            .map(|p| p.focus.clone())
            .unwrap_or_else(|| cx.focus_handle())
    }
}

impl EntityInputHandler for DocView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let (_, text) = self.ime_text()?;
        let a = from_utf16(&text, range.start);
        let b = from_utf16(&text, range.end);
        *adjusted = Some(to_utf16(&text, a)..to_utf16(&text, b));
        text.get(a..b).map(str::to_string)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let (para, text) = self.ime_text()?;
        let p = self.pages.as_ref()?;
        if let Some(c) = &p.compose {
            let end = to_utf16(&text, c.at + c.len);
            return Some(UTF16Selection {
                range: end..end,
                reversed: false,
            });
        }
        let caret = to_utf16(&text, p.caret.offset);
        let anchor = if p.anchor.para == para {
            to_utf16(&text, p.anchor.offset)
        } else {
            caret
        };
        Some(UTF16Selection {
            range: caret.min(anchor)..caret.max(anchor),
            reversed: caret < anchor,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let p = self.pages.as_ref()?;
        let c = p.compose.as_ref()?;
        let (_, text) = self.ime_text()?;
        Some(to_utf16(&text, c.at)..to_utf16(&text, c.at + c.len))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.compose_end(None, cx);
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.grid.is_some() {
            self.grid_typed(text, false, window, cx);
            return;
        }
        if self.pages.as_ref().is_some_and(|p| p.compose.is_some()) {
            self.compose_end(Some(text), cx);
            return;
        }
        if let (Some(range), Some((para, ptext))) = (range, self.ime_text())
            && let Some(p) = self.pages_mut()
        {
            p.anchor = Pos::new(para, from_utf16(&ptext, range.start));
            p.caret = Pos::new(para, from_utf16(&ptext, range.end));
        }
        self.type_text(text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        _selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.grid.is_some() {
            self.grid_typed(new_text, true, window, cx);
            return;
        }
        let composing = self.pages.as_ref().is_some_and(|p| p.compose.is_some());
        if !composing {
            // Starting: replace the selection (or the given range) with nothing first.
            if let (Some(range), Some((para, ptext))) = (range.clone(), self.ime_text())
                && let Some(p) = self.pages_mut()
            {
                p.anchor = Pos::new(para, from_utf16(&ptext, range.start));
                p.caret = Pos::new(para, from_utf16(&ptext, range.end));
            }
            let Some(p) = self.pages.as_ref() else { return };
            let before = (p.caret, p.anchor);
            let mut undo = Vec::new();
            if p.has_selection() {
                let (a, b, layout) = (p.anchor, p.caret, p.layout.clone());
                match pageedit::replace(&mut self.doc, &layout, a, b, vec![vec![]]) {
                    Ok((caret, ops)) => {
                        undo = ops;
                        self.relayout();
                        if let Some(p) = self.pages_mut() {
                            p.caret = caret;
                            p.anchor = caret;
                        }
                    }
                    Err(why) => {
                        self.refuse(why, cx);
                        return;
                    }
                }
            }
            let Some(p) = self.pages.as_ref() else { return };
            let caret = p.caret;
            let Some((para, _)) = pageedit::find(&self.doc.office, caret.para) else {
                return;
            };
            let base = para.inlines.clone();
            if let Some(p) = self.pages_mut() {
                p.compose = Some(Compose {
                    para: caret.para,
                    base,
                    at: caret.offset,
                    len: 0,
                    undo,
                    before,
                });
            }
        }
        let marks = self.typing_marks();
        let Some(p) = self.pages.as_ref() else { return };
        let Some(c) = p.compose.as_ref() else { return };
        let (para, at, base) = (c.para, c.at, c.base.clone());
        let insert: Vec<Inline> = pageedit::pieces(new_text, &marks)
            .into_iter()
            .flatten()
            .collect();
        let len = pageedit::text_len(&insert);
        let inlines = pageedit::splice(&base, at, at, insert);
        let mut edit = pageedit::Edit::new(&mut self.doc);
        if let Err(why) = edit.set_content(para, inlines) {
            if let Some(p) = self.pages_mut() {
                p.compose = None;
            }
            self.refuse(why, cx);
            return;
        }
        drop(edit.finish());
        self.relayout();
        if let Some(p) = self.pages_mut() {
            if let Some(c) = p.compose.as_mut() {
                c.len = len;
            }
            p.caret = Pos::new(para, at + len);
            p.anchor = p.caret;
            if new_text.is_empty() {
                p.compose = None;
            }
        }
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (para, text) = self.ime_text()?;
        let p = self.pages.as_ref()?;
        let caret = p
            .layout
            .caret(Pos::new(para, from_utf16(&text, range.start)))?;
        let frame = *p.frames.borrow().get(caret.page)?;
        let z = p.scale();
        Some(Bounds::new(
            point(
                frame.origin.x + px(caret.x * z),
                frame.origin.y + px(caret.top * z),
            ),
            size(px(2.0), px(caret.height * z)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        at: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let (para, text) = self.ime_text()?;
        let p = self.pages.as_ref()?;
        let frames = p.frames.borrow();
        let (page, frame) = frames.iter().enumerate().find(|(_, b)| b.contains(&at))?;
        let z = p.scale();
        let pos = p.layout.hit(
            page,
            f32::from(at.x - frame.origin.x) / z,
            f32::from(at.y - frame.origin.y) / z,
        )?;
        (pos.para == para).then(|| to_utf16(&text, pos.offset))
    }
}
