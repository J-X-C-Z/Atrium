//! Bridge the existing public Document model and the lossless OOXML package.
use crate::{
    Document, Format, Office, Op, OpenError, Package, SaveError, package::PackageSaveError,
    sheet_binding::SheetBinding, slides_binding::SlidesBinding, word_binding::WordBinding,
};
use std::{fmt, path::Path};

pub(crate) struct FileBinding {
    package: Package,
    model: FileModel,
}
enum FileModel {
    Word(Box<WordBinding>),
    Sheet(Box<SheetBinding>),
    Slides(Box<SlidesBinding>),
}
impl fmt::Debug for FileBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileBinding")
            .field("source", &self.package.source())
            .finish_non_exhaustive()
    }
}

fn digest_hex(digest: [u8; 32]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for b in digest {
        result.push(HEX[(b >> 4) as usize] as char);
        result.push(HEX[(b & 15) as usize] as char);
    }
    result
}

pub(crate) fn open(path: &Path) -> Result<Document, OpenError> {
    let format = Format::of(path).ok_or(OpenError::Unsupported)?;
    let package = Package::open(path)?;
    package.validate_main_type(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
    )?;
    let (office, model) = match format {
        Format::Word => {
            let (state, model) = WordBinding::load(&package)?;
            (Office::Word(model), FileModel::Word(Box::new(state)))
        }
        Format::Sheet => {
            let (model, state) = SheetBinding::load(&package)?;
            (Office::Sheet(model), FileModel::Sheet(Box::new(state)))
        }
        Format::Slides => {
            let (model, state) = SlidesBinding::load(&package)?;
            (Office::Slides(model), FileModel::Slides(Box::new(state)))
        }
    };
    let mut doc = Document::new(path.into(), office, digest_hex(package.original_hash()));
    doc.binding = Some(Box::new(FileBinding { package, model }));
    Ok(doc)
}
impl From<PackageSaveError> for SaveError {
    fn from(error: PackageSaveError) -> Self {
        match error {
            PackageSaveError::ChangedOnDisk => Self::ChangedOnDisk,
            other => Self::Io(other.to_string()),
        }
    }
}
pub(crate) fn save(doc: &mut Document, to: &Path, over: bool) -> Result<(), SaveError> {
    let state = doc.binding.as_ref().ok_or(SaveError::Unsupported)?;
    let original = &state.package;
    if Format::of(original.source()) != Format::of(to) {
        return Err(SaveError::Unsupported);
    }
    let same = to == original.source();
    if over && !same {
        return Err(SaveError::Unsupported);
    }
    let expected = if same {
        let current = original.live_hash()?;
        if !over && current != original.original_hash() {
            return Err(SaveError::ChangedOnDisk);
        }
        Some(current)
    } else {
        None
    };
    let (parts, word) = match (&state.model, &doc.office) {
        (FileModel::Word(binding), Office::Word(_)) => {
            let mut next = binding.as_ref().clone();
            for op in &doc.pending {
                let Op::Word(edit) = op else {
                    return Err(SaveError::Unsupported);
                };
                next.apply(edit).map_err(|e| SaveError::Io(e.to_string()))?;
            }
            (next.changed_parts(), Some(next))
        }
        (FileModel::Sheet(binding), Office::Sheet(model)) => {
            (binding.changes(original, model)?, None)
        }
        (FileModel::Slides(binding), Office::Slides(model)) => {
            (binding.changes(original, model)?, None)
        }
        _ => return Err(SaveError::Unsupported),
    };
    let prepared = original.prepare(to, &parts)?;
    // Resolve all XML paths and model references before replacing a user's file.
    let next = match (&doc.office, word) {
        (Office::Word(_), Some(mut next)) => {
            next.mark_saved();
            FileModel::Word(Box::new(next))
        }
        (Office::Sheet(model), None) => FileModel::Sheet(Box::new(
            SheetBinding::rebase(&prepared, model).map_err(|e| SaveError::Io(e.to_string()))?,
        )),
        (Office::Slides(model), None) => FileModel::Slides(Box::new(
            SlidesBinding::rebase(&prepared, model).map_err(|e| SaveError::Io(e.to_string()))?,
        )),
        _ => return Err(SaveError::Unsupported),
    };
    original.persist(&prepared, expected)?;
    doc.path = to.into();
    doc.hash = digest_hex(prepared.original_hash());
    doc.pending.clear();
    doc.binding = Some(Box::new(FileBinding {
        package: prepared,
        model: next,
    }));
    Ok(())
}
