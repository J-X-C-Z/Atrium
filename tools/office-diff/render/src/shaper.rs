//! A [`atrium_layout::Shaper`] over the fonts this machine has, so the
//! comparison harness lays text out with real advances instead of
//! [`atrium_layout::FixedShaper`]'s square guesses.
//!
//! Text is shaped with rustybuzz. A character the requested face lacks
//! falls back to a CJK or Latin face that has it, which is what the app's
//! text system does.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use atrium_layout::{Cluster, FontKey, Metrics, Shaper};
use rustybuzz::{Direction, UnicodeBuffer};

/// Families tried for Chinese, Japanese and Korean text, in order.
const CJK_FAMILIES: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans CJK JP",
    "Source Han Sans SC",
    "Source Han Sans",
    "PingFang SC",
    "Hiragino Sans GB",
    "Microsoft YaHei",
    "SimSun",
    "Noto Serif CJK SC",
    "WenQuanYi Micro Hei",
];

/// Families tried for other text when the requested one is missing.
const LATIN_FAMILIES: &[&str] = &[
    "Helvetica",
    "Arial",
    "Liberation Sans",
    "Noto Sans",
    "DejaVu Sans",
    "Carlito",
    "Times New Roman",
];

#[derive(Clone)]
struct FaceData {
    bytes: Arc<Vec<u8>>,
    index: u32,
}

pub struct SystemShaper {
    db: fontdb::Database,
    faces: Mutex<HashMap<fontdb::ID, Option<FaceData>>>,
    /// (family, bold, italic) to the face chosen for that request.
    chosen: Mutex<HashMap<(String, bool, bool), Option<fontdb::ID>>>,
    glyphs: Mutex<HashMap<(fontdb::ID, char), bool>>,
}

impl SystemShaper {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self {
            db,
            faces: Mutex::new(HashMap::new()),
            chosen: Mutex::new(HashMap::new()),
            glyphs: Mutex::new(HashMap::new()),
        }
    }

    fn face_data(&self, id: fontdb::ID) -> Option<FaceData> {
        if let Some(found) = self.faces.lock().unwrap().get(&id) {
            return found.clone();
        }
        let found = self.load_face(id);
        self.faces.lock().unwrap().insert(id, found.clone());
        found
    }

    fn load_face(&self, id: fontdb::ID) -> Option<FaceData> {
        let info = self.db.face(id)?;
        let index = info.index;
        let bytes = match &info.source {
            fontdb::Source::File(path) => std::fs::read(path).ok()?,
            fontdb::Source::Binary(data) => data.as_ref().as_ref().to_vec(),
            _ => return None,
        };
        Some(FaceData {
            bytes: Arc::new(bytes),
            index,
        })
    }

    fn query(&self, name: &str, bold: bool, italic: bool) -> Option<fontdb::ID> {
        let families = [fontdb::Family::Name(name)];
        self.db.query(&fontdb::Query {
            families: &families,
            weight: if bold {
                fontdb::Weight::BOLD
            } else {
                fontdb::Weight::NORMAL
            },
            stretch: fontdb::Stretch::Normal,
            style: if italic {
                fontdb::Style::Italic
            } else {
                fontdb::Style::Normal
            },
        })
    }

    fn chosen_face(&self, font: &FontKey) -> Option<fontdb::ID> {
        let key = (font.family.clone(), font.bold, font.italic);
        if let Some(found) = self.chosen.lock().unwrap().get(&key) {
            return *found;
        }
        let mut found = None;
        if !font.family.is_empty() {
            found = self.query(&font.family, font.bold, font.italic);
        }
        if found.is_none() {
            found = self.query("Noto Sans CJK SC", font.bold, font.italic);
        }
        if found.is_none() {
            for family in LATIN_FAMILIES {
                if let Some(id) = self.query(family, font.bold, font.italic) {
                    found = Some(id);
                    break;
                }
            }
        }
        self.chosen.lock().unwrap().insert(key, found);
        found
    }

    fn has_glyph(&self, id: fontdb::ID, c: char) -> bool {
        if let Some(found) = self.glyphs.lock().unwrap().get(&(id, c)) {
            return *found;
        }
        let found = match self.face_data(id) {
            Some(data) => ttf_parser::Face::parse(&data.bytes, data.index)
                .ok()
                .and_then(|face| face.glyph_index(c))
                .is_some(),
            None => false,
        };
        self.glyphs.lock().unwrap().insert((id, c), found);
        found
    }

    /// The face for `c`: the requested one when it has the glyph, else a
    /// CJK face for CJK characters and the usual Latin faces otherwise.
    fn face_for(&self, font: &FontKey, primary: Option<fontdb::ID>, c: char) -> Option<fontdb::ID> {
        if let Some(id) = primary {
            if self.has_glyph(id, c) {
                return Some(id);
            }
        }
        let cjk = is_cjk(c);
        let order: &[&str] = if cjk { CJK_FAMILIES } else { LATIN_FAMILIES };
        for family in order {
            if let Some(id) = self.query(family, font.bold, font.italic) {
                if self.has_glyph(id, c) {
                    return Some(id);
                }
            }
        }
        // Last resort: a CJK face often covers punctuation other faces miss.
        for family in CJK_FAMILIES {
            if let Some(id) = self.query(family, font.bold, font.italic) {
                if self.has_glyph(id, c) {
                    return Some(id);
                }
            }
        }
        primary
    }

    /// Shapes one same-face run. Returns the advance of every character,
    /// keyed by its byte offset in `text`, and the run's total width.
    fn shape_run(&self, text: &str, id: fontdb::ID, size: f32) -> (Vec<(usize, f32)>, f32) {
        let Some(data) = self.face_data(id) else {
            return self.fallback_advances(text, size);
        };
        let Some(face) = rustybuzz::Face::from_slice(&data.bytes, data.index) else {
            return self.fallback_advances(text, size);
        };
        let upem = face.units_per_em() as f32;
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        if buffer.direction() != Direction::RightToLeft {
            buffer.set_direction(Direction::LeftToRight);
        }
        let shaped = rustybuzz::shape(&face, &[], buffer);
        let infos = shaped.glyph_infos();
        let positions = shaped.glyph_positions();

        // Where each glyph starts, in bytes and in points.
        let mut starts: Vec<(usize, f32)> = Vec::with_capacity(infos.len());
        let mut x = 0.0f32;
        for (info, pos) in infos.iter().zip(positions.iter()) {
            starts.push((info.cluster as usize, x));
            x += pos.x_advance as f32 * size / upem;
        }
        let total = x;
        if starts.is_empty() {
            return (text.char_indices().map(|(i, _)| (i, 0.0)).collect(), 0.0);
        }
        let at = |byte: usize| -> f32 {
            let mut found = starts[0].1;
            for (start, x) in &starts {
                if *start <= byte {
                    found = *x;
                } else {
                    break;
                }
            }
            found
        };
        let mut bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        bounds.push(text.len());
        let mut out = Vec::with_capacity(bounds.len().saturating_sub(1));
        for pair in bounds.windows(2) {
            let start = pair[0];
            let end = pair[1];
            out.push((start, (at(end) - at(start)).max(0.0)));
        }
        (out, total)
    }

    fn fallback_advances(&self, text: &str, size: f32) -> (Vec<(usize, f32)>, f32) {
        let out: Vec<(usize, f32)> = text
            .chars()
            .map(|c| {
                if c.is_ascii() {
                    (0, size * 0.5)
                } else {
                    (0, size)
                }
            })
            .collect();
        let mut with_starts = Vec::with_capacity(out.len());
        let mut total = 0.0;
        for ((start, _), (_, advance)) in text.char_indices().zip(out) {
            with_starts.push((start, advance));
            total += advance;
        }
        (with_starts, total)
    }
}

