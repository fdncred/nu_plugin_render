//! Fonts: finding them on the system, shaping text, and drawing glyphs.
//!
//! Nothing is bundled. Every good free font is OFL-licensed, so faces come
//! from the system through `fontdb`. A character the main face lacks is
//! looked up in a list of fallback families, then in every installed face.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};

use fontdb::{Database, Family, ID, Query, Stretch, Style, Weight};
use swash::scale::image::Image;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::shape::ShapeContext;
use swash::text::{Codepoint as _, Script};
use swash::zeno::Format;
use swash::{CacheKey, FontRef};
use unicode_width::UnicodeWidthChar as _;

use crate::style::Rgba;

/// Sans-serif families for table text, best first.
const SANS: &[&str] = &[
    "Inter",
    "SF Pro Text",
    "SF Pro",
    "Helvetica Neue",
    "Segoe UI",
    "Noto Sans",
    "Cantarell",
    "DejaVu Sans",
    "Arial",
];

/// Monospace families, for `--mono`.
const MONO: &[&str] = &[
    "JetBrains Mono",
    "SF Mono",
    "Menlo",
    "Cascadia Mono",
    "Consolas",
    "DejaVu Sans Mono",
    "Courier New",
];

/// Color emoji families.
const EMOJI: &[&str] = &["Apple Color Emoji", "Noto Color Emoji", "Segoe UI Emoji"];

/// Families to look in for other characters the main face lacks, such as
/// symbols and CJK.
const FALLBACK: &[&str] = &[
    "Segoe UI Symbol",
    "Apple Symbols",
    "PingFang SC",
    "Hiragino Sans",
    "Apple SD Gothic Neo",
    "Noto Sans CJK SC",
    "Microsoft YaHei",
    "Arial Unicode MS",
    "Menlo",
    "DejaVu Sans",
    "Symbola",
];

/// Whether `c` is in a private use area.
fn is_private_use(c: char) -> bool {
    matches!(
        u32::from(c),
        0xE000..=0xF8FF | 0xF_0000..=0xF_FFFD | 0x10_0000..=0x10_FFFD
    )
}

/// Installed Nerd Font faces, symbols-only and Mono variants first.
fn nerd_fonts(db: &Database) -> Vec<ID> {
    let rank = |family: &str| {
        if !(family.contains("Nerd Font") || family.ends_with(" NF") || family.ends_with(" NFM")) {
            None
        } else if family.starts_with("Symbols Nerd Font") {
            Some(0)
        } else if family.contains("Mono") {
            Some(1)
        } else {
            Some(2)
        }
    };
    let mut found: Vec<(u8, ID)> = db
        .faces()
        .filter(|info| info.style == Style::Normal && info.weight == Weight::NORMAL)
        .filter_map(|info| {
            let family = &info.families.first()?.0;
            Some((rank(family)?, info.id))
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, id)| id).take(4).collect()
}

/// Index of a loaded face in [`Fonts`].
pub type FaceId = usize;

/// A loaded face: its bytes, mapped from the file, and where in them the
/// face starts.
pub struct Face {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    offset: u32,
    /// swash keys its shaping and scaling caches by this, so it must stay
    /// the same for the face's whole life.
    key: CacheKey,
    /// The family name, for diagnostics.
    pub family: String,
}

impl Face {
    fn new(data: Arc<dyn AsRef<[u8]> + Send + Sync>, index: u32, family: String) -> Option<Face> {
        let font = FontRef::from_index((*data).as_ref(), index as usize)?;
        let (offset, key) = (font.offset, font.key);
        Some(Face {
            data,
            offset,
            key,
            family,
        })
    }

    pub fn font(&self) -> FontRef<'_> {
        FontRef {
            data: (*self.data).as_ref(),
            offset: self.offset,
            key: self.key,
        }
    }

    fn has(&self, c: char) -> bool {
        self.font().charmap().map(c) != 0
    }
}

/// The regular and bold faces of one family.
#[derive(Clone, Copy, Debug)]
pub struct FacePair {
    pub regular: FaceId,
    pub bold: FaceId,
}

/// The system's fonts, loaded once per plugin process.
pub struct Fonts {
    inner: Mutex<Inner>,
    /// Installed Nerd Fonts, for icons in the private use area.
    nerd: Vec<ID>,
    emoji: Vec<ID>,
    fallbacks: Vec<ID>,
    pub sans: FacePair,
    pub mono: FacePair,
}

struct Inner {
    db: Database,
    faces: Vec<Arc<Face>>,
    loaded: HashMap<ID, FaceId>,
    /// The face found for each character the main faces lack.
    by_char: HashMap<char, Option<FaceId>>,
}

