use crate::{
    OpenError,
    xml::{MAX_PART_BYTES, XmlPart},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub file_bytes: u64,
    pub inflated_bytes: u64,
    pub entries: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            file_bytes: 100_000_000,
            inflated_bytes: 500_000_000,
            entries: 100_000,
        }
    }
}
/// Retains the entire original archive, including metadata, for exact no-op saves.
/// Edited saves preserve untouched local ZIP records.
#[derive(Clone, Debug)]
pub struct Package {
    bytes: Arc<[u8]>,
    zip: zip::ZipArchive<Cursor<Arc<[u8]>>>,
    path: PathBuf,
    hash: [u8; 32],
    permissions: fs::Permissions,
    names: HashSet<String>,
    edits: BTreeMap<String, Vec<u8>>,
    limits: Limits,
}
#[derive(Debug, thiserror::Error)]
pub enum PackageSaveError {
    #[error("source changed on disk")]
    ChangedOnDisk,
    #[error("save-as destination exists")]
    DestinationExists,
    #[error("invalid replacement: {0}")]
    InvalidPart(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
fn damaged(e: impl std::fmt::Display) -> OpenError {
    OpenError::Damaged(e.to_string())
}
fn open_regular(path: &Path) -> std::io::Result<File> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(std::io::Error::other(
            "source is not a regular file (symlinks are not opened)",
        ));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("source changed type"));
    }
    Ok(file)
}
fn bounded(path: &Path, limit: u64) -> Result<(Vec<u8>, fs::Permissions), OpenError> {
    let file = open_regular(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(damaged("not a regular file"));
    }
    if metadata.len() > limit {
        return Err(OpenError::TooBig);
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(OpenError::TooBig);
    }
    Ok((bytes, metadata.permissions()))
}
impl Package {
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        Self::open_with_limits(path, Limits::default())
    }
    pub fn open_with_limits(path: &Path, limits: Limits) -> Result<Self, OpenError> {
        let (bytes, permissions) = bounded(path, limits.file_bytes)?;
        Self::from_bytes(path, bytes.into(), permissions, limits)
    }
    fn from_bytes(
        path: &Path,
        bytes: Arc<[u8]>,
        permissions: fs::Permissions,
        limits: Limits,
    ) -> Result<Self, OpenError> {
        if bytes.starts_with(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
            let compound = cfb::CompoundFile::open(Cursor::new(&bytes)).map_err(damaged)?;
            return Err(
                if compound.is_stream("/EncryptionInfo") && compound.is_stream("/EncryptedPackage")
                {
                    OpenError::Encrypted
                } else {
                    OpenError::Legacy
                },
            );
        }
        if path.extension().and_then(|s| s.to_str()).is_some_and(|s| {
            ["doc", "xls", "ppt"]
                .iter()
                .any(|e| s.eq_ignore_ascii_case(e))
        }) {
            return Err(OpenError::Legacy);
        }
        preflight_directory(&bytes, limits)?;
        let mut zip = zip::ZipArchive::new(Cursor::new(&bytes)).map_err(damaged)?;
        if zip.len() > limits.entries {
            return Err(OpenError::TooBig);
        }
        let mut names = HashSet::new();
        let mut total = 0u64;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(damaged)?;
            let name = entry.name();
            if name.is_empty()
                || name.starts_with('/')
                || name.contains('\\')
                || name.split('/').any(|p| p == ".." || p == ".")
                || !names.insert(name.to_owned())
            {
                return Err(damaged("ambiguous or unsafe entry name"));
            }
            total = total.checked_add(entry.size()).ok_or(OpenError::TooBig)?;
            if total > limits.inflated_bytes {
                return Err(OpenError::TooBig);
            }
            // Verify CRC and actual inflation, not only central-directory claims; never extract.
            let advertised = entry.size();
            let actual = std::io::copy(
                &mut (&mut entry).take(advertised.saturating_add(1)),
                &mut std::io::sink(),
            )
            .map_err(damaged)?;
            if actual != advertised {
                return Err(damaged("entry size mismatch"));
            }
        }
        if !names.contains("[Content_Types].xml") {
            return Err(damaged("missing content types"));
        }
        let hash = Sha256::digest(&bytes).into();
        let zip = zip::ZipArchive::new(Cursor::new(Arc::clone(&bytes))).map_err(damaged)?;
        Ok(Self {
            bytes,
            zip,
            path: path.to_path_buf(),
            hash,
            permissions,
            names,
            edits: BTreeMap::new(),
            limits,
        })
    }
    pub fn has_part(&self, name: &str) -> bool {
        self.names.contains(name)
    }
    pub fn read_part(&self, name: &str) -> Result<Vec<u8>, OpenError> {
        self.read_part_with_limit(name, self.limits.inflated_bytes)
    }
    pub(crate) fn read_part_with_limit(
        &self,
        name: &str,
        limit: u64,
    ) -> Result<Vec<u8>, OpenError> {
        if let Some(bytes) = self.edits.get(name) {
            if bytes.len() as u64 > limit {
                return Err(OpenError::TooBig);
            }
            return Ok(bytes.clone());
        }
        self.read_original_part_with_limit(name, limit)
    }
    fn read_original_part(&self, name: &str) -> Result<Vec<u8>, OpenError> {
        self.read_original_part_with_limit(name, self.limits.inflated_bytes)
    }
    fn read_original_part_with_limit(&self, name: &str, limit: u64) -> Result<Vec<u8>, OpenError> {
        let mut zip = self.zip.clone();
        let entry = zip.by_name(name).map_err(damaged)?;
        if entry.size() > limit {
            return Err(OpenError::TooBig);
        }
        let mut data = Vec::new();
        entry
            .take(limit.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(damaged)?;
        if data.len() as u64 > limit {
            return Err(OpenError::TooBig);
        }
        Ok(data)
    }
    pub fn part(&self, name: &str) -> Result<XmlPart, OpenError> {
        XmlPart::parse(self.read_part_with_limit(name, MAX_PART_BYTES as u64)?)
    }
    pub fn original_hash(&self) -> [u8; 32] {
        self.hash
    }
    /// Validate the batch before modifying the in-memory projection. New entries
    /// are not supported; replace existing XML or relationship parts.
    pub fn replace_parts(&mut self, changes: &[(String, Vec<u8>)]) -> Result<(), PackageSaveError> {
        self.validate_changes(changes, false)?;
        let mut updates = Vec::with_capacity(changes.len());
        for (name, data) in changes {
            let original = self
                .read_original_part(name)
                .map_err(|e| PackageSaveError::InvalidPart(e.to_string()))?;
            updates.push((name, (original != *data).then(|| data.clone())));
        }
        for (name, update) in updates {
            if let Some(bytes) = update {
                self.edits.insert(name.clone(), bytes);
            } else {
                self.edits.remove(name);
            }
        }
        Ok(())
    }
    fn has_package_signatures(&self) -> Result<bool, PackageSaveError> {
        if self
            .names
            .iter()
            .any(|n| n.to_ascii_lowercase().starts_with("_xmlsignatures/"))
        {
            return Ok(true);
        }
        if !self.has_part("_rels/.rels") {
            return Ok(false);
        }
        let root = self
            .part("_rels/.rels")
            .map_err(|e| PackageSaveError::InvalidPart(e.to_string()))?;
        Ok(root.root().children.iter().any(|n| {
            n.local_name == "Relationship"
                && n.attr(None, "Type")
                    .is_some_and(|t| t.ends_with("/digital-signature/origin"))
        }))
    }
    fn validate_changes(
        &self,
        changes: &[(String, Vec<u8>)],
        allow_new: bool,
    ) -> Result<(), PackageSaveError> {
        if !changes.is_empty() && self.has_package_signatures()? {
            return Err(PackageSaveError::InvalidPart(
                "signed packages cannot be edited".into(),
            ));
        }
        let mut names = HashSet::new();
        for (name, data) in changes {
            if !(self.has_part(name)
                || allow_new
                    && (self.can_add_relationships_part(name)?
                        || self.can_add_numbering_part(name, changes)?))
                || !(name.ends_with(".xml") || name.ends_with(".rels"))
                || !names.insert(name.as_str())
            {
                return Err(PackageSaveError::InvalidPart(
                    "part is missing, duplicate or not XML".into(),
                ));
            }
            if data.len() > MAX_PART_BYTES {
                return Err(PackageSaveError::InvalidPart(
                    "replacement XML exceeds part limit".into(),
                ));
            }
            XmlPart::parse(data.clone())
                .map_err(|e| PackageSaveError::InvalidPart(e.to_string()))?;
        }
        Ok(())
    }
    fn can_add_relationships_part(&self, name: &str) -> Result<bool, PackageSaveError> {
        let Some((dir, relative)) = name.rsplit_once("_rels/") else {
            return Ok(false);
        };
        let Some(basename) = relative.strip_suffix(".rels") else {
            return Ok(false);
        };
        if (!dir.is_empty() && !dir.ends_with('/'))
            || basename.is_empty()
            || basename.contains(['/', '\\'])
            || (name.len() > 4096)
            || self.names.iter().any(|n| n.eq_ignore_ascii_case(name))
            || !self.has_part(&format!("{dir}{basename}"))
        {
            return Ok(false);
        }
        let declared = self
            .part("[Content_Types].xml")
            .map_err(|e| PackageSaveError::InvalidPart(e.to_string()))?;
        Ok(declared.root().children.iter().any(|n| {
            n.local_name == "Default"
                && n.attr(None, "Extension") == Some("rels")
                && n.attr(None, "ContentType")
                    == Some("application/vnd.openxmlformats-package.relationships+xml")
        }))
    }
    fn can_add_numbering_part(
        &self,
        name: &str,
        changes: &[(String, Vec<u8>)],
    ) -> Result<bool, PackageSaveError> {
        if name.rsplit('/').next() != Some("numbering.xml")
            || name.contains(['\\', ':', '?', '#', '\0'])
            || name.len() > 4096
            || name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || self
                .names
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            return Ok(false);
        }
        let Some((_, manifest)) = changes.iter().find(|(p, _)| p == "[Content_Types].xml") else {
            return Ok(false);
        };
        if manifest.len() > MAX_PART_BYTES {
            return Ok(false);
        }
        let types = XmlPart::parse(manifest.clone())
            .map_err(|e| PackageSaveError::InvalidPart(e.to_string()))?;
        let root = types.root();
        let ns = "http://schemas.openxmlformats.org/package/2006/content-types";
        if !root.is(ns, "Types") {
            return Ok(false);
        }
        let part = format!("/{name}");
        let mut entries = root
            .children
            .iter()
            .filter(|n| n.is(ns, "Override") && n.attr(None, "PartName") == Some(part.as_str()));
        Ok(entries.next().is_some_and(|n| {
            n.attr(None, "ContentType")
                == Some(
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
                )
        }) && entries.next().is_none())
    }
    /// Build and re-validate the resulting package before performing any write.
    pub(crate) fn prepare(
        &self,
        to: &Path,
        changes: &[(String, Vec<u8>)],
    ) -> Result<Self, PackageSaveError> {
        self.validate_changes(changes, true)?;
        let mut replacements = self
            .edits
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .collect::<BTreeMap<_, _>>();
        for (name, data) in changes {
            let original = self
                .has_part(name)
                .then(|| self.read_original_part(name))
                .transpose()
                .map_err(|e| PackageSaveError::InvalidPart(e.to_string()))?;
            if original.as_ref() == Some(data) {
                replacements.remove(name.as_str());
            } else {
                replacements.insert(name.as_str(), data.as_slice());
            }
        }
        if replacements.is_empty() {
            let mut copy = self.clone();
            copy.path = to.into();
            copy.edits.clear();
            return Ok(copy);
        }
        let data = crate::zip_edit::rewrite(&self.bytes, &replacements, self.limits)?;
        Self::from_bytes(to, data.into(), self.permissions.clone(), self.limits)
            .map_err(|e| PackageSaveError::InvalidPart(e.to_string()))
    }
    pub(crate) fn source(&self) -> &Path {
        &self.path
    }
    pub(crate) fn persist(
        &self,
        prepared: &Package,
        expected: Option<[u8; 32]>,
    ) -> Result<(), PackageSaveError> {
        let to = prepared.source();
        if to == self.path && expected == Some(prepared.hash) {
            return self.check_expected(expected);
        }
        self.save_bytes_with(to, &prepared.bytes, expected, || Ok(()))
    }
    #[cfg(test)]
    pub(crate) fn save_replaced(
        &self,
        to: &Path,
        changes: &[(String, Vec<u8>)],
    ) -> Result<(), PackageSaveError> {
        let prepared = self.prepare(to, changes)?;
        self.persist(&prepared, (to == self.path).then_some(self.hash))
    }
    fn check_expected(&self, expected: Option<[u8; 32]>) -> Result<(), PackageSaveError> {
        if let Some(hash) = expected
            && self.live_hash()? != hash
        {
            return Err(PackageSaveError::ChangedOnDisk);
        }
        Ok(())
    }
    fn save_bytes_with(
        &self,
        to: &Path,
        bytes: &[u8],
        expected: Option<[u8; 32]>,
        before_commit: impl FnOnce() -> std::io::Result<()>,
    ) -> Result<(), PackageSaveError> {
        self.check_expected(expected)?;
        let same = to == self.path;
        if same && expected.is_none() {
            return Err(PackageSaveError::ChangedOnDisk);
        }
        if !same && to.exists() {
            return Err(PackageSaveError::DestinationExists);
        }
        let parent = to
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(bytes)?;
        temp.as_file().set_permissions(self.permissions.clone())?;
        temp.as_file().sync_all()?;
        before_commit()?;
        self.check_expected(expected)?;
        if same {
            temp.persist(to).map_err(|e| e.error)?;
        } else {
            temp.persist_noclobber(to).map_err(|e| e.error)?;
        }
        Ok(())
    }
    pub(crate) fn live_hash(&self) -> Result<[u8; 32], PackageSaveError> {
        let read = || -> std::io::Result<[u8; 32]> {
            let file = open_regular(&self.path)?;
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.len() > self.limits.file_bytes {
                return Err(std::io::Error::other(
                    "source is not a regular bounded file",
                ));
            }
            let mut read = file.take(self.limits.file_bytes.saturating_add(1));
            let mut h = Sha256::new();
            let n = std::io::copy(&mut read, &mut h)?;
            if n > self.limits.file_bytes {
                return Err(std::io::Error::other("source exceeded file limit"));
            }
            Ok(h.finalize().into())
        };
        read().map_err(|_| PackageSaveError::ChangedOnDisk)
    }
}