impl Default for SystemShaper {
    fn default() -> Self {
        Self::new()
    }
}

impl Shaper for SystemShaper {
    fn shape(&self, text: &str, font: &FontKey) -> Vec<Cluster> {
        if text.is_empty() {
            return Vec::new();
        }
        let primary = self.chosen_face(font);
        let mut out: Vec<Cluster> = Vec::new();
        let mut run_start = 0usize;
        let mut run_face: Option<fontdb::ID> = None;
        for (byte, c) in text.char_indices() {
            let face = self.face_for(font, primary, c);
            if face != run_face && byte > run_start {
                let (advances, _) = match run_face {
                    Some(id) => self.shape_run(&text[run_start..byte], id, font.size),
                    None => self.fallback_advances(&text[run_start..byte], font.size),
                };
                let run = &text[run_start..byte];
                for (start, advance) in advances {
                    out.push(Cluster {
                        start: run_start + start.min(run.len()),
                        advance,
                    });
                }
                run_start = byte;
            }
            run_face = face;
        }
        let (advances, _) = match run_face {
            Some(id) => self.shape_run(&text[run_start..], id, font.size),
            None => self.fallback_advances(&text[run_start..], font.size),
        };
        let run = &text[run_start..];
        for (start, advance) in advances {
            out.push(Cluster {
                start: run_start + start.min(run.len()),
                advance,
            });
        }
        out
    }

    fn metrics(&self, font: &FontKey) -> Metrics {
        let Some(id) = self.chosen_face(font) else {
            return atrium_layout::FixedShaper.metrics(font_for_metrics(font));
        };
        let Some(data) = self.face_data(id) else {
            return atrium_layout::FixedShaper.metrics(font_for_metrics(font));
        };
        let Some(face) = ttf_parser::Face::parse(&data.bytes, data.index).ok() else {
            return atrium_layout::FixedShaper.metrics(font_for_metrics(font));
        };
        let scale = font.size / face.units_per_em() as f32;
        Metrics {
            ascent: face.ascender() as f32 * scale,
            descent: -(face.descender() as f32) * scale,
            line_gap: face.line_gap() as f32 * scale,
        }
    }
}

fn font_for_metrics(font: &FontKey) -> &FontKey {
    font
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x2FFF
        | 0x3000..=0x30FF
        | 0x3100..=0x31FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF
        | 0x20000..=0x2FA1F)
}