impl Inner {
    /// The first of `names` that is installed, else the generic family.
    fn pick(&mut self, names: &[&str], generic: Family<'_>, weight: Weight) -> Option<FaceId> {
        let id = names
            .iter()
            .find_map(|name| self.find(Family::Name(name), weight))
            .or_else(|| self.find(generic, weight))?;
        self.load(id)
    }

    fn find(&self, family: Family<'_>, weight: Weight) -> Option<ID> {
        let families = [family];
        self.db.query(&Query {
            families: &families,
            weight,
            stretch: Stretch::Normal,
            style: Style::Normal,
        })
    }

    fn load(&mut self, id: ID) -> Option<FaceId> {
        if let Some(&face) = self.loaded.get(&id) {
            return Some(face);
        }
        // SAFETY: fontdb maps the font file read-only. The caveat is that
        // another process could change the file while it is mapped; installed
        // fonts don't change while a table is drawn.
        let (data, index) = unsafe { self.db.make_shared_face_data(id) }?;
        let family = self
            .db
            .face(id)
            .and_then(|info| info.families.first())
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        let face = Face::new(data, index, family)?;
        let face_id = self.faces.len();
        self.faces.push(Arc::new(face));
        self.loaded.insert(id, face_id);
        Some(face_id)
    }

    /// Any installed face with `c`, for characters no fallback family has.
    fn scan(&mut self, c: char) -> Option<FaceId> {
        let candidates: Vec<ID> = self
            .db
            .faces()
            .filter(|info| info.style == Style::Normal)
            .map(|info| info.id)
            .collect();
        let id = candidates.into_iter().find(|&id| {
            self.db
                .with_face_data(id, |data, index| {
                    FontRef::from_index(data, index as usize)
                        .is_some_and(|font| font.charmap().map(c) != 0)
                })
                .unwrap_or(false)
        })?;
        self.load(id)
    }
}

