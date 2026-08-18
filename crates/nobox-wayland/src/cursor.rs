use std::{collections::HashMap, env, fs::File, io::Read as _};

use smithay::{
    backend::{allocator::Fourcc, renderer::element::memory::MemoryRenderBuffer},
    input::pointer::CursorIcon,
    utils::{Logical, Point, Transform},
};
use tracing::warn;
use xcursor::{CursorTheme, parser::parse_xcursor};

const DEFAULT_CURSOR_SIZE: u32 = 24;
const MAX_CURSOR_FILE_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct ThemedCursorImage {
    pub(crate) buffer: MemoryRenderBuffer,
    pub(crate) hotspot: Point<f64, Logical>,
}

pub(crate) struct CursorThemeManager {
    theme: CursorTheme,
    logical_size: u32,
    images: HashMap<(CursorIcon, i32), Option<ThemedCursorImage>>,
}

impl CursorThemeManager {
    pub(crate) fn load() -> Self {
        let name = env::var("XCURSOR_THEME")
            .ok()
            .filter(|name| valid_theme_name(name))
            .unwrap_or_else(|| "default".to_owned());
        let logical_size = env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|size| size.parse::<u32>().ok())
            .unwrap_or(DEFAULT_CURSOR_SIZE)
            .clamp(8, 256);
        Self {
            theme: CursorTheme::load(&name),
            logical_size,
            images: HashMap::new(),
        }
    }

    pub(crate) fn image(
        &mut self,
        icon: CursorIcon,
        output_scale: f64,
    ) -> Option<ThemedCursorImage> {
        let buffer_scale = output_scale.ceil().clamp(1.0, 8.0) as i32;
        let key = (icon, buffer_scale);
        if !self.images.contains_key(&key) {
            let image = self.load_image(icon, buffer_scale);
            self.images.insert(key, image);
        }
        self.images.get(&key).cloned().flatten()
    }

    fn load_image(&self, icon: CursorIcon, buffer_scale: i32) -> Option<ThemedCursorImage> {
        let path = cursor_names(icon)
            .into_iter()
            .find_map(|name| self.theme.load_icon(name));
        let Some(path) = path else {
            warn!(
                cursor = icon.name(),
                "cursor theme has no usable named cursor; using fallback"
            );
            return None;
        };
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) => {
                warn!(%error, cursor = icon.name(), path = %path.display(), "could not open themed cursor; using fallback");
                return None;
            }
        };
        let mut bytes = Vec::new();
        if let Err(error) = file
            .by_ref()
            .take(MAX_CURSOR_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
        {
            warn!(%error, cursor = icon.name(), path = %path.display(), "could not read themed cursor; using fallback");
            return None;
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CURSOR_FILE_BYTES {
            warn!(cursor = icon.name(), path = %path.display(), "themed cursor exceeds the bounded file size; using fallback");
            return None;
        }
        let Some(images) = parse_xcursor(&bytes) else {
            warn!(cursor = icon.name(), path = %path.display(), "could not parse themed cursor; using fallback");
            return None;
        };
        let requested_size = self
            .logical_size
            .saturating_mul(u32::try_from(buffer_scale).unwrap_or(1));
        let image = images
            .into_iter()
            .min_by_key(|image| image.size.abs_diff(requested_size))?;
        let width = i32::try_from(image.width).ok()?;
        let height = i32::try_from(image.height).ok()?;
        let scale = f64::from(buffer_scale);
        Some(ThemedCursorImage {
            buffer: MemoryRenderBuffer::from_slice(
                &image.pixels_rgba,
                Fourcc::Argb8888,
                (width, height),
                buffer_scale,
                Transform::Normal,
                None,
            ),
            hotspot: (f64::from(image.xhot) / scale, f64::from(image.yhot) / scale).into(),
        })
    }
}

fn valid_theme_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\', '\0'])
}

fn cursor_names(icon: CursorIcon) -> Vec<&'static str> {
    let mut names = vec![icon.name()];
    names.extend(match icon {
        CursorIcon::Default => &["left_ptr", "arrow"][..],
        CursorIcon::Pointer => &["hand2", "hand1"][..],
        CursorIcon::Text => &["xterm"][..],
        CursorIcon::Wait => &["watch"][..],
        CursorIcon::Progress => &["left_ptr_watch", "half-busy"][..],
        CursorIcon::EResize => &["right_side"][..],
        CursorIcon::NResize => &["top_side"][..],
        CursorIcon::NeResize => &["top_right_corner"][..],
        CursorIcon::NwResize => &["top_left_corner"][..],
        CursorIcon::SResize => &["bottom_side"][..],
        CursorIcon::SeResize => &["bottom_right_corner"][..],
        CursorIcon::SwResize => &["bottom_left_corner"][..],
        CursorIcon::WResize => &["left_side"][..],
        CursorIcon::EwResize | CursorIcon::ColResize => &["sb_h_double_arrow"][..],
        CursorIcon::NsResize | CursorIcon::RowResize => &["sb_v_double_arrow"][..],
        CursorIcon::NeswResize => &["fd_double_arrow"][..],
        CursorIcon::NwseResize => &["bd_double_arrow"][..],
        CursorIcon::Move | CursorIcon::AllResize => &["fleur"][..],
        _ => &[],
    });
    if icon != CursorIcon::Default {
        names.extend(["default", "left_ptr"]);
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_names_are_single_bounded_components() {
        assert!(valid_theme_name("wonderland"));
        assert!(valid_theme_name("Breeze Light"));
        assert!(!valid_theme_name(""));
        assert!(!valid_theme_name("../theme"));
        assert!(!valid_theme_name(&"x".repeat(129)));
    }

    #[test]
    fn cursor_names_prefer_protocol_name_then_legacy_aliases() {
        assert_eq!(
            cursor_names(CursorIcon::Default),
            ["default", "left_ptr", "arrow"]
        );
        assert_eq!(
            cursor_names(CursorIcon::Text),
            ["text", "xterm", "default", "left_ptr"]
        );
    }
}
