//! Wayland-local translation for Agent Seat keyboard input.
//!
//! The wire names keys and modifiers without exposing Linux or XKB codes. This
//! module owns that translation; display-neutral policy never sees either.

use nobox_agent_wire::Modifier;
use smithay::input::keyboard::{Keycode, Keysym, xkb};

const MAX_PACED_TEXT_SCALARS: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeyStroke {
    pub(crate) key: Keycode,
    pub(crate) held: Vec<Keycode>,
}

pub(crate) enum TextPlan {
    Strokes(Vec<KeyStroke>),
    Exact(String),
}

pub(crate) struct AgentKeyboard {
    keymap: xkb::Keymap,
}

impl AgentKeyboard {
    pub(crate) fn compile_default() -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "",
            "",
            "",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )?;
        Some(Self { keymap })
    }

    pub(crate) fn named_key(
        &self,
        name: &str,
        modifiers: &[Modifier],
    ) -> Result<KeyStroke, String> {
        let symbol = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
        if symbol == xkb::keysyms::KEY_NoSymbol.into() {
            return Err(format!("no key named {name} on this layout"));
        }
        let key = self
            .key_for_symbol(symbol)
            .ok_or_else(|| format!("no key named {name} on this layout"))?;
        let mut held = modifiers
            .iter()
            .map(|modifier| {
                self.modifier_key(*modifier)
                    .ok_or_else(|| format!("no {modifier:?} key on this layout"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        held.sort_unstable_by_key(|key| key.raw());
        held.dedup();
        Ok(KeyStroke { key, held })
    }

    /// Resolves the whole string before returning any events to inject.
    pub(crate) fn text(&self, text: &str) -> Result<TextPlan, String> {
        for (index, character) in text.chars().enumerate() {
            if character.is_control() && !matches!(character, '\n' | '\t') {
                return Err(format!(
                    "character {} (U+{:04X}) is not printable; exact text also accepts newline and tab",
                    index + 1,
                    u32::from(character)
                ));
            }
        }
        let strokes = text
            .chars()
            .enumerate()
            .map(|(index, character)| {
                let symbol = match character {
                    '\n' => xkb::keysyms::KEY_Return.into(),
                    '\t' => xkb::keysyms::KEY_Tab.into(),
                    character => Keysym::from_char(character),
                };
                self.stroke_for_symbol(symbol).ok_or_else(|| {
                    format!(
                        "character {} (U+{:04X}) is unavailable on this keyboard layout",
                        index + 1,
                        u32::from(character)
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>();
        Ok(match strokes {
            Ok(strokes) if strokes.len() <= MAX_PACED_TEXT_SCALARS => TextPlan::Strokes(strokes),
            Err(_) => TextPlan::Exact(text.to_owned()),
            Ok(_) => TextPlan::Exact(text.to_owned()),
        })
    }

    fn key_for_symbol(&self, symbol: Keysym) -> Option<Keycode> {
        let mut found = None;
        self.keymap.key_for_each(|keymap, key| {
            if found.is_some() {
                return;
            }
            for layout in 0..keymap.num_layouts_for_key(key) {
                for level in 0..keymap.num_levels_for_key(key, layout) {
                    if keymap
                        .key_get_syms_by_level(key, layout, level)
                        .contains(&symbol)
                    {
                        found = Some(key);
                        return;
                    }
                }
            }
        });
        found
    }

    fn stroke_for_symbol(&self, symbol: Keysym) -> Option<KeyStroke> {
        let shift_index = self.keymap.mod_get_index(xkb::MOD_NAME_SHIFT);
        let level3_index = self.keymap.mod_get_index(xkb::MOD_NAME_ISO_LEVEL3_SHIFT);
        let shift_mask = modifier_mask(shift_index);
        let level3_mask = modifier_mask(level3_index);
        let allowed = shift_mask | level3_mask;
        let shift = self.modifier_key(Modifier::Shift);
        let level3 = self.modifier_key(Modifier::AltGr);
        let mut found = None;
        self.keymap.key_for_each(|keymap, key| {
            if found.is_some() {
                return;
            }
            for layout in 0..keymap.num_layouts_for_key(key) {
                for level in 0..keymap.num_levels_for_key(key, layout) {
                    if !keymap
                        .key_get_syms_by_level(key, layout, level)
                        .contains(&symbol)
                    {
                        continue;
                    }
                    let mut masks = [0_u32; 8];
                    let count = keymap
                        .key_get_mods_for_level(key, layout, level, &mut masks)
                        .min(masks.len());
                    let mask = masks[..count]
                        .iter()
                        .copied()
                        .filter(|mask| mask & !allowed == 0)
                        .filter(|mask| mask & shift_mask == 0 || shift.is_some())
                        .filter(|mask| mask & level3_mask == 0 || level3.is_some())
                        .min_by_key(|mask| mask.count_ones());
                    let Some(mask) = mask else {
                        continue;
                    };
                    let mut held = Vec::new();
                    if mask & shift_mask != 0 {
                        held.push(shift.expect("the mask was filtered by availability"));
                    }
                    if mask & level3_mask != 0 {
                        held.push(level3.expect("the mask was filtered by availability"));
                    }
                    found = Some(KeyStroke { key, held });
                    return;
                }
            }
        });
        found
    }

    fn modifier_key(&self, modifier: Modifier) -> Option<Keycode> {
        let name = match modifier {
            Modifier::Shift => "Shift_L",
            Modifier::Control => "Control_L",
            Modifier::Alt => "Alt_L",
            Modifier::Super => "Super_L",
            Modifier::AltGr => "ISO_Level3_Shift",
        };
        let symbol = xkb::keysym_from_name(name, xkb::KEYSYM_NO_FLAGS);
        self.key_for_symbol(symbol)
    }
}

fn modifier_mask(index: xkb::ModIndex) -> u32 {
    if index != xkb::MOD_INVALID && index < u32::BITS {
        1_u32 << index
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_keymap_resolves_named_keys_and_modifiers() {
        let keyboard = AgentKeyboard::compile_default().expect("default keymap");
        let stroke = keyboard
            .named_key("Return", &[Modifier::Control, Modifier::Shift])
            .expect("named key");
        assert_eq!(stroke.held.len(), 2);
    }

    #[test]
    fn text_is_fully_validated_before_injection() {
        let keyboard = AgentKeyboard::compile_default().expect("default keymap");
        assert!(matches!(keyboard.text("aA\n\t"), Ok(TextPlan::Strokes(_))));
        assert!(keyboard.text("ok\u{7}suffix").is_err());
    }
}
