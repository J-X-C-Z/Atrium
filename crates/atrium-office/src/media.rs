//! Internal Word image bytes. Nothing is fetched, decoded, or written to disk.
use crate::{OpenError, Package};

const WORD: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const WORD_STRICT: &str = "http://purl.oclc.org/ooxml/wordprocessingml/main";

/// Maximum inflated size of a single image returned by read_image.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// An internal image relationship declared by the Word main document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaRef {
    relationship_id: String,
    target: String,
    archive_hash: [u8; 32],
}

impl MediaRef {
    pub fn relationship_id(&self) -> &str {
        &self.relationship_id
    }

    /// An OPC part name, not an operating system path.
    pub fn part_name(&self) -> &str {
        &self.target
    }
}

fn damaged(message: &str) -> OpenError {
    OpenError::Damaged(message.into())
}

/// List main-document image relationships in source order. Does not load
/// images or resolve external links. Header/footer relationships are omitted.
pub fn embedded_word_images(package: &Package) -> Result<Vec<MediaRef>, OpenError> {
    let document = package
        .related_part("", "officeDocument")?
        .unwrap_or_else(|| "word/document.xml".into());
    let xml = package.part(&document)?;
    if ![WORD, WORD_STRICT]
        .iter()
        .any(|namespace| xml.root().is(namespace, "document"))
    {
        return Err(damaged("not a WordprocessingML document"));
    }

    package
        .relationships(&document)?
        .into_iter()
        .filter(|rel| !rel.external && rel.kind("image"))
        .map(|rel| {
            if !package.has_part(&rel.target) {
                return Err(damaged("missing embedded image relationship target"));
            }
            Ok(MediaRef {
                relationship_id: rel.id,
                target: rel.target,
                archive_hash: package.original_hash(),
            })
        })
        .collect()
}

/// Read opaque image bytes. Callers must apply decoder limits and choose
/// supported image formats before displaying untrusted content.
pub fn read_image(package: &Package, image: &MediaRef) -> Result<Vec<u8>, OpenError> {
    if package.original_hash() != image.archive_hash {
        return Err(damaged("image reference belongs to another package"));
    }
    package.read_part_with_limit(&image.target, MAX_IMAGE_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::File,
        io::{Read, Write, repeat},
        path::Path,
    };
    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    const LINK_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/";
    const REL_NS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

    fn build(path: &Path, size: u64, missing: bool, word_namespace: &str) {
        let mut zip = ZipWriter::new(File::create(path).unwrap());
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, body) in [
            (
                "[Content_Types].xml",
                "<Types xmlns='http://schemas.openxmlformats.org/package/2006/content-types'><Override PartName='/custom/main.xml' ContentType='application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml'/><Default Extension='png' ContentType='image/png'/></Types>".to_owned(),
            ),
            (
                "_rels/.rels",
                format!("<Relationships xmlns='{REL_NS}'><Relationship Id='document' Type='{LINK_NS}officeDocument' Target='/custom/main.xml'/></Relationships>"),
            ),
            (
                "custom/main.xml",
                format!("<word:document xmlns:word='{word_namespace}'><word:body/></word:document>"),
            ),
            (
                "custom/_rels/main.xml.rels",
                format!("<Relationships xmlns='{REL_NS}'><Relationship Id='external' Type='{LINK_NS}image' TargetMode='External' Target='https://example.invalid/photo.png'/><Relationship Id='rId9' Type='{LINK_NS}image' Target='../media/%E7%8C%AB.png'/><Relationship Id='notImage' Type='{LINK_NS}styles' Target='styles.xml'/></Relationships>"),
            ),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        }
        if !missing {
            zip.start_file("media/猫.png", options).unwrap();
            std::io::copy(&mut repeat(23).take(size), &mut zip).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn internal_nonstandard_target_reads_without_fetching_links() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.docx");
        build(&source, 11, false, WORD_STRICT);
        let package = Package::open(&source).unwrap();
        let list = embedded_word_images(&package).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].relationship_id(), "rId9");
        assert_eq!(list[0].part_name(), "media/猫.png");
        assert_eq!(read_image(&package, &list[0]).unwrap(), [23; 11]);

        build(&source, 12, false, WORD_STRICT);
        assert!(read_image(&Package::open(&source).unwrap(), &list[0]).is_err());
    }

    #[test]
    fn oversized_target_is_listed_but_not_loaded() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("large.docx");
        build(&source, MAX_IMAGE_BYTES + 1, false, WORD);
        let package = Package::open(&source).unwrap();
        let list = embedded_word_images(&package).unwrap();
        assert!(matches!(
            read_image(&package, &list[0]),
            Err(OpenError::TooBig)
        ));
    }

    #[test]
    fn missing_image_or_non_word_main_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.docx");
        for (missing, namespace) in [(true, WORD), (false, "urn:other")] {
            build(&source, 11, missing, namespace);
            assert!(embedded_word_images(&Package::open(&source).unwrap()).is_err());
        }
    }
}
