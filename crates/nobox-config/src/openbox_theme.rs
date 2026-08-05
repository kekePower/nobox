use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::{RgbColor, ThemeConfig, TitleAlignment};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_SOURCE_LINES: usize = 10_000;
const MAX_PROPERTY_BYTES: usize = 512;

/// Result of translating representable Openbox `themerc` properties.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenboxThemeImport {
    /// Valid nobox theme produced from the source.
    pub theme: ThemeConfig,
    /// Number of Openbox properties that contributed to the result.
    pub mapped_properties: usize,
    /// Explicit compatibility notes for lossy or ignored source features.
    pub warnings: Vec<String>,
}

impl OpenboxThemeImport {
    /// Parses an Openbox 3 `themerc` and maps the subset nobox represents.
    ///
    /// Property names are case-insensitive and later duplicate definitions win,
    /// matching the practical behavior of existing Openbox themes.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is empty or exceeds importer bounds.
    pub fn parse(source: &str) -> Result<Self, OpenboxThemeImportError> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(OpenboxThemeImportError::SourceTooLarge(source.len()));
        }
        let line_count = source.lines().count();
        if line_count > MAX_SOURCE_LINES {
            return Err(OpenboxThemeImportError::TooManyLines(line_count));
        }

        let mut properties = BTreeMap::new();
        let mut malformed = 0_usize;
        let mut duplicates = 0_usize;
        for line in source.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('!') || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                malformed = malformed.saturating_add(1);
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().to_owned();
            if key.is_empty()
                || value.is_empty()
                || key.len() > MAX_PROPERTY_BYTES
                || value.len() > MAX_PROPERTY_BYTES
            {
                malformed = malformed.saturating_add(1);
                continue;
            }
            if properties.insert(key, value).is_some() {
                duplicates = duplicates.saturating_add(1);
            }
        }
        if properties.is_empty() {
            return Err(OpenboxThemeImportError::NoProperties);
        }

        let mut theme = ThemeConfig::default();
        let mut used = BTreeSet::new();
        let mut warnings = Vec::new();

        if let Some((key, value)) = property(&properties, &["border.width"], &mut used) {
            match value.parse::<u32>() {
                Ok(width) if width <= 64 => theme.border_width = width,
                _ => warnings.push(format!("ignored invalid {key} value {value:?}")),
            }
        }
        if let Some((key, value)) = property(&properties, &["padding.width"], &mut used) {
            match value.parse::<u32>() {
                Ok(padding) if padding <= 64 => theme.title_padding = padding,
                _ => warnings.push(format!("ignored invalid {key} value {value:?}")),
            }
        }
        if let Some((key, value)) = property(
            &properties,
            &[
                "window.label.text.justify",
                "window.*.label.text.justify",
                "*.text.justify",
                "*.justify",
            ],
            &mut used,
        ) {
            theme.title_alignment = match value.to_ascii_lowercase().as_str() {
                "left" => TitleAlignment::Left,
                "center" => TitleAlignment::Center,
                "right" => TitleAlignment::Right,
                _ => {
                    warnings.push(format!("ignored invalid {key} value {value:?}"));
                    theme.title_alignment
                }
            };
        }

        map_color(
            &properties,
            &[
                "window.active.border.color",
                "window.*.border.color",
                "border.color",
            ],
            &mut used,
            &mut warnings,
            &mut theme.active_border,
        );
        map_color(
            &properties,
            &[
                "window.inactive.border.color",
                "window.*.border.color",
                "border.color",
            ],
            &mut used,
            &mut warnings,
            &mut theme.inactive_border,
        );
        map_color(
            &properties,
            &[
                "window.active.title.bg.color",
                "window.*.title.bg.color",
                "*.title.bg.color",
            ],
            &mut used,
            &mut warnings,
            &mut theme.active_titlebar,
        );
        map_color(
            &properties,
            &[
                "window.inactive.title.bg.color",
                "window.*.title.bg.color",
                "*.title.bg.color",
            ],
            &mut used,
            &mut warnings,
            &mut theme.inactive_titlebar,
        );
        map_color(
            &properties,
            &["window.active.label.text.color"],
            &mut used,
            &mut warnings,
            &mut theme.title_text,
        );

        let mut button_background = None;
        map_optional_color(
            &properties,
            &[
                "window.active.button.unpressed.bg.color",
                "window.active.button.*.bg.color",
            ],
            &mut used,
            &mut warnings,
            &mut button_background,
        );
        if let Some(color) = button_background {
            theme.minimize_button = color;
            theme.maximize_button = color;
            theme.close_button = color;
        }
        map_color(
            &properties,
            &[
                "window.active.button.unpressed.image.color",
                "window.active.button.*.image.color",
            ],
            &mut used,
            &mut warnings,
            &mut theme.button_glyph,
        );

        if properties.keys().any(|key| {
            key.starts_with("window.active.title.bg.colorto")
                || key.starts_with("window.inactive.title.bg.colorto")
                || key.ends_with(".color.splitto")
        }) {
            warnings.push("collapsed Openbox title gradients to their first color stop".to_owned());
        }
        if let Some((key, inactive)) = property(
            &properties,
            &["window.inactive.label.text.color"],
            &mut used,
        ) && parse_openbox_color(inactive).is_some_and(|color| color != theme.title_text)
        {
            warnings.push(format!(
                "{key} differs from the active label color; nobox currently uses one title-text color"
            ));
        }
        if malformed > 0 {
            warnings.push(format!("ignored {malformed} malformed property line(s)"));
        }
        if duplicates > 0 {
            warnings.push(format!(
                "resolved {duplicates} duplicate property definition(s) using the last value"
            ));
        }
        let ignored = properties.len().saturating_sub(used.len());
        if ignored > 0 {
            warnings.push(format!(
                "ignored {ignored} Openbox property/properties without a nobox equivalent"
            ));
        }
        warnings.push(
            "kept nobox defaults for fonts, titlebar height, and urgent colors; Openbox stores fonts outside themerc"
                .to_owned(),
        );

        Ok(Self {
            theme,
            mapped_properties: used.len(),
            warnings,
        })
    }

    /// Renders a complete, independently valid nobox `[theme]` TOML document.
    #[must_use]
    pub fn to_toml(&self) -> String {
        let theme = &self.theme;
        let alignment = match theme.title_alignment {
            TitleAlignment::Left => "left",
            TitleAlignment::Center => "center",
            TitleAlignment::Right => "right",
        };
        format!(
            "# Generated by `nobox import-openbox-theme`.\n\
             # Review compatibility notes printed during import.\n\n\
             [theme]\n\
             border_width = {}\n\
             titlebar_height = {}\n\
             font = \"{}\"\n\
             title_alignment = \"{}\"\n\
             title_padding = {}\n\
             active_border = \"{}\"\n\
             inactive_border = \"{}\"\n\
             urgent_border = \"{}\"\n\
             active_titlebar = \"{}\"\n\
             inactive_titlebar = \"{}\"\n\
             urgent_titlebar = \"{}\"\n\
             title_text = \"{}\"\n\
             minimize_button = \"{}\"\n\
             maximize_button = \"{}\"\n\
             close_button = \"{}\"\n\
             button_glyph = \"{}\"\n",
            theme.border_width,
            theme.titlebar_height,
            escape_toml_basic_string(&theme.font),
            alignment,
            theme.title_padding,
            theme.active_border,
            theme.inactive_border,
            theme.urgent_border,
            theme.active_titlebar,
            theme.inactive_titlebar,
            theme.urgent_titlebar,
            theme.title_text,
            theme.minimize_button,
            theme.maximize_button,
            theme.close_button,
            theme.button_glyph,
        )
    }
}

