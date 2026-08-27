//! Pixel conversion and encoding for `nobox-screenshot`.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use jpeg_encoder::{ColorType as JpegColorType, Encoder as JpegEncoder};
use std::io::Write;
use x11rb::protocol::xproto::{ImageOrder, Setup, Visualid};

/// Default JPEG quality, chosen for compact screenshots that retain readable UI text.
pub const DEFAULT_JPEG_QUALITY: u8 = 75;

/// Supported screenshot encodings.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum ImageFormat {
    /// Lossless PNG.
    Png,
    /// Lossy JPEG with adjustable quality.
    #[value(alias = "jpg")]
    Jpeg,
}

impl ImageFormat {
    /// Conventional filename suffix without a leading dot.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    /// MIME type used for clipboard transfer.
    #[must_use]
    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// One RGB capture and its location in root coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Capture {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// Root-coordinate X origin.
    pub x: i16,
    /// Root-coordinate Y origin.
    pub y: i16,
    /// Packed RGB8 pixels.
    pub rgb: Vec<u8>,
}

/// One ARGB cursor image and its root-coordinate hotspot position.
#[derive(Clone, Copy, Debug)]
pub struct CursorImage<'a> {
    /// Cursor hotspot X position in root coordinates.
    pub x: i16,
    /// Cursor hotspot Y position in root coordinates.
    pub y: i16,
    /// Hotspot X offset inside the cursor image.
    pub hotspot_x: u16,
    /// Hotspot Y offset inside the cursor image.
    pub hotspot_y: u16,
    /// Cursor image width.
    pub width: u16,
    /// Cursor image height.
    pub height: u16,
    /// Packed premultiplied ARGB pixels supplied by XFixes.
    pub argb: &'a [u32],
}

/// Converts one X11 `GetImage` reply into packed RGB8 pixels.
pub fn x11_to_rgb(
    setup: &Setup,
    screen_index: usize,
    width: u16,
    height: u16,
    depth: u8,
    data: &[u8],
) -> Result<Vec<u8>> {
    let format = setup
        .pixmap_formats
        .iter()
        .find(|format| format.depth == depth)
        .context("the X server did not advertise the captured pixmap format")?;
    if format.bits_per_pixel != 24 && format.bits_per_pixel != 32 {
        bail!(
            "cannot encode the server's {}-bit pixels",
            format.bits_per_pixel
        );
    }
    let screen = setup
        .roots
        .get(screen_index)
        .context("the selected X11 screen disappeared")?;
    let visual = find_visual(setup, screen_index, screen.root_visual);
    let (red_mask, green_mask, blue_mask) = visual
        .map_or((0x00ff_0000, 0x0000_ff00, 0x0000_00ff), |visual| {
            (visual.red_mask, visual.green_mask, visual.blue_mask)
        });
    let bytes_per_pixel = usize::from(format.bits_per_pixel / 8);
    let unpadded = usize::from(width) * bytes_per_pixel;
    let pad_bytes = usize::from(format.scanline_pad) / 8;
    let stride = unpadded.div_ceil(pad_bytes) * pad_bytes;
    let required = stride
        .checked_mul(usize::from(height))
        .context("captured image size overflowed")?;
    if data.len() < required {
        bail!("the X server returned a short image");
    }

    let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
    for row in 0..usize::from(height) {
        for column in 0..usize::from(width) {
            let offset = row * stride + column * bytes_per_pixel;
            let chunk = &data[offset..offset + bytes_per_pixel];
            let pixel = match setup.image_byte_order {
                ImageOrder::LSB_FIRST => chunk
                    .iter()
                    .enumerate()
                    .fold(0_u32, |pixel, (index, byte)| {
                        pixel | (u32::from(*byte) << (8 * index))
                    }),
                ImageOrder::MSB_FIRST => chunk
                    .iter()
                    .fold(0_u32, |pixel, byte| (pixel << 8) | u32::from(*byte)),
                _ => bail!("the X server reported an unknown image byte order"),
            };
            rgb.extend_from_slice(&[
                channel(pixel, red_mask),
                channel(pixel, green_mask),
                channel(pixel, blue_mask),
            ]);
        }
    }
    Ok(rgb)
}

fn find_visual(
    setup: &Setup,
    screen_index: usize,
    visual_id: Visualid,
) -> Option<&x11rb::protocol::xproto::Visualtype> {
    setup.roots[screen_index]
        .allowed_depths
        .iter()
        .flat_map(|depth| depth.visuals.iter())
        .find(|visual| visual.visual_id == visual_id)
}

const fn channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let width = mask.count_ones();
    let value = (pixel & mask) >> shift;
    if width >= 8 {
        (value >> (width - 8)) as u8
    } else {
        ((value << (8 - width)) | (value >> width.saturating_sub(8 - width))) as u8
    }
}

