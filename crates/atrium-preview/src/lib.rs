//! File previews for the side panel: Word, PowerPoint and PDF as page
//! images, spreadsheets and CSV as cell grids.
//!
//! The API is fixed here and used by the UI. To build
//! (`docs/file-preview-implementation.md`): the bodies of
//! [`Previewer::preview`] and [`Previewer::find`], with office2pdf, hayro
//! and calamine.

use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Word,
    Slides,
    Sheet,
    Csv,
    Pdf,
    Other,
}

/// By extension, then checked against the content: OOXML and ODF are zips,
/// PDF starts with `%PDF-`. A mismatch is `Other`.
pub fn kind(path: &Path) -> Kind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let claimed = match ext.as_str() {
        "docx" => Kind::Word,
        "pptx" => Kind::Slides,
        "xlsx" | "xls" | "xlsb" | "ods" => Kind::Sheet,
        "csv" | "tsv" => Kind::Csv,
        "pdf" => Kind::Pdf,
        _ => return Kind::Other,
    };
    let mut head = [0u8; 5];
    let read = std::fs::File::open(path)
        .and_then(|mut f| std::io::Read::read(&mut f, &mut head))
        .unwrap_or(0);
    let head = &head[..read];
    let fits = match claimed {
        Kind::Pdf => head.starts_with(b"%PDF-"),
        Kind::Csv => !head.starts_with(b"PK\x03\x04") && !head.starts_with(b"%PDF-"),
        // xls and some xlsb are OLE files.
        Kind::Sheet if ext == "xls" => head.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]),
        _ => head.starts_with(b"PK\x03\x04"),
    };
    if fits { claimed } else { Kind::Other }
}

/// Font folders and files used to draw documents.
#[derive(Clone, Debug, Default)]
pub struct Fonts {
    pub paths: Vec<PathBuf>,
}

impl Fonts {
    /// The system's font folders.
    pub fn system() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let paths = if cfg!(target_os = "macos") {
            vec![
                "/System/Library/Fonts".into(),
                "/Library/Fonts".into(),
                home.join("Library/Fonts"),
            ]
        } else if cfg!(windows) {
            let win = std::env::var_os("WINDIR")
                .map(PathBuf::from)
                .unwrap_or("C:\\Windows".into());
            let local = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_default();
            vec![win.join("Fonts"), local.join("Microsoft\\Windows\\Fonts")]
        } else {
            vec!["/usr/share/fonts".into(), home.join(".local/share/fonts")]
        };
        Self { paths }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    /// A sheet as cells.
    #[default]
    Grid,
    /// A sheet as printed pages.
    Pages,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// The panel's width times its scale factor; pages are drawn this wide.
    pub width_px: u32,
    /// The pages wanted now; the UI asks for more as it scrolls.
    pub pages: Range<usize>,
    /// For sheets: which one, as a grid.
    pub sheet: Option<usize>,
    pub layout: Layout,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub index: usize,
    pub png: PathBuf,
    pub width_pt: f32,
    pub height_pt: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CellAlign {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub bold: bool,
    pub align: CellAlign,
    /// Rows and columns it spans; (1, 1) for an ordinary cell.
    pub span: (u16, u16),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Preview {
    Pages {
        pages: Vec<Page>,
        total: usize,
        /// Parts that may look different from the original app.
        warnings: Vec<String>,
    },
    Table {
        sheets: Vec<String>,
        current: usize,
        rows: Vec<Vec<Cell>>,
        more_rows: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub page: usize,
    /// x, y, width, height in the page's points.
    pub rect_pt: [f32; 4],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    Unsupported,
    Password,
    Damaged,
    TooBig,
    Timeout,
    Failed(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Unsupported => f.write_str("no preview for this kind of file yet"),
            Refused::Password => f.write_str("the file is password protected"),
            Refused::Damaged => f.write_str("the file looks damaged"),
            Refused::TooBig => f.write_str("the file is too big to preview"),
            Refused::Timeout => f.write_str("the preview took too long"),
            Refused::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for Refused {}

pub struct Previewer {
    cache_dir: PathBuf,
    fonts: Fonts,
}

impl Previewer {
    pub fn new(cache_dir: PathBuf, fonts: Fonts) -> Self {
        Self { cache_dir, fonts }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    /// Blocking: the UI runs it on the background executor.
    pub fn preview(&self, path: &Path, _ask: &Ask) -> Result<Preview, Refused> {
        if std::fs::metadata(path).is_err() {
            return Err(Refused::Failed("the file is gone".into()));
        }
        Err(Refused::Unsupported)
    }

    /// Where `needle` appears, for highlights.
    pub fn find(&self, _path: &Path, _needle: &str) -> Result<Vec<Hit>, Refused> {
        Err(Refused::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_check_the_content() {
        let dir = std::env::temp_dir().join(format!("atrium-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("report.xlsx");
        std::fs::write(&fake, b"%PDF-1.7 not a workbook").unwrap();
        assert_eq!(kind(&fake), Kind::Other);
        let pdf = dir.join("report.pdf");
        std::fs::write(&pdf, b"%PDF-1.7").unwrap();
        assert_eq!(kind(&pdf), Kind::Pdf);
        let csv = dir.join("data.csv");
        std::fs::write(&csv, "a,b\n1,2\n").unwrap();
        assert_eq!(kind(&csv), Kind::Csv);
        assert_eq!(kind(&dir.join("notes.txt")), Kind::Other);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