/// Bounded Openbox theme import failures.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum OpenboxThemeImportError {
    /// Protect startup tools from unexpectedly large input.
    #[error("Openbox theme is {0} bytes; maximum is 1048576")]
    SourceTooLarge(usize),
    /// Bound line-oriented parsing work.
    #[error("Openbox theme has {0} lines; maximum is 10000")]
    TooManyLines(usize),
    /// An empty or comment-only source cannot be imported.
    #[error("Openbox theme contains no properties")]
    NoProperties,
}

fn property<'a>(
    properties: &'a BTreeMap<String, String>,
    candidates: &[&str],
    used: &mut BTreeSet<String>,
) -> Option<(&'a str, &'a str)> {
    candidates.iter().find_map(|candidate| {
        properties.get_key_value(*candidate).map(|(key, value)| {
            used.insert(key.clone());
            (key.as_str(), value.as_str())
        })
    })
}

fn map_color(
    properties: &BTreeMap<String, String>,
    candidates: &[&str],
    used: &mut BTreeSet<String>,
    warnings: &mut Vec<String>,
    target: &mut RgbColor,
) {
    let mut mapped = None;
    map_optional_color(properties, candidates, used, warnings, &mut mapped);
    if let Some(color) = mapped {
        *target = color;
    }
}

fn map_optional_color(
    properties: &BTreeMap<String, String>,
    candidates: &[&str],
    used: &mut BTreeSet<String>,
    warnings: &mut Vec<String>,
    target: &mut Option<RgbColor>,
) {
    let Some((key, value)) = property(properties, candidates, used) else {
        return;
    };
    if let Some(color) = parse_openbox_color(value) {
        *target = Some(color);
    } else {
        warnings.push(format!("ignored unsupported {key} color {value:?}"));
    }
}

