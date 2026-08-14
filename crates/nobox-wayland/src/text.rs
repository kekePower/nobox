use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{Hash, Hasher},
};

use fontdb::{Database, Family, Query};
use fontdue::{Font, FontSettings, Metrics};
use nobox_core::Geometry;
use thiserror::Error;

const MAX_CACHED_GLYPHS: usize = 4_096;

/// Failure to find or parse a usable system font.
#[derive(Debug, Error)]
pub(crate) enum TextRendererError {
    /// No matching TrueType/OpenType face was installed.
    #[error("no usable system font was found")]
    MissingFont,
    /// The selected face could not be parsed by the safe rasterizer.
    #[error("the selected system font could not be parsed")]
    InvalidFont,
}

/// One same-coverage horizontal glyph run ready for compositor rendering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TextRun {
    pub(crate) geometry: Geometry,
    pub(crate) coverage: u8,
}

#[derive(Clone, Copy, Debug, Eq)]
struct GlyphKey {
    character: char,
    pixels: u16,
}

impl PartialEq for GlyphKey {
    fn eq(&self, other: &Self) -> bool {
        self.character == other.character && self.pixels == other.pixels
    }
}

impl Hash for GlyphKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.character.hash(state);
        self.pixels.hash(state);
    }
}

#[derive(Clone, Debug)]
struct GlyphRaster {
    metrics: Metrics,
    runs: Vec<TextRun>,
}

/// System-font loader and bounded glyph raster cache shared by compositor UI.
pub(crate) struct TextRenderer {
    font: Font,
    glyphs: RefCell<HashMap<GlyphKey, GlyphRaster>>,
}

impl TextRenderer {
    /// Loads the configured family with predictable common sans-serif fallbacks.
    pub(crate) fn load(configured_font: &str) -> Result<Self, TextRendererError> {
        let mut database = Database::new();
        database.load_system_fonts();
        let requested = configured_family(configured_font).unwrap_or("DejaVu Sans");
        let families = [
            Family::Name(requested),
            Family::Name("DejaVu Sans"),
            Family::Name("Liberation Sans"),
            Family::Name("Noto Sans"),
            Family::SansSerif,
        ];
        let id = database
            .query(&Query {
                families: &families,
                ..Query::default()
            })
            .ok_or(TextRendererError::MissingFont)?;
        let font = database
            .with_face_data(id, |data, collection_index| {
                Font::from_bytes(
                    data,
                    FontSettings {
                        collection_index,
                        ..FontSettings::default()
                    },
                )
            })
            .ok_or(TextRendererError::MissingFont)?
            .map_err(|_| TextRendererError::InvalidFont)?;
        Ok(Self {
            font,
            glyphs: RefCell::new(HashMap::new()),
        })
    }

    /// Measures the horizontal advance of text at a whole-pixel Em size.
    pub(crate) fn measure(&self, text: &str, pixels: u16) -> i32 {
        let pixels = f32::from(pixels.max(1));
        let mut width = 0.0_f32;
        let mut previous = None;
        for character in text.chars() {
            if let Some(previous) = previous {
                width += self
                    .font
                    .horizontal_kern(previous, character, pixels)
                    .unwrap_or(0.0);
            }
            width += self.font.metrics(character, pixels).advance_width;
            previous = Some(character);
        }
        width.ceil().clamp(0.0, i32::MAX as f32) as i32
    }

    /// Rasterizes clipped text into quantized horizontal coverage runs.
    pub(crate) fn runs(
        &self,
        text: &str,
        origin_x: i32,
        clip: Geometry,
        pixels: u16,
    ) -> Vec<TextRun> {
        let pixels = pixels.max(1);
        let pixels_f32 = f32::from(pixels);
        let line = self.font.horizontal_line_metrics(pixels_f32);
        let line_height = line.map_or(pixels_f32, |metrics| metrics.ascent - metrics.descent);
        let ascent = line.map_or(pixels_f32 * 0.8, |metrics| metrics.ascent);
        let baseline = f64::from(clip.y)
            + (f64::from(clip.height) - f64::from(line_height)).max(0.0) / 2.0
            + f64::from(ascent);
        let clip_right = i64::from(clip.x).saturating_add(i64::from(clip.width));
        let mut pen_x = f64::from(origin_x);
        let mut previous = None;
        let mut output = Vec::new();
        for character in text.chars() {
            if let Some(previous) = previous {
                pen_x += f64::from(
                    self.font
                        .horizontal_kern(previous, character, pixels_f32)
                        .unwrap_or(0.0),
                );
            }
            let glyph = self.glyph(character, pixels);
            let glyph_x = pen_x.round() as i32;
            let glyph_top = baseline.round() as i32
                - i32::try_from(glyph.metrics.height).unwrap_or(i32::MAX)
                - glyph.metrics.ymin;
            for run in &glyph.runs {
                let translated = Geometry::new(
                    glyph_x
                        .saturating_add(glyph.metrics.xmin)
                        .saturating_add(run.geometry.x),
                    glyph_top.saturating_add(run.geometry.y),
                    run.geometry.width,
                    run.geometry.height,
                );
                if let Some(geometry) = intersect(translated, clip) {
                    output.push(TextRun {
                        geometry,
                        coverage: run.coverage,
                    });
                }
            }
            pen_x += f64::from(glyph.metrics.advance_width);
            previous = Some(character);
            if pen_x.round() as i64 >= clip_right {
                break;
            }
        }
        output
    }