/// Alpha-blends the XFixes cursor into a capture when it overlaps.
pub fn blend_cursor(capture: &mut Capture, cursor: CursorImage<'_>) {
    let left = i32::from(cursor.x) - i32::from(cursor.hotspot_x) - i32::from(capture.x);
    let top = i32::from(cursor.y) - i32::from(cursor.hotspot_y) - i32::from(capture.y);
    for source_y in 0..usize::from(cursor.height) {
        for source_x in 0..usize::from(cursor.width) {
            let destination_x = left + i32::try_from(source_x).unwrap_or(i32::MAX);
            let destination_y = top + i32::try_from(source_y).unwrap_or(i32::MAX);
            if destination_x < 0
                || destination_y < 0
                || destination_x >= i32::from(capture.width)
                || destination_y >= i32::from(capture.height)
            {
                continue;
            }
            let Some(&pixel) = cursor
                .argb
                .get(source_y * usize::from(cursor.width) + source_x)
            else {
                continue;
            };
            let alpha = (pixel >> 24) & 0xff;
            if alpha == 0 {
                continue;
            }
            let destination = (usize::try_from(destination_y).unwrap_or(0)
                * usize::from(capture.width)
                + usize::try_from(destination_x).unwrap_or(0))
                * 3;
            for (channel_index, shift) in [16_u32, 8, 0].into_iter().enumerate() {
                let foreground = (pixel >> shift) & 0xff;
                let background = u32::from(capture.rgb[destination + channel_index]);
                capture.rgb[destination + channel_index] =
                    (foreground + (background * (255 - alpha) + 127) / 255).min(255) as u8;
            }
        }
    }
}

/// Encodes packed RGB8 pixels into the selected image format.
pub fn encode<W: Write>(
    mut output: W,
    capture: &Capture,
    format: ImageFormat,
    quality: u8,
) -> Result<()> {
    let expected = usize::from(capture.width) * usize::from(capture.height) * 3;
    if capture.rgb.len() != expected {
        bail!("capture contains an invalid RGB buffer length");
    }
    match format {
        ImageFormat::Png => {
            let mut encoder = png::Encoder::new(
                &mut output,
                u32::from(capture.width),
                u32::from(capture.height),
            );
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .and_then(|mut writer| writer.write_image_data(&capture.rgb))
                .context("could not encode PNG")?;
        }
        ImageFormat::Jpeg => {
            if !(1..=100).contains(&quality) {
                bail!("JPEG quality must be between 1 and 100");
            }
            JpegEncoder::new(&mut output, quality)
                .encode(
                    &capture.rgb,
                    capture.width,
                    capture.height,
                    JpegColorType::Rgb,
                )
                .context("could not encode JPEG")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detailed_fixture() -> Capture {
        let (width, height) = (320_u16, 180_u16);
        let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
        for y in 0..height {
            for x in 0..width {
                let checker = if (x / 5 + y / 5) % 2 == 0 { 24 } else { 232 };
                rgb.extend_from_slice(&[
                    (u32::from(x) * 255 / u32::from(width - 1)) as u8,
                    (u32::from(y) * 255 / u32::from(height - 1)) as u8,
                    checker,
                ]);
            }
        }
        Capture {
            width,
            height,
            x: 0,
            y: 0,
            rgb,
        }
    }

    #[test]
    fn jpeg_quality_materially_changes_transport_size() {
        let fixture = detailed_fixture();
        let mut quality_60 = Vec::new();
        let mut quality_80 = Vec::new();
        let mut quality_100 = Vec::new();
        encode(&mut quality_60, &fixture, ImageFormat::Jpeg, 60).unwrap();
        encode(&mut quality_80, &fixture, ImageFormat::Jpeg, 80).unwrap();
        encode(&mut quality_100, &fixture, ImageFormat::Jpeg, 100).unwrap();
        assert!(quality_60.starts_with(&[0xff, 0xd8]));
        assert!(quality_60.len() < quality_80.len());
        assert!(quality_80.len() < quality_100.len());
        assert!(quality_80.len() * 2 < quality_100.len());
    }

    #[test]
    fn cursor_blending_clips_and_honors_alpha() {
        let mut capture = Capture {
            width: 2,
            height: 1,
            x: 10,
            y: 20,
            rgb: vec![0, 0, 0, 20, 40, 60],
        };
        blend_cursor(
            &mut capture,
            CursorImage {
                x: 10,
                y: 20,
                hotspot_x: 0,
                hotspot_y: 0,
                width: 2,
                height: 1,
                argb: &[0xffff_0000, 0x8000_8000],
            },
        );
        assert_eq!(&capture.rgb[..3], &[255, 0, 0]);
        assert_eq!(&capture.rgb[3..], &[10, 148, 30]);
    }
}