fn parse_openbox_color(value: &str) -> Option<RgbColor> {
    let value = value.trim().to_ascii_lowercase();
    if let Ok(color) = value.parse() {
        return Some(color);
    }
    if let Some(hex) = value.strip_prefix('#')
        && hex.len() == 3
    {
        let mut channels = hex.chars().map(|channel| channel.to_digit(16));
        let red = u8::try_from(channels.next()??).ok()? * 17;
        let green = u8::try_from(channels.next()??).ok()? * 17;
        let blue = u8::try_from(channels.next()??).ok()? * 17;
        return Some(RgbColor::new(red, green, blue));
    }
    if let Some(rgb) = value.strip_prefix("rgb:") {
        let channels = rgb
            .split('/')
            .map(parse_x11_hex_channel)
            .collect::<Option<Vec<_>>>()?;
        return (channels.len() == 3).then(|| RgbColor::new(channels[0], channels[1], channels[2]));
    }
    match value.as_str() {
        "black" => Some(RgbColor::new(0, 0, 0)),
        "white" => Some(RgbColor::new(255, 255, 255)),
        "red" => Some(RgbColor::new(255, 0, 0)),
        "green" => Some(RgbColor::new(0, 255, 0)),
        "blue" => Some(RgbColor::new(0, 0, 255)),
        "yellow" => Some(RgbColor::new(255, 255, 0)),
        "cyan" => Some(RgbColor::new(0, 255, 255)),
        "magenta" => Some(RgbColor::new(255, 0, 255)),
        "gray" | "grey" => Some(RgbColor::new(190, 190, 190)),
        _ => parse_gray_percentage(&value),
    }
}

fn parse_x11_hex_channel(channel: &str) -> Option<u8> {
    if channel.is_empty() || channel.len() > 4 {
        return None;
    }
    let value = u32::from_str_radix(channel, 16).ok()?;
    let maximum = (1_u32 << (channel.len() * 4)).saturating_sub(1);
    u8::try_from(value.saturating_mul(255) / maximum).ok()
}

fn parse_gray_percentage(value: &str) -> Option<RgbColor> {
    let percentage = value
        .strip_prefix("gray")
        .or_else(|| value.strip_prefix("grey"))?
        .parse::<u32>()
        .ok()?;
    if percentage > 100 {
        return None;
    }
    let channel = u8::try_from(percentage.saturating_mul(255) / 100).ok()?;
    Some(RgbColor::new(channel, channel, channel))
}

fn escape_toml_basic_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;

    #[test]
    fn representative_openbox_theme_maps_to_valid_nobox_toml() {
        let source = "\
            border.width: 1\n\
            padding.width: 3\n\
            *.justify: center\n\
            window.*.border.color: #585a5d\n\
            window.active.title.bg: raised gradient vertical\n\
            window.active.title.bg.color: #8CB0DC\n\
            window.active.title.bg.colorTo: #7AA1D1\n\
            window.inactive.title.bg.color: #E3E2E0\n\
            window.active.label.text.color: white\n\
            window.inactive.label.text.color: #70747d\n\
            window.active.button.*.bg.color: rgb:92/b4/df\n\
            window.active.button.*.image.color: #444\n\
            menu.items.bg.color: grey85\n";
        let imported = OpenboxThemeImport::parse(source).expect("valid Openbox theme");
        assert_eq!(imported.theme.border_width, 1);
        assert_eq!(imported.theme.title_padding, 3);
        assert_eq!(imported.theme.title_alignment, TitleAlignment::Center);
        assert_eq!(imported.theme.active_border, RgbColor(0x58_5a_5d));
        assert_eq!(imported.theme.inactive_border, RgbColor(0x58_5a_5d));
        assert_eq!(imported.theme.active_titlebar, RgbColor(0x8c_b0_dc));
        assert_eq!(imported.theme.inactive_titlebar, RgbColor(0xe3_e2_e0));
        assert_eq!(imported.theme.title_text, RgbColor(0xff_ff_ff));
        assert_eq!(imported.theme.minimize_button, RgbColor(0x92_b4_df));
        assert_eq!(imported.theme.maximize_button, RgbColor(0x92_b4_df));
        assert_eq!(imported.theme.close_button, RgbColor(0x92_b4_df));
        assert_eq!(imported.theme.button_glyph, RgbColor(0x44_44_44));
        assert!(
            imported
                .warnings
                .iter()
                .any(|warning| warning.contains("gradient"))
        );
        assert!(
            imported
                .warnings
                .iter()
                .any(|warning| warning.contains("title-text"))
        );
        Config::parse(&imported.to_toml()).expect("import output is valid nobox TOML");
    }

    #[test]
    fn legacy_color_syntax_and_hostile_input_are_bounded() {
        assert_eq!(parse_openbox_color("#711"), Some(RgbColor(0x77_11_11)));
        assert_eq!(
            parse_openbox_color("rgb:80/84/88"),
            Some(RgbColor(0x80_84_88))
        );
        assert_eq!(parse_openbox_color("grey60"), Some(RgbColor(0x99_99_99)));
        assert_eq!(parse_openbox_color("not-a-color"), None);
        assert_eq!(
            OpenboxThemeImport::parse("! comments only"),
            Err(OpenboxThemeImportError::NoProperties)
        );
        assert!(matches!(
            OpenboxThemeImport::parse(&"x".repeat(MAX_SOURCE_BYTES + 1)),
            Err(OpenboxThemeImportError::SourceTooLarge(_))
        ));
    }
}