    fn glyph(&self, character: char, pixels: u16) -> GlyphRaster {
        let key = GlyphKey { character, pixels };
        if let Some(glyph) = self.glyphs.borrow().get(&key) {
            return glyph.clone();
        }
        let (metrics, bitmap) = self.font.rasterize(character, f32::from(pixels));
        let mut runs = Vec::new();
        for row in 0..metrics.height {
            let mut column = 0;
            while column < metrics.width {
                let coverage = quantized_coverage(bitmap[row * metrics.width + column]);
                if coverage == 0 {
                    column += 1;
                    continue;
                }
                let start = column;
                column += 1;
                while column < metrics.width
                    && quantized_coverage(bitmap[row * metrics.width + column]) == coverage
                {
                    column += 1;
                }
                runs.push(TextRun {
                    geometry: Geometry::new(
                        i32::try_from(start).unwrap_or(i32::MAX),
                        i32::try_from(row).unwrap_or(i32::MAX),
                        u32::try_from(column - start).unwrap_or(u32::MAX),
                        1,
                    ),
                    coverage,
                });
            }
        }
        let glyph = GlyphRaster { metrics, runs };
        let mut glyphs = self.glyphs.borrow_mut();
        if glyphs.len() >= MAX_CACHED_GLYPHS {
            glyphs.clear();
        }
        glyphs.insert(key, glyph.clone());
        glyph
    }
}

fn configured_family(configured: &str) -> Option<&str> {
    if configured.starts_with('-') {
        return configured
            .split('-')
            .nth(2)
            .filter(|family| !family.is_empty() && *family != "*");
    }
    configured
        .split([':', ','])
        .next()
        .map(str::trim)
        .filter(|family| !family.is_empty())
}

const fn quantized_coverage(coverage: u8) -> u8 {
    match coverage {
        0..=31 => 0,
        32..=95 => 64,
        96..=159 => 128,
        160..=223 => 192,
        224..=255 => 255,
    }
}

fn intersect(left: Geometry, right: Geometry) -> Option<Geometry> {
    let x = i64::from(left.x).max(i64::from(right.x));
    let y = i64::from(left.y).max(i64::from(right.y));
    let end_x = i64::from(left.x)
        .saturating_add(i64::from(left.width))
        .min(i64::from(right.x).saturating_add(i64::from(right.width)));
    let end_y = i64::from(left.y)
        .saturating_add(i64::from(left.height))
        .min(i64::from(right.y).saturating_add(i64::from(right.height)));
    (end_x > x && end_y > y).then(|| {
        Geometry::new(
            i32::try_from(x).unwrap_or(if x.is_negative() { i32::MIN } else { i32::MAX }),
            i32::try_from(y).unwrap_or(if y.is_negative() { i32::MIN } else { i32::MAX }),
            u32::try_from(end_x - x).unwrap_or(u32::MAX),
            u32::try_from(end_y - y).unwrap_or(u32::MAX),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xlfd_and_plain_font_names_select_a_family() {
        assert_eq!(
            configured_family("-*-helvetica-medium-r-normal--12-*-*-*-p-*-iso10646-1"),
            Some("helvetica")
        );
        assert_eq!(
            configured_family("DejaVu Sans:style=Book"),
            Some("DejaVu Sans")
        );
        assert_eq!(configured_family(""), None);
    }

    #[test]
    fn clipping_and_coverage_are_bounded() {
        assert_eq!(quantized_coverage(31), 0);
        assert_eq!(quantized_coverage(32), 64);
        assert_eq!(quantized_coverage(255), 255);
        assert_eq!(
            intersect(Geometry::new(-2, 4, 10, 5), Geometry::new(0, 0, 4, 8)),
            Some(Geometry::new(0, 4, 4, 4))
        );
        assert_eq!(
            intersect(Geometry::new(10, 10, 2, 2), Geometry::new(0, 0, 4, 4)),
            None
        );
    }
}