impl Fonts {
    /// Read the system's font list and pick the faces tables use.
    ///
    /// # Errors
    ///
    /// Fails when no sans-serif or monospace face can be loaded at all.
    pub fn load() -> Result<Fonts, String> {
        let mut db = Database::new();
        db.load_system_fonts();
        let mut inner = Inner {
            db,
            faces: Vec::new(),
            loaded: HashMap::new(),
            by_char: HashMap::new(),
        };
        let missing = |what: &str| format!("no {what} font is installed");
        let sans = FacePair {
            regular: inner
                .pick(SANS, Family::SansSerif, Weight::NORMAL)
                .ok_or_else(|| missing("sans-serif"))?,
            bold: inner
                .pick(SANS, Family::SansSerif, Weight::SEMIBOLD)
                .ok_or_else(|| missing("bold sans-serif"))?,
        };
        let mono = FacePair {
            regular: inner
                .pick(MONO, Family::Monospace, Weight::NORMAL)
                .ok_or_else(|| missing("monospace"))?,
            bold: inner
                .pick(MONO, Family::Monospace, Weight::BOLD)
                .ok_or_else(|| missing("bold monospace"))?,
        };
        let nerd = nerd_fonts(&inner.db);
        let installed = |names: &[&str], inner: &Inner| -> Vec<ID> {
            names
                .iter()
                .filter_map(|name| inner.find(Family::Name(name), Weight::NORMAL))
                .collect()
        };
        let emoji = installed(EMOJI, &inner);
        let fallbacks = installed(FALLBACK, &inner);
        Ok(Fonts {
            inner: Mutex::new(inner),
            nerd,
            emoji,
            fallbacks,
            sans,
            mono,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while the lock was held leaves the tables usable.
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn face(&self, id: FaceId) -> Option<Arc<Face>> {
        self.lock().faces.get(id).cloned()
    }

    /// The face to draw `c` in: `base` when it has the character, else the
    /// first fallback that does.
    ///
    /// A private-use character goes to a Nerd Font first, even when `base`
    /// has it: Nerd Font icons live there, and other fonts put unrelated
    /// symbols at the same codepoints.
    pub fn face_for(&self, base: FaceId, c: char) -> FaceId {
        let mut inner = self.lock();
        if is_private_use(c)
            && let Some(face) = self.nerd.iter().find_map(|&id| {
                let face = inner.load(id)?;
                inner.faces[face].has(c).then_some(face)
            })
        {
            return face;
        }
        if inner.faces.get(base).is_some_and(|face| face.has(c)) {
            return base;
        }
        if let Some(found) = inner.by_char.get(&c) {
            return found.unwrap_or(base);
        }
        // A wide character is an emoji, so color emoji fonts go first. A
        // narrow one, such as ♥, wants a text glyph that fits one cell.
        let (first, then) = if c.width() == Some(2) {
            (&self.emoji, &self.fallbacks)
        } else {
            (&self.fallbacks, &self.emoji)
        };
        let found = first
            .iter()
            .chain(then)
            .find_map(|&id| {
                let face = inner.load(id)?;
                inner.faces[face].has(c).then_some(face)
            })
            .or_else(|| inner.scan(c));
        inner.by_char.insert(c, found);
        found.unwrap_or(base)
    }
}

/// One glyph placed on a line.
#[derive(Clone, Copy, Debug)]
pub struct Glyph {
    pub face: FaceId,
    pub id: u16,
    /// Where to draw it, relative to the line's start and baseline.
    pub x: f32,
    pub y: f32,
    /// The pen position before it, and how far it moves the pen.
    pub pen: f32,
    pub advance: f32,
    /// Its own color, from an ANSI escape in the text. `None` draws it in
    /// the color of the cell it's in.
    pub color: Option<Rgba>,
}

/// A shaped line of text, starting at x = 0.
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub glyphs: Vec<Glyph>,
    pub width: f32,
    /// Background colors behind parts of the line, from ANSI escapes.
    pub highlights: Vec<Highlight>,
}

/// A background color from `start` to `end` along a line.
#[derive(Clone, Copy, Debug)]
pub struct Highlight {
    pub start: f32,
    pub end: f32,
    pub color: Rgba,
}

impl Line {
    /// Cut the line to at most `max` wide, ending it in `ellipsis`.
    pub fn truncate(&mut self, max: f32, ellipsis: &Line) {
        if self.width <= max {
            return;
        }
        let room = max - ellipsis.width;
        let keep = self
            .glyphs
            .iter()
            .position(|glyph| glyph.pen + glyph.advance > room)
            .unwrap_or(self.glyphs.len());
        self.glyphs.truncate(keep);
        let start = self
            .glyphs
            .last()
            .map_or(0.0, |glyph| glyph.pen + glyph.advance);
        self.glyphs
            .extend(ellipsis.glyphs.iter().map(|glyph| Glyph {
                x: glyph.x + start,
                pen: glyph.pen + start,
                ..*glyph
            }));
        self.width = start + ellipsis.width;
        self.highlights.retain(|highlight| highlight.start < start);
        for highlight in &mut self.highlights {
            highlight.end = highlight.end.min(start);
        }
    }

    /// Add `other` to the end of this line. A highlight that continues one
    /// in the same color joins it, so a colored run stays one piece.
    pub fn extend(&mut self, other: Line) {
        let offset = self.width;
        self.glyphs
            .extend(other.glyphs.into_iter().map(|glyph| Glyph {
                x: glyph.x + offset,
                pen: glyph.pen + offset,
                ..glyph
            }));
        for highlight in other.highlights {
            let (start, end) = (highlight.start + offset, highlight.end + offset);
            match self.highlights.last_mut() {
                Some(last) if last.color == highlight.color && (last.end - start).abs() < 0.5 => {
                    last.end = end;
                }
                _ => self.highlights.push(Highlight {
                    start,
                    end,
                    color: highlight.color,
                }),
            }
        }
        self.width += other.width;
    }
}

/// Whether `c` belongs with the character before it: joiners, variation
/// selectors, skin tones, keycaps, tags, and combining marks.
pub fn joins(c: char) -> bool {
    matches!(
        u32::from(c),
        0x200D | 0xFE00..=0xFE0F | 0x20E3 | 0x1F3FB..=0x1F3FF | 0x0300..=0x036F | 0xE0020..=0xE007F
    )
}

/// Shapes text at one size.
pub struct Shaper<'a> {
    fonts: &'a Fonts,
    context: ShapeContext,
    pub px: f32,
    /// The widest digit of each face, for tabular numbers.
    digit_widths: HashMap<FaceId, f32>,
    /// The face each character drew in, by base face.
    faces: HashMap<(FaceId, char), FaceId>,
}

impl<'a> Shaper<'a> {
    pub fn new(fonts: &'a Fonts, px: f32) -> Self {
        Shaper {
            fonts,
            context: ShapeContext::new(),
            px,
            digit_widths: HashMap::new(),
            faces: HashMap::new(),
        }
    }

    /// Shape `text` in `base`, with fallback faces where it lacks a
    /// character. With `tabular`, every ASCII digit takes the width of the
    /// widest one, so numbers line up even in faces without `tnum`.
    pub fn line(&mut self, text: &str, base: FaceId, tabular: bool) -> Line {
        let mut line = Line::default();
        self.append(&mut line, text, base, tabular, None);
        line
    }

    /// Shape `text` onto the end of `line`, its glyphs in `color`.
    pub fn append(
        &mut self,
        line: &mut Line,
        text: &str,
        base: FaceId,
        tabular: bool,
        color: Option<Rgba>,
    ) {
        let start = line.glyphs.len();
        for (range, face) in self.runs(text, base) {
            self.shape_run(&text[range], face, tabular, line);
        }
        for glyph in &mut line.glyphs[start..] {
            glyph.color = color;
        }
    }

