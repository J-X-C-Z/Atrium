//! Atrium's document core: Word, Excel and PowerPoint files as a simple
//! model you can edit in the side panel, saved without losing anything the
//! model does not cover.
//!
//! Done here: the model, the edits with their exact inverses, and the
//! change reports handed to the agent. To build
//! (`docs/office-editing-implementation.md`): the file binding behind
//! [`open`] and [`Office::save`], which reads the package into this model and
//! writes back only the parts the edits touched.

mod report;
mod sheet;
mod slides;
mod word;

use std::path::{Path, PathBuf};

pub use report::{Change, Key, Lang, Location, Piece, Snapshot, changes, describe};
pub use sheet::{Cell, Sheet, SheetOp, Value, Workbook, a1, parse_input};
pub use slides::{Deck, Shape, Slide, SlideOp};
pub use word::{
    Align, Atom, AtomKind, Block, EditError, Inline, Marks, NodeId, Para, ParaStyle, Place, Table,
    WordDoc, WordOp, inline_text,
};

/// Which model a file opens as, by its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Word,
    Sheet,
    Slides,
}

impl Format {
    pub fn of(path: &Path) -> Option<Format> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "docx" => Some(Format::Word),
            "xlsx" => Some(Format::Sheet),
            "pptx" => Some(Format::Slides),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// Password protected.
    Encrypted,
    /// `.doc`, `.xls` or `.ppt`.
    Legacy,
    Damaged(String),
    TooBig,
    /// No editor for this kind of file yet.
    Unsupported,
    Io(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Encrypted => f.write_str("the file is password protected"),
            OpenError::Legacy => f.write_str("the file is in an old Office format"),
            OpenError::Damaged(why) => write!(f, "the file looks damaged: {why}"),
            OpenError::TooBig => f.write_str("the file is too big to open here"),
            OpenError::Unsupported => f.write_str("this kind of file cannot be edited here yet"),
            OpenError::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for OpenError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The file changed on disk since it was opened or last saved.
    ChangedOnDisk,
    /// Saving this kind of file is not built yet.
    Unsupported,
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::ChangedOnDisk => f.write_str("the file changed on disk"),
            SaveError::Unsupported => f.write_str("saving this kind of file is not available yet"),
            SaveError::Io(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SaveError {}

/// One edit to whichever model is open.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Word(WordOp),
    Sheet(SheetOp),
    Slides(SlideOp),
}

/// An open document.
#[derive(Clone, Debug)]
pub enum Office {
    Word(WordDoc),
    Sheet(Workbook),
    Slides(Deck),
}

impl Office {
    /// Applies `op` and returns its inverse. A failed op changes nothing.
    pub fn apply(&mut self, op: Op) -> Result<Op, EditError> {
        match (self, op) {
            (Office::Word(d), Op::Word(op)) => d.apply(op).map(Op::Word),
            (Office::Sheet(w), Op::Sheet(op)) => w.apply(op).map(Op::Sheet),
            (Office::Slides(d), Op::Slides(op)) => d.apply(op).map(Op::Slides),
            _ => Err(EditError::Invalid(
                "an edit for another kind of document".into(),
            )),
        }
    }

    /// The text of every paragraph and cell, to compare with later.
    pub fn snapshot(&self) -> Snapshot {
        report::snapshot(self)
    }
}

/// An open file: its model, and what has been done to it since it was
/// opened or last saved.
#[derive(Debug)]
pub struct Document {
    pub path: PathBuf,
    pub office: Office,
    /// Edits since the last save, in order, for the file binding to write.
    pending: Vec<Op>,
    /// The file's SHA-256 when opened or last saved.
    pub hash: String,
}

impl Document {
    /// A document over `office` that `path` came from. Used by the file
    /// binding, and by tests.
    pub fn new(path: PathBuf, office: Office, hash: String) -> Self {
        Self {
            path,
            office,
            pending: Vec::new(),
            hash,
        }
    }

    pub fn apply(&mut self, op: Op) -> Result<Op, EditError> {
        let inverse = self.office.apply(op.clone())?;
        self.pending.push(op);
        Ok(inverse)
    }

    pub fn is_dirty(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Writes the edits into the file: only the paragraphs and cells they
    /// touched change. Fails with `ChangedOnDisk` when the file is no
    /// longer the one that was opened.
    pub fn save(&mut self) -> Result<(), SaveError> {
        let to = self.path.clone();
        self.save_as(&to)
    }

    /// Saves even though the file changed on disk, replacing those changes.
    pub fn save_over(&mut self) -> Result<(), SaveError> {
        // The file binding re-reads the file's hash, then saves as usual.
        let to = self.path.clone();
        self.save_as(&to)
    }

    pub fn save_as(&mut self, _to: &Path) -> Result<(), SaveError> {
        // The file binding replays `pending` onto the package here.
        Err(SaveError::Unsupported)
    }
}

/// Opens `path` for editing.
pub fn open(path: &Path) -> Result<Document, OpenError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "doc" | "xls" | "ppt") {
        return Err(OpenError::Legacy);
    }
    if Format::of(path).is_none() {
        return Err(OpenError::Unsupported);
    }
    std::fs::metadata(path).map_err(|e| OpenError::Io(e.to_string()))?;
    // The file binding reads the package into a model here.
    Err(OpenError::Unsupported)
}
