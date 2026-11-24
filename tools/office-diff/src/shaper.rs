//! Measures and shapes text with the system's fonts, through `fontdb` and
//! `rustybuzz`. The tool is the only place that touches real fonts: the
//! layout engine itself still takes a [`Shaper`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use atrium_layout::{Cluster, FontKey, Metrics, Shaper};
use rustybuzz::Direction;

/// One glyph selected for drawing, positioned from the run's origin.
#[derive(Clone, Copy, Debug)]
pub struct DrawGlyph {
    pub glyph: u16,
    /// Points right of the run's x.
    pub x: f32,
    /// Points above the run's baseline.
    pub y: f32,
    /// How far the pen moves after this glyph, in points.
    pub advance: f32,
}

/// A run of glyphs drawn with one face. `size` is the face's size in points.
pub struct DrawRun {
    pub face: Arc<FaceData>,
    pub size: f32,
    pub glyphs: Vec<DrawGlyph>,
}

/// A font file loaded into memory.
pub struct FaceData {
    pub data: Vec<u8>,
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FaceKey {
    family: String,
    cjk: bool,
    bold: bool,
    italic: bool,
}

/// A [`Shaper`] over the system's font database.
pub struct SystemShaper {
    db: fontdb::Database,
    /// Candidate faces per request, best first. A requested family that is
    /// missing or lacks a glyph falls through to the next candidate.
    faces: Mutex<HashMap<FaceKey, Vec<Arc<FaceData>>>>,
}

const CJK_FAMILIES: &[&str] = &[
    "PingFang SC",
    "Songti SC",
    "STSong",
    "Noto Sans CJK SC",
    "Noto Serif CJK SC",
    "Source Han Sans SC",
    "Microsoft YaHei",
    "SimSun",
    "WenQuanYi Micro Hei",
    "Noto Sans SC",
    "Arial Unicode MS",
];

const LATIN_FAMILIES: &[&str] = &[
    "Helvetica",
    "Arial",
    "Liberation Sans",
    "DejaVu Sans",
    "Noto Sans",
    "Segoe UI",
    "Calibri",
    "Times New Roman",
];

/// The same ranges the layout engine uses to decide what needs a CJK face.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2FFF | 0x3000..=0x30FF | 0x3100..=0x31FF
        | 0x3200..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFE30..=0xFE4F
        | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

/// One shaped glyph before it is drawn or measured.
struct ShapedGlyph {
    glyph: u16,
    cluster: usize,
    x: f32,
    y: f32,
    advance: f32,
}

struct Shaped {
    face: Arc<FaceData>,
    glyphs: Vec<ShapedGlyph>,
    missing: bool,
}

impl SystemShaper {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self {
            db,
            faces: Mutex::new(HashMap::new()),
        }
    }

    fn faces_for(&self, key: &FaceKey) -> Vec<Arc<FaceData>> {
        if let Ok(cache) = self.faces.lock()
            && let Some(found) = cache.get(key)
        {
            return found.clone();
        }
        let found = self.load_faces(key);
        if let Ok(mut cache) = self.faces.lock() {
            cache.insert(key.clone(), found.clone());
        }
        found
    }

    fn load_faces(&self, key: &FaceKey) -> Vec<Arc<FaceData>> {
        let mut out = Vec::new();
        let mut seen = Vec::new();
        for family in self.candidates(key) {
            let Some(id) = self.query(&family, key.bold, key.italic) else {
                continue;
            };
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            if let Some(data) = self.load_id(id)
                && !(key.cjk && !covers_cjk(&data))
            {
                out.push(data);
            }
        }
        if out.is_empty() {
            // Nothing matched the requested script: accept any loadable face,
            // so a missing font degrades instead of vanishing.
            for family in LATIN_FAMILIES.iter().chain(CJK_FAMILIES) {
                if let Some(id) = self.query(family, key.bold, key.italic)
                    && let Some(data) = self.load_id(id)
                {
                    out.push(data);
                }
            }
        }
        if out.is_empty()
            && let Some(face) = self.db.faces().next()
            && let Some(data) = self.load_id(face.id)
        {
            out.push(data);
        }
        out
    }