    /// Split `text` into runs that each draw in one face.
    fn runs(&mut self, text: &str, base: FaceId) -> Vec<(Range<usize>, FaceId)> {
        let mut runs: Vec<(Range<usize>, FaceId)> = Vec::new();
        let mut after_joiner = false;
        for (start, c) in text.char_indices() {
            let end = start + c.len_utf8();
            let face = match runs.last() {
                Some((_, previous)) if after_joiner || joins(c) => *previous,
                _ => *self
                    .faces
                    .entry((base, c))
                    .or_insert_with(|| self.fonts.face_for(base, c)),
            };
            after_joiner = c == '\u{200D}';
            match runs.last_mut() {
                Some((range, last)) if *last == face => range.end = end,
                _ => runs.push((start..end, face)),
            }
        }
        runs
    }

    fn digit_width(&mut self, face_id: FaceId) -> f32 {
        if let Some(&width) = self.digit_widths.get(&face_id) {
            return width;
        }
        let mut widest = 0.0f32;
        if let Some(face) = self.fonts.face(face_id) {
            let mut shaper = self
                .context
                .builder(face.font())
                .size(self.px)
                .features(&[(*b"tnum", 1u16)])
                .build();
            shaper.add_str("0123456789");
            shaper.shape_with(|cluster| {
                for glyph in cluster.glyphs {
                    widest = widest.max(glyph.advance);
                }
            });
        }
        self.digit_widths.insert(face_id, widest);
        widest
    }

    fn shape_run(&mut self, text: &str, face_id: FaceId, tabular: bool, line: &mut Line) {
        let digit = if tabular {
            Some(self.digit_width(face_id))
        } else {
            None
        };
        let Some(face) = self.fonts.face(face_id) else {
            return;
        };
        let font = face.font();
        let script = text
            .chars()
            .map(|c| c.script())
            .find(|script| !matches!(script, Script::Common | Script::Inherited | Script::Unknown))
            .unwrap_or(Script::Latin);
        let features: &[([u8; 4], u16)] = if tabular { &[(*b"tnum", 1)] } else { &[] };
        let mut shaper = self
            .context
            .builder(font)
            .script(script)
            .size(self.px)
            .features(features)
            .build();
        shaper.add_str(text);
        let mut pen = line.width;
        shaper.shape_with(|cluster| {
            let source = cluster.source.to_range();
            let is_digit = text
                .get(source)
                .is_some_and(|s| s.len() == 1 && s.as_bytes()[0].is_ascii_digit());
            for glyph in cluster.glyphs {
                let (advance, dx) = match digit {
                    Some(width) if is_digit => (width, (width - glyph.advance) / 2.0),
                    _ => (glyph.advance, 0.0),
                };
                line.glyphs.push(Glyph {
                    face: face_id,
                    id: glyph.id,
                    x: pen + glyph.x + dx,
                    y: glyph.y,
                    pen,
                    advance,
                    color: None,
                });
                pen += advance;
            }
        });
        line.width = pen;
    }
}

/// Draws glyphs into alpha masks or color bitmaps, and keeps them.
pub struct Rasterizer<'a> {
    fonts: &'a Fonts,
    context: ScaleContext,
    cache: HashMap<(FaceId, u16, u32), Option<Image>>,
}

impl<'a> Rasterizer<'a> {
    pub fn new(fonts: &'a Fonts) -> Self {
        Rasterizer {
            fonts,
            context: ScaleContext::new(),
            cache: HashMap::new(),
        }
    }

    /// Glyph `id` of `face` at `px`: color for emoji, a mask otherwise.
    pub fn glyph(&mut self, face: FaceId, id: u16, px: f32) -> Option<&Image> {
        let key = (face, id, (px * 64.0) as u32);
        if !self.cache.contains_key(&key) {
            let image = self.fonts.face(face).and_then(|face| {
                let mut scaler = self
                    .context
                    .builder(face.font())
                    .size(px)
                    .hint(true)
                    .build();
                Render::new(&[
                    Source::ColorOutline(0),
                    Source::ColorBitmap(StrikeWith::BestFit),
                    Source::Outline,
                ])
                .format(Format::Alpha)
                .render(&mut scaler, id)
            });
            self.cache.insert(key, image);
        }
        self.cache.get(&key).and_then(Option::as_ref)
    }
}

/// Font metrics at `px`: ascent, descent, and leading.
pub fn metrics(fonts: &Fonts, face: FaceId, px: f32) -> Option<swash::Metrics> {
    let face = fonts.face(face)?;
    Some(face.font().metrics(&[]).scale(px))
}
