//! How a document looks: fonts, sizes, spacing, pages, tables and shapes,
//! resolved so that a layout engine can draw the document without reading
//! the file's styles itself.
//!
//! Lengths are in twips, Word's own unit: twentieths of a point. Values are
//! the *effective* ones, after the document defaults, the style chain and
//! direct formatting are applied; the loader resolves them, the writer
//! writes back only what differs from the paragraph's style.

/// A length in twentieths of a point.
pub type Twips = i32;

/// Twips in one point.
pub const TWIPS_PER_PT: i32 = 20;

/// A colour as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u32);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0x000000);
    pub const WHITE: Rgb = Rgb(0xFFFFFF);
}

/// The fonts a run asks for: one for Latin text, one for Chinese,
/// Japanese and Korean. Empty means the document's default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Fonts {
    pub latin: String,
    pub east_asia: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Baseline {
    #[default]
    Normal,
    Superscript,
    Subscript,
}

/// How lines of a paragraph are spaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineSpacing {
    /// A multiple of the font's line height, in 240ths: 240 is single, 360 is 1.5.
    Auto(u16),
    /// At least this tall.
    AtLeast(Twips),
    /// Exactly this tall, cutting what does not fit.
    Exact(Twips),
}

impl Default for LineSpacing {
    fn default() -> Self {
        LineSpacing::Auto(240)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabKind {
    #[default]
    Left,
    Center,
    Right,
    /// Lines up the decimal point.
    Decimal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabStop {
    /// From the paragraph's left indent's origin: the text area's left edge.
    pub at: Twips,
    pub kind: TabKind,
    /// The character that fills the gap, such as '.' or '-'.
    pub leader: Option<char>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Border {
    pub width: Twips,
    pub color: Rgb,
    /// The gap between the border and the text.
    pub space: Twips,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Borders {
    pub top: Option<Border>,
    pub bottom: Option<Border>,
    pub left: Option<Border>,
    pub right: Option<Border>,
}

/// How a list level writes its number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NumFormat {
    /// 1, 2, 3
    Decimal,
    /// 01, 02, 03
    DecimalZero,
    /// a, b, c
    LowerLetter,
    UpperLetter,
    /// i, ii, iii
    LowerRoman,
    UpperRoman,
    /// 一, 二, 三
    Chinese,
    /// 壹, 贰, 叁
    ChineseLegal,
    /// ①, ②, ③
    Circled,
    /// The same mark on every item.
    Bullet(String),
    /// No number at all.
    None,
}

/// A paragraph's place in a numbered or bulleted list. The engine counts
/// the items of each `list` itself, so numbers follow edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListLook {
    /// Which list this item counts in: Word's `numId`.
    pub list: u32,
    /// 0 to 8.
    pub level: u8,
    /// The label's pattern, with `%1` … `%9` standing for each level's
    /// number, as in "%1." or "（%2）" or "%1.%2".
    pub pattern: String,
    /// Each level's format, up to and including `level`.
    pub formats: Vec<NumFormat>,
    /// Each level's first number, up to and including `level`.
    pub starts: Vec<u32>,
    /// How the label's run looks; `None` takes the paragraph's first run.
    pub marks: Option<Box<crate::Marks>>,
    /// What follows the label: a tab to the next stop, a space, or nothing.
    pub suffix: LabelSuffix,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LabelSuffix {
    #[default]
    Tab,
    Space,
    Nothing,
}

/// How a paragraph is laid out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParaLook {
    pub before: Twips,
    pub after: Twips,
    pub line: LineSpacing,
    /// Space before is dropped at the top of a page, and between paragraphs
    /// of the same style when the style says so (`contextualSpacing`).
    pub contextual: bool,
    pub left: Twips,
    pub right: Twips,
    /// Positive indents the first line; negative hangs it.
    pub first_line: Twips,
    pub keep_next: bool,
    pub keep_lines: bool,
    pub page_break_before: bool,
    /// Avoid a single line of the paragraph alone at the top or bottom of a page.
    pub widow_control: bool,
    pub tabs: Vec<TabStop>,
    pub shading: Option<Rgb>,
    pub borders: Borders,
    pub list: Option<ListLook>,
    /// Lines sit on the section's line grid (`docGrid`), as Chinese documents often do.
    pub snap_to_grid: bool,
    /// Word's "allow punctuation to hang": a closing mark may sit past the
    /// right edge instead of moving to the next line (`overflowPunct`).
    pub overflow_punct: bool,
    /// Add a quarter em between East Asian text and Latin text (`autoSpaceDE`).
    pub auto_space_de: bool,
    /// Add a quarter em between East Asian text and numbers (`autoSpaceDN`).
    pub auto_space_dn: bool,
    /// Justify every line, including the last (`jc="distribute"`).
    pub distribute: bool,
    /// How the paragraph mark looks: sets the height of an empty paragraph,
    /// and what typing into it starts with.
    pub mark: crate::Marks,
}

impl Default for ParaLook {
    fn default() -> Self {
        Self {
            before: 0,
            after: 0,
            line: LineSpacing::default(),
            contextual: false,
            left: 0,
            right: 0,
            first_line: 0,
            keep_next: false,
            keep_lines: false,
            page_break_before: false,
            widow_control: false,
            tabs: Vec::new(),
            shading: None,
            borders: Borders::default(),
            list: None,
            snap_to_grid: false,
            overflow_punct: true,
            auto_space_de: true,
            auto_space_dn: true,
            distribute: false,
            mark: crate::Marks::default(),
        }
    }
}

/// One page setup. A document has one per section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageLook {
    pub width: Twips,
    pub height: Twips,
    pub top: Twips,
    pub bottom: Twips,
    pub left: Twips,
    pub right: Twips,
    /// From the page's top edge to the header's top.
    pub header: Twips,
    /// From the page's bottom edge to the footer's bottom.
    pub footer: Twips,
    /// Extra left margin for binding.
    pub gutter: Twips,
    pub columns: u8,
    pub column_gap: Twips,
    /// The line grid's pitch, when lines snap to one.
    pub line_pitch: Option<Twips>,
}

impl Default for PageLook {
    /// A4 with Word's default margins for Chinese documents.
    fn default() -> Self {
        Self {
            width: 11906,
            height: 16838,
            top: 1440,
            bottom: 1440,
            left: 1800,
            right: 1800,
            header: 851,
            footer: 992,
            gutter: 0,
            columns: 1,
            column_gap: 425,
            line_pitch: None,
        }
    }
}

/// A section: its page setup and its headers and footers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Section {
    /// The id of the section's last top-level block; `None` for the last section.
    pub last_block: Option<crate::NodeId>,
    pub page: PageLook,
    pub header: Option<Vec<crate::Block>>,
    pub footer: Option<Vec<crate::Block>>,
    /// Different header and footer on the section's first page.
    pub first_header: Option<Vec<crate::Block>>,
    pub first_footer: Option<Vec<crate::Block>>,
    pub title_page: bool,
    /// Where page numbers start, when the section restarts them.
    pub page_start: Option<u32>,
    /// Starts on a new page; `false` continues on the same page.
    pub new_page: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VAlign {
    #[default]
    Top,
    Center,
    Bottom,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellLook {
    pub shading: Option<Rgb>,
    /// Overrides the table's borders for this cell.
    pub borders: Option<Borders>,
    pub v_align: VAlign,
    /// Continues the merged cell above (`vMerge`): drawn as one with it.
    pub merged_up: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableLook {
    /// Each grid column's width.
    pub columns: Vec<Twips>,
    /// Matches `Table::rows`: one per cell.
    pub cells: Vec<Vec<CellLook>>,
    /// Each row's height and whether it is exact; `None` grows with its text.
    pub rows: Vec<Option<(Twips, bool)>>,
    /// Rows repeated at the top of each page.
    pub header_rows: usize,
    /// Rows that may not split across pages (`cantSplit`).
    pub cant_split: Vec<bool>,
    pub borders: Borders,
    /// Between cells inside the table.
    pub inside_h: Option<Border>,
    pub inside_v: Option<Border>,
    /// Padding: top, right, bottom, left.
    pub padding: [Twips; 4],
    pub align: crate::Align,
    pub indent: Twips,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Wrap {
    /// Text flows above and below, not beside.
    #[default]
    TopAndBottom,
    /// Text flows around the picture's box.
    Square,
    /// No wrapping; drawn behind the text.
    Behind,
    /// No wrapping; drawn over the text.
    InFront,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Origin {
    #[default]
    Page,
    Margin,
    /// The paragraph that holds the anchor; for `y` only.
    Paragraph,
    /// The column; for `x` only.
    Column,
}

/// A picture placed on the page rather than in the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Float {
    pub x: Twips,
    pub from_x: Origin,
    pub y: Twips,
    pub from_y: Origin,
    pub wrap: Wrap,
    /// Space kept clear around it: top, right, bottom, left.
    pub clear: [Twips; 4],
}

/// How an atom takes up space.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AtomLook {
    /// A picture's width and height.
    pub size: Option<(Twips, Twips)>,
    pub float: Option<Float>,
    /// What the atom shows in the line: a field's last result, a footnote's
    /// number. An atom with neither `size` nor `shown` takes no space, as a
    /// bookmark. A `PAGE` or `NUMPAGES` field shows the number the layout gives it.
    pub shown: Option<String>,
}

/// How a slide shape is filled. Theme colours arrive resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Fill {
    #[default]
    None,
    /// A colour and its opacity, 0 to 255.
    Solid(Rgb, u8),
    Gradient {
        /// Positions from 0 to 1000, colour, opacity.
        stops: Vec<(u16, Rgb, u8)>,
        /// Degrees, clockwise from left to right.
        angle: i32,
        radial: bool,
    },
    Picture {
        path: std::path::PathBuf,
        /// Cropped off each side, in thousandths: top, right, bottom, left.
        crop: [i32; 4],
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dash {
    #[default]
    Solid,
    Dot,
    Dash,
    DashDot,
    LongDash,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arrow {
    #[default]
    None,
    Triangle,
    Stealth,
    Oval,
    Diamond,
    Open,
}

/// A shape's outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stroke {
    pub width: Twips,
    pub color: Rgb,
    pub alpha: u8,
    pub dash: Dash,
    /// At the path's start and end.
    pub head: Arrow,
    pub tail: Arrow,
}

/// One step of a shape's outline, in twips from the shape's top-left
/// corner, after flips; the frame's size is the shape's size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathCmd {
    Move(Twips, Twips),
    Line(Twips, Twips),
    Quad(Twips, Twips, Twips, Twips),
    Cubic(Twips, Twips, Twips, Twips, Twips, Twips),
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shadow {
    pub color: Rgb,
    pub alpha: u8,
    pub blur: Twips,
    pub distance: Twips,
    /// Degrees, clockwise from right.
    pub direction: i32,
}

/// What a slide shape is, besides its text.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ShapeKind {
    /// A text box or a drawn shape (its text in `Shape::paras`).
    #[default]
    Text,
    /// A picture: its file is in `ShapeLook::fill` as `Fill::Picture`.
    Picture,
    /// A table: its cells' paragraphs are editable like a Word table's.
    Table(Box<crate::Table>),
    Chart(Box<Chart>),
    /// A diagram or object shown by the picture the file keeps for it.
    Fallback(Option<std::path::PathBuf>),
}

/// A chart from its cached values: enough to draw it, not to edit its data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chart {
    pub kind: ChartKind,
    pub title: Option<String>,
    pub categories: Vec<String>,
    pub series: Vec<Series>,
    pub stacked: bool,
    /// Stacked to 100 %.
    pub percent: bool,
    pub legend: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ChartKind {
    #[default]
    Column,
    Bar,
    Line,
    Area,
    Pie,
    Doughnut,
    Scatter,
    /// Another kind, by its element name: drawn as a placeholder.
    Other(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Series {
    pub name: String,
    pub values: Vec<Option<f64>>,
    /// X values, for scatter charts.
    pub x: Vec<Option<f64>>,
    pub color: Option<Rgb>,
}

/// A slide shape's own look, apart from its text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapeLook {
    pub fill: Fill,
    pub line: Option<Stroke>,
    /// The outline; empty means the frame's rectangle.
    pub path: Vec<PathCmd>,
    pub flip: (bool, bool),
    pub shadow: Option<Shadow>,
    /// Text insets: top, right, bottom, left.
    pub insets: [Twips; 4],
    pub v_align: VAlign,
    /// Degrees, clockwise.
    pub rotation: i32,
    /// Shrinks text to fit, to this scale in thousandths (`normAutofit`).
    pub font_scale: Option<u16>,
}

// ---------------------------------------------------------------- sheets

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HAlign {
    /// Text left, numbers right, as Excel does.
    #[default]
    General,
    Left,
    Center,
    Right,
    /// Repeats the text to fill the cell.
    Fill,
    Justify,
    CenterAcross,
    Distributed,
}

/// How a cell looks. A workbook keeps each distinct style once; cells,
/// rows and columns refer to it by index, 0 being the default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellStyle {
    /// Font, size, colour, bold, italic, underline, strike.
    pub font: crate::Marks,
    pub fill: Option<Rgb>,
    pub borders: Borders,
    pub h_align: HAlign,
    pub v_align: VAlign,
    pub wrap: bool,
    pub shrink: bool,
    /// Indent steps of about three characters each.
    pub indent: u8,
    /// Degrees counter-clockwise, -90 to 90; 255 stacks letters vertically.
    pub rotation: i16,
    /// The number format code, as `0.00%` or `yyyy"年"m"月"d"日"`.
    pub number_format: String,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            font: crate::Marks::default(),
            fill: None,
            borders: Borders::default(),
            h_align: HAlign::General,
            v_align: VAlign::Bottom,
            wrap: false,
            shrink: false,
            indent: 0,
            rotation: 0,
            number_format: "General".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Compare {
    Between(f64, f64),
    NotBetween(f64, f64),
    Equal(f64),
    NotEqual(f64),
    Greater(f64),
    Less(f64),
    AtLeast(f64),
    AtMost(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub enum CondKind {
    /// Compares the cell's value; `style` applies when it holds.
    CellIs(Compare),
    /// Text containing this.
    Contains(String),
    /// The top or bottom `n` values, or `n` percent.
    Top {
        n: u32,
        bottom: bool,
        percent: bool,
    },
    AboveAverage {
        below: bool,
    },
    Duplicate {
        unique: bool,
    },
    /// Colours from the lowest to the highest value: two or three stops.
    ColorScale(Vec<Rgb>),
    /// A bar across the cell, as long as the value is large.
    DataBar(Rgb),
    /// A rule this model does not evaluate, by its type: it never applies.
    Other(String),
}

/// A conditional format over a range.
#[derive(Clone, Debug, PartialEq)]
pub struct CondRule {
    /// First row, first column, last row, last column.
    pub range: (u32, u32, u32, u32),
    pub kind: CondKind,
    /// What changes when the rule holds (`dxf`): only the set fields count.
    pub style: Option<CellStyle>,
    /// Lower runs first; `stop_if_true` stops later rules.
    pub priority: u32,
    pub stop_if_true: bool,
}

/// Something drawn over a sheet, anchored to cells.
#[derive(Clone, Debug, PartialEq)]
pub struct Anchored {
    /// Row, column, and offsets in points into that cell.
    pub from: (u32, u32, f32, f32),
    pub to: (u32, u32, f32, f32),
    pub what: Drawing,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Drawing {
    Picture(std::path::PathBuf),
    Chart(Chart),
    /// A text box or drawn shape, laid out like a slide shape.
    Shape(Box<crate::Shape>),
}

/// How a sheet is laid out. Sizes are in points.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetLook {
    /// Column widths where the file sets them.
    pub columns: std::collections::BTreeMap<u32, f32>,
    pub default_width: f32,
    pub rows: std::collections::BTreeMap<u32, f32>,
    pub default_height: f32,
    pub hidden_rows: std::collections::BTreeSet<u32>,
    pub hidden_columns: std::collections::BTreeSet<u32>,
    /// Style indexes for whole rows and columns.
    pub row_styles: std::collections::BTreeMap<u32, u32>,
    pub column_styles: std::collections::BTreeMap<u32, u32>,
    /// Rows and columns frozen at the top and left.
    pub frozen: (u32, u32),
    pub gridlines: bool,
    /// Percent.
    pub zoom: u16,
    pub conditional: Vec<CondRule>,
    pub drawings: Vec<Anchored>,
    pub tab_color: Option<Rgb>,
}

impl Default for SheetLook {
    /// Excel's defaults: 8.43 characters of Calibri 11 wide, 15 points high.
    fn default() -> Self {
        Self {
            columns: Default::default(),
            default_width: 48.0,
            rows: Default::default(),
            default_height: 15.0,
            hidden_rows: Default::default(),
            hidden_columns: Default::default(),
            row_styles: Default::default(),
            column_styles: Default::default(),
            frozen: (0, 0),
            gridlines: true,
            zoom: 100,
            conditional: Vec::new(),
            drawings: Vec::new(),
            tab_color: None,
        }
    }
}
