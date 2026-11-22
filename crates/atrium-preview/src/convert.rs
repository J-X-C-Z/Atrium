//! Office documents to PDF, through `office2pdf`.

use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use office2pdf::config::ConvertOptions;
use office2pdf::error::ConvertError;
use zip::ZipArchive;

use crate::cache::{UNPACKED_LIMIT, probe};
use crate::{Kind, Refused};

/// Reject files that are too large before doing any work, and packages that
/// unpack to far more than they claim.
pub(crate) fn check_limits(path: &Path, kind: Kind) -> Result<(), Refused> {
    let meta = std::fs::metadata(path).map_err(|_| Refused::Failed("the file is gone".into()))?;
    if meta.len() > crate::cache::INPUT_LIMIT {
        return Err(Refused::TooBig);
    }
    if matches!(kind, Kind::Word | Kind::Slides | Kind::Sheet) {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        // OLE workbooks (.xls) are not zips.
        if !matches!(ext.as_str(), "xls" | "xlsb") {
            check_zip(path)?;
        }
    }
    Ok(())
}

fn check_zip(path: &Path) -> Result<(), Refused> {
    let file = File::open(path).map_err(|_| Refused::Failed("the file is gone".into()))?;
    let mut archive = ZipArchive::new(file).map_err(|_| Refused::Damaged)?;
    let mut total = 0u64;
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index).map_err(|_| Refused::Damaged)?;
        if entry.encrypted() {
            return Err(Refused::Password);
        }
        total = total.saturating_add(entry.size());
        if total > UNPACKED_LIMIT {
            return Err(Refused::TooBig);
        }
    }
    Ok(())
}

/// Convert a Word, PowerPoint or Excel file to PDF. `sheet` selects one sheet
/// of a workbook when the caller wants a printed sheet.
pub(crate) fn to_pdf(
    path: &Path,
    fonts: &crate::Fonts,
    sheet: Option<&str>,
) -> Result<(Vec<u8>, Vec<String>), Refused> {
    let options = options(fonts, sheet);
    probe::converted(path);
    let result = catch_unwind(AssertUnwindSafe(|| {
        office2pdf::convert_with_options(path, &options)
    }));
    match result {
        Ok(Ok(result)) => Ok((
            result.pdf,
            result
                .warnings
                .into_iter()
                .map(|warning| warning.to_string())
                .collect(),
        )),
        Ok(Err(error)) => Err(map_error(error)),
        Err(panic) => Err(Refused::Failed(format!(
            "the converter stopped on this file ({}), so it may be damaged",
            panic_message(&panic)
        ))),
    }
}

fn options(fonts: &crate::Fonts, sheet: Option<&str>) -> ConvertOptions {
    ConvertOptions {
        font_paths: fonts
            .paths
            .iter()
            .filter(|path| path.is_dir())
            .cloned()
            .collect(),
        last_resort_font_family: Some(
            crate::pdf::cjk_family()
                .unwrap_or("Noto Sans CJK SC")
                .to_string(),
        ),
        sheet_names: sheet.map(|name| vec![name.to_string()]),
        ..Default::default()
    }
}

pub(crate) fn map_error(error: ConvertError) -> Refused {
    match error {
        ConvertError::UnsupportedEncryption => Refused::Password,
        ConvertError::UnsupportedFormat(_) => Refused::Unsupported,
        ConvertError::Io(err) => Refused::Failed(format!("the file could not be read: {err}")),
        ConvertError::Parse(why) => {
            if looks_encrypted(&why) {
                Refused::Password
            } else {
                Refused::Failed(format!("the file could not be read: {why}"))
            }
        }
        ConvertError::Render(why) => Refused::Failed(format!("the page could not be drawn: {why}")),
    }
}

fn looks_encrypted(why: &str) -> bool {
    let why = why.to_ascii_lowercase();
    why.contains("encrypt") || why.contains("password")
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "unknown panic".into()
    }
}