// ZIP32 preflight preserves duplicate visibility before zip's name-index deduplication.
// ZIP64 and multi-disk are explicitly unsupported in this first slice.
fn preflight_directory(bytes: &[u8], limits: Limits) -> Result<(), OpenError> {
    let u16at = |p: usize| -> Result<u16, OpenError> {
        Ok(u16::from_le_bytes(
            bytes
                .get(p..p + 2)
                .ok_or_else(|| damaged("truncated ZIP"))?
                .try_into()
                .unwrap(),
        ))
    };
    let u32at = |p: usize| -> Result<u32, OpenError> {
        Ok(u32::from_le_bytes(
            bytes
                .get(p..p + 4)
                .ok_or_else(|| damaged("truncated ZIP"))?
                .try_into()
                .unwrap(),
        ))
    };
    let start = bytes.len().saturating_sub(65557);
    let eocd = (start..bytes.len().saturating_sub(21))
        .rev()
        .find(|&i| {
            bytes.get(i..i + 4) == Some(b"PK\x05\x06")
                && u16at(i + 20).is_ok_and(|n| i + 22 + usize::from(n) == bytes.len())
        })
        .ok_or_else(|| damaged("missing ZIP end record"))?;
    let count = usize::from(u16at(eocd + 10)?);
    if u16at(eocd + 4)? != 0
        || u16at(eocd + 6)? != 0
        || usize::from(u16at(eocd + 8)?) != count
        || count == 65535
    {
        return Err(damaged("ZIP64 or split archive unsupported"));
    }
    if count > limits.entries {
        return Err(OpenError::TooBig);
    }
    let size = u32at(eocd + 12)? as usize;
    let mut p = u32at(eocd + 16)? as usize;
    if p.checked_add(size) != Some(eocd) {
        return Err(damaged("invalid central directory extent"));
    }
    let mut names = HashSet::new();
    let mut total = 0u64;
    for _ in 0..count {
        if bytes.get(p..p + 4) != Some(b"PK\x01\x02") {
            return Err(damaged("invalid central entry"));
        }
        let inflated = u32at(p + 24)?;
        if inflated == u32::MAX
            || u32at(p + 20)? == u32::MAX
            || u32at(p + 42)? == u32::MAX
            || u16at(p + 34)? != 0
        {
            return Err(damaged("ZIP64 or split entry unsupported"));
        }
        total = total
            .checked_add(u64::from(inflated))
            .ok_or(OpenError::TooBig)?;
        if total > limits.inflated_bytes {
            return Err(OpenError::TooBig);
        }
        let n = usize::from(u16at(p + 28)?);
        let extra = usize::from(u16at(p + 30)?);
        let comment = usize::from(u16at(p + 32)?);
        let name = bytes
            .get(p + 46..p + 46 + n)
            .ok_or_else(|| damaged("truncated ZIP name"))?;
        if !names.insert(name) {
            return Err(damaged("duplicate entry name"));
        }
        p = p
            .checked_add(46 + n + extra + comment)
            .ok_or(OpenError::TooBig)?;
        if p > eocd {
            return Err(damaged("central entry outside directory"));
        }
    }
    if p != eocd {
        return Err(damaged("central directory count mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injected_failure_after_temp_sync_never_changes_original() {
        let fixture = include_bytes!("../tests/fixtures/chinese-custom-heading.docx");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fixture.docx");
        fs::write(&file, fixture).unwrap();
        let package = Package::open(&file).unwrap();
        let error = package
            .save_bytes_with(&file, b"new bytes", Some(package.hash), || {
                Err(std::io::Error::other("injected after sync"))
            })
            .unwrap_err();
        assert!(matches!(error,PackageSaveError::Io(e) if e.to_string()=="injected after sync"));
        assert_eq!(fs::read(&file).unwrap(), fixture);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn force_save_checks_the_last_observed_source_after_temporary_sync() {
        let fixture = include_bytes!("../tests/fixtures/chinese-custom-heading.docx");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("source.docx");
        fs::write(&file, fixture).unwrap();
        let package = Package::open(&file).unwrap();
        fs::write(&file, b"external file that owner saw").unwrap();
        let confirmed = package.live_hash().unwrap();
        let result = package.save_bytes_with(&file, fixture, Some(confirmed), || {
            fs::write(&file, b"subsequent edit")
        });
        assert!(matches!(result, Err(PackageSaveError::ChangedOnDisk)));
        assert_eq!(fs::read(&file).unwrap(), b"subsequent edit");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
    #[test]
    fn copy_does_not_clobber_existing_destination_or_require_old_source() {
        let fixture = include_bytes!("../tests/fixtures/chinese-custom-heading.docx");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("source.docx");
        fs::write(&file, fixture).unwrap();
        let package = Package::open(&file).unwrap();
        fs::write(&file, b"external changes preserved").unwrap();
        let existing = dir.path().join("already.docx");
        fs::write(&existing, b"never overwritten").unwrap();
        assert!(matches!(
            package.persist(&package.prepare(&existing, &[]).unwrap(), None),
            Err(PackageSaveError::DestinationExists)
        ));
        assert_eq!(fs::read(existing).unwrap(), b"never overwritten");
        let fresh = dir.path().join("new.docx");
        package
            .persist(&package.prepare(&fresh, &[]).unwrap(), None)
            .unwrap();
        assert_eq!(fs::read(fresh).unwrap(), fixture);
        assert_eq!(fs::read(file).unwrap(), b"external changes preserved");
    }
    #[cfg(unix)]
    #[test]
    fn symlink_source_and_existing_destination_never_retarget_save() {
        let fixture = include_bytes!("../tests/fixtures/chinese-custom-heading.docx");
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.docx");
        fs::write(&source, fixture).unwrap();
        let alias = dir.path().join("alias.docx");
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        assert!(Package::open(&alias).is_err());
        let package = Package::open(&source).unwrap();
        let prepared = package.prepare(&alias, &[]).unwrap();
        assert!(matches!(
            package.persist(&prepared, None),
            Err(PackageSaveError::DestinationExists)
        ));
        assert!(fs::symlink_metadata(alias).unwrap().is_symlink());
        assert_eq!(fs::read(source).unwrap(), fixture);
    }
    #[test]
    fn invalid_output_part_leaves_current_source_and_destination_intact() {
        let fixture = include_bytes!("../tests/fixtures/chinese-custom-heading.docx");
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.docx");
        let dest = dir.path().join("not-created.docx");
        fs::write(&source, fixture).unwrap();
        let package = Package::open(&source).unwrap();
        let bad = [("word/document.xml".to_owned(), b"<xml><unclosed>".to_vec())];
        assert!(matches!(
            package.prepare(&dest, &bad),
            Err(PackageSaveError::InvalidPart(_))
        ));
        assert!(!dest.exists());
        assert_eq!(fs::read(source).unwrap(), fixture);
    }
}