    fn load_id(&self, id: fontdb::ID) -> Option<Arc<FaceData>> {
        self.db
            .with_face_data(id, |data, index| FaceData {
                data: data.to_vec(),
                index,
            })
            .map(Arc::new)
    }

    fn candidates(&self, key: &FaceKey) -> Vec<String> {
        let mut out = Vec::new();
        if !key.family.is_empty() {
            out.push(key.family.clone());
        }
        let list = if key.cjk {
            CJK_FAMILIES
        } else {
            LATIN_FAMILIES
        };
        out.extend(list.iter().map(|s| (*s).to_string()));
        out
    }

    fn query(&self, family: &str, bold: bool, italic: bool) -> Option<fontdb::ID> {
        let query = fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            weight: if bold {
                fontdb::Weight::BOLD
            } else {
                fontdb::Weight::NORMAL
            },
            style: if italic {
                fontdb::Style::Italic
            } else {
                fontdb::Style::Normal
            },
            stretch: fontdb::Stretch::Normal,
        };
        self.db.query(&query)
    }

    fn key_for(&self, font: &FontKey) -> FaceKey {
        FaceKey {
            family: font.family.clone(),
            cjk: font.cjk,
            bold: font.bold,
            italic: font.italic,
        }
    }

    /// Shapes one script group, trying faces until every character has a
    /// glyph. Falls back to the first face when none covers everything.
    fn shape_group(&self, group: &str, key: &FaceKey, size: f32) -> Option<Shaped> {
        let mut first: Option<Shaped> = None;
        for face in self.faces_for(key) {
            let Some(face_rb) = rustybuzz::Face::from_slice(&face.data, face.index) else {
                continue;
            };
            let upem = face_rb.units_per_em() as f32;
            let scale = size / upem.max(1.0);
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(group);
            buffer.set_direction(Direction::LeftToRight);
            let shaped = rustybuzz::shape(&face_rb, &[], buffer);
            let mut glyphs = Vec::new();
            let mut missing = false;
            for (info, pos) in shaped
                .glyph_infos()
                .iter()
                .zip(shaped.glyph_positions().iter())
            {
                if info.glyph_id == 0 {
                    missing = true;
                    continue;
                }
                glyphs.push(ShapedGlyph {
                    glyph: info.glyph_id as u16,
                    cluster: info.cluster as usize,
                    x: pos.x_offset as f32 * scale,
                    y: pos.y_offset as f32 * scale,
                    advance: pos.x_advance as f32 * scale,
                });
            }
            let candidate = Shaped {
                face,
                glyphs,
                missing,
            };
            if !candidate.missing {
                return Some(candidate);
            }
            if first.is_none() {
                first = Some(candidate);
            }
        }
        first
    }

    /// Shapes a whole run and returns it in drawable groups. Groups are split
    /// where the script changes, so Latin and CJK each use their own face.
    pub fn draw_runs(&self, text: &str, font: &FontKey) -> Vec<DrawRun> {
        let mut out = Vec::new();
        let size = font.size.max(0.0);
        for (group, group_cjk) in groups(text) {
            let mut key = self.key_for(font);
            key.cjk = group_cjk;
            let Some(shaped) = self.shape_group(group, &key, size) else {
                continue;
            };
            let glyphs = shaped
                .glyphs
                .into_iter()
                .map(|glyph| DrawGlyph {
                    glyph: glyph.glyph,
                    x: glyph.x,
                    y: glyph.y,
                    advance: glyph.advance,
                })
                .collect();
            out.push(DrawRun {
                face: shaped.face,
                size,
                glyphs,
            });
        }
        out
    }
}

impl Default for SystemShaper {
    fn default() -> Self {
        Self::new()
    }
}

fn covers_cjk(face: &FaceData) -> bool {
    let Ok(parsed) = ttf_parser::Face::parse(&face.data, face.index) else {
        return false;
    };
    parsed.glyph_index('中').is_some()
}

/// Splits text into runs of the same script. Each item is the slice and
/// whether it needs a CJK face.
fn groups(text: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut current: Option<bool> = None;
    for (at, c) in text.char_indices() {
        let cjk = is_cjk(c);
        match current {
            None => current = Some(cjk),
            Some(was) if was != cjk => {
                out.push((&text[start..at], was));
                start = at;
                current = Some(cjk);
            }
            _ => {}
        }
    }
    if let Some(cjk) = current {
        out.push((&text[start..], cjk));
    }
    out
}

