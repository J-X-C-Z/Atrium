//! File previews for the side panel: Word, PowerPoint and PDF as page
//! images, spreadsheets and CSV as cell grids.
//!
//! The API is fixed here and used by the UI. To build
//! (`docs/file-preview-implementation.md`): the bodies of
//! [`Previewer::preview`] and [`Previewer::find`], with office2pdf, hayro
//! and calamine.

mod cache;
mod convert;
mod grid;
mod pdf;

#[doc(hidden)]
pub use cache::probe;

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::Duration;

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
    pub fn preview(&self, path: &Path, ask: &Ask) -> Result<Preview, Refused> {
        let meta =
            std::fs::metadata(path).map_err(|_| Refused::Failed("the file is gone".into()))?;
        if meta.len() > cache::INPUT_LIMIT {
            return Err(Refused::TooBig);
        }
        let kind = kind(path);
        match kind {
            Kind::Other => Err(Refused::Unsupported),
            Kind::Csv => {
                let path = path.to_path_buf();
                let ask = ask.clone();
                with_deadline(move |_| grid::csv(&path, &ask))
            }
            Kind::Sheet if ask.layout == Layout::Grid => {
                let path = path.to_path_buf();
                let ask = ask.clone();
                with_deadline(move |_| grid::sheet(&path, &ask))
            }
            _ => self.pages(path, kind, ask),
        }
    }

    fn pages(&self, path: &Path, kind: Kind, ask: &Ask) -> Result<Preview, Refused> {
        let key = cache::Cache::key(path)?;
        let path = path.to_path_buf();
        let cache_dir = self.cache_dir.clone();
        let fonts = self.fonts.clone();
        let ask = ask.clone();
        with_deadline(move |cancel| {
            let (bytes, warnings) = if kind == Kind::Pdf {
                let bytes =
                    std::fs::read(&path).map_err(|_| Refused::Failed("the file is gone".into()))?;
                (bytes, Vec::new())
            } else {
                convert::check_limits(&path, kind)?;
                ensure_pdf(&cache_dir, &path, &key, &fonts, &cancel)?
            };
            let pdf = pdf::open(&bytes)?;
            let total = pdf::count(&pdf);
            let start = ask.pages.start.min(total);
            let end = ask.pages.end.min(total).min(start.saturating_add(20));
            let cache = cache::Cache::new(cache_dir);
            let pages = pdf::render_pages(&pdf, &cache, &key, ask.width_px, start..end, &cancel)?;
            Ok(Preview::Pages {
                pages,
                total,
                warnings,
            })
        })
    }

    /// Where `needle` appears, for highlights.
    pub fn find(&self, path: &Path, needle: &str) -> Result<Vec<Hit>, Refused> {
        let meta =
            std::fs::metadata(path).map_err(|_| Refused::Failed("the file is gone".into()))?;
        if meta.len() > cache::INPUT_LIMIT {
            return Err(Refused::TooBig);
        }
        let kind = kind(path);
        if matches!(kind, Kind::Other | Kind::Csv | Kind::Sheet) {
            return Err(Refused::Unsupported);
        }
        let key = cache::Cache::key(path)?;
        let path = path.to_path_buf();
        let cache_dir = self.cache_dir.clone();
        let fonts = self.fonts.clone();
        let needle = needle.to_string();
        with_deadline(move |cancel| {
            let bytes = if kind == Kind::Pdf {
                std::fs::read(&path).map_err(|_| Refused::Failed("the file is gone".into()))?
            } else {
                convert::check_limits(&path, kind)?;
                ensure_pdf(&cache_dir, &path, &key, &fonts, &cancel)?.0
            };
            let pdf = pdf::open(&bytes)?;
            pdf::find(&pdf, &needle, &fonts)
        })
    }
}

/// Run dependency work off the caller's thread, with a 30 s deadline. The
/// thread cannot be killed, so a late result is discarded through `cancel`.
fn with_deadline<T, F>(job: F) -> Result<T, Refused>
where
    T: Send + 'static,
    F: FnOnce(Arc<AtomicBool>) -> Result<T, Refused> + Send + 'static,
{
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let flag = cancel.clone();
    std::thread::Builder::new()
        .name("atrium-preview".into())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(flag)));
            let _ = sender.send(match result {
                Ok(result) => result,
                Err(_) => Err(Refused::Failed("the preview stopped unexpectedly".into())),
            });
        })
        .map_err(|err| Refused::Failed(format!("the preview could not start: {err}")))?;
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            cancel.store(true, Ordering::SeqCst);
            Err(Refused::Timeout)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(Refused::Failed("the preview stopped unexpectedly".into()))
        }
    }
}

/// Convert once per file version, then share the cached PDF.
fn ensure_pdf(
    cache_dir: &Path,
    path: &Path,
    key: &str,
    fonts: &Fonts,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, Vec<String>), Refused> {
    let cache = cache::Cache::new(cache_dir.to_path_buf());
    if let Some(bytes) = cache.pdf(key) {
        return Ok((bytes, cache.warnings(key)));
    }
    let lock = key_lock(key);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(bytes) = cache.pdf(key) {
        return Ok((bytes, cache.warnings(key)));
    }
    convert::check_limits(path, kind(path))?;
    let (bytes, warnings) = convert::to_pdf(path, fonts, None)?;
    if cancel.load(Ordering::SeqCst) {
        return Err(Refused::Timeout);
    }
    cache
        .put_pdf(key, &bytes)
        .map_err(|err| Refused::Failed(format!("the preview could not be saved: {err}")))?;
    cache.put_warnings(key, &warnings);
    Ok((bytes, warnings))
}

/// One conversion per file version at a time.
fn key_lock(key: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key.to_string(), Arc::downgrade(&lock));
    lock
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