impl Shaper for SystemShaper {
    fn shape(&self, text: &str, font: &FontKey) -> Vec<Cluster> {
        let mut out = Vec::with_capacity(text.len());
        let mut offset = 0;
        let size = font.size.max(0.0);
        for (group, group_cjk) in groups(text) {
            let mut key = self.key_for(font);
            key.cjk = group_cjk;
            let base = offset;
            offset += group.len();
            let mut advances: HashMap<usize, f32> = HashMap::new();
            if let Some(shaped) = self.shape_group(group, &key, size) {
                for glyph in shaped.glyphs {
                    *advances.entry(glyph.cluster).or_default() += glyph.advance;
                }
            }
            for (at, c) in group.char_indices() {
                let advance = advances.get(&at).copied().unwrap_or_else(|| {
                    // A ligature or a missing glyph: fall back to a simple
                    // estimate so caret stops stay usable.
                    if c.is_ascii() { size / 2.0 } else { size }
                });
                out.push(Cluster {
                    start: base + at,
                    advance,
                });
            }
        }
        out
    }

    fn metrics(&self, font: &FontKey) -> Metrics {
        let key = self.key_for(font);
        let size = font.size.max(0.0);
        if let Some(face) = self.faces_for(&key).first()
            && let Ok(parsed) = ttf_parser::Face::parse(&face.data, face.index)
        {
            let upem = parsed.units_per_em() as f32;
            let scale = size / upem.max(1.0);
            return Metrics {
                ascent: (parsed.ascender() as f32 * scale).max(size * 0.6),
                descent: (-parsed.descender() as f32 * scale).max(size * 0.15),
                line_gap: (parsed.line_gap() as f32 * scale).max(0.0),
            };
        }
        Metrics {
            ascent: size * 0.8,
            descent: size * 0.2,
            line_gap: 0.0,
        }
    }

    fn underline(&self, font: &FontKey) -> (f32, f32) {
        let key = self.key_for(font);
        let size = font.size.max(0.0);
        if let Some(face) = self.faces_for(&key).first()
            && let Ok(parsed) = ttf_parser::Face::parse(&face.data, face.index)
            && let Some(metrics) = parsed.underline_metrics()
        {
            let scale = size / parsed.units_per_em().max(1) as f32;
            return (
                (-metrics.position as f32 * scale).max(size * 0.05),
                (metrics.thickness as f32 * scale).max(0.5),
            );
        }
        let m = self.metrics(font);
        (m.descent * 0.5, (size / 18.0).max(0.5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_and_cjk_measure_positively() {
        let shaper = SystemShaper::new();
        let latin = FontKey {
            family: String::new(),
            size: 12.0,
            bold: false,
            italic: false,
            cjk: false,
        };
        let clusters = shaper.shape("Hello", &latin);
        assert_eq!(clusters.len(), 5);
        assert!(clusters.iter().all(|c| c.advance > 0.0), "{clusters:?}");
        let cjk = FontKey {
            cjk: true,
            ..latin.clone()
        };
        let clusters = shaper.shape("预算", &cjk);
        assert_eq!(clusters.len(), 2);
        assert!(clusters.iter().all(|c| c.advance > 0.0), "{clusters:?}");
        let metrics = shaper.metrics(&cjk);
        assert!(metrics.ascent > 0.0 && metrics.descent > 0.0);
    }

    #[test]
    fn groups_split_by_script() {
        let out = groups("ab中文cd");
        assert_eq!(out, vec![("ab", false), ("中文", true), ("cd", false)]);
    }

    #[test]
    fn an_unavailable_family_still_draws_a_bullet() {
        let shaper = SystemShaper::new();
        let font = FontKey {
            family: "Symbol".into(),
            size: 12.0,
            bold: false,
            italic: false,
            cjk: false,
        };
        let runs = shaper.draw_runs("•", &font);
        assert!(
            runs.iter().any(|run| !run.glyphs.is_empty()),
            "no glyph drawn for a bullet"
        );
    }
}
