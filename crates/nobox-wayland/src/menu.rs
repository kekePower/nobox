use nobox_config::{Action, MenuEntry};
use nobox_core::ClientId;
use nobox_desktop::DesktopApplication;

#[derive(Clone, Debug)]
pub(crate) struct RuntimeMenu {
    pub(crate) title: String,
    pub(crate) entries: Vec<RuntimeMenuEntry>,
}

#[derive(Clone, Debug)]
pub(crate) enum RuntimeMenuEntry {
    Item {
        label: String,
        accelerator: Option<char>,
        action: RuntimeMenuAction,
        target: Option<ClientId>,
    },
    Submenu {
        label: String,
        accelerator: Option<char>,
        menu: RuntimeSubmenu,
    },
    Separator {
        label: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub(crate) enum RuntimeSubmenu {
    Named(String),
    Inline(Box<RuntimeMenu>),
}

#[derive(Clone, Debug)]
pub(crate) enum RuntimeMenuAction {
    Configured(Vec<Action>),
    ActivateClient(ClientId),
    Dismiss,
    Exit,
    SessionLogout,
    Execute { command: String, activation: bool },
    LaunchApplication(DesktopApplication),
}

pub(crate) fn paginate_runtime_menu(mut menu: RuntimeMenu, rows: usize) -> RuntimeMenu {
    for entry in &mut menu.entries {
        if let RuntimeMenuEntry::Submenu {
            menu: RuntimeSubmenu::Inline(submenu),
            ..
        } = entry
        {
            **submenu = paginate_runtime_menu((**submenu).clone(), rows);
        }
    }
    if rows < 2 || menu.entries.len() <= rows {
        return menu;
    }

    let page_entries = rows - 1;
    let mut remaining = std::mem::take(&mut menu.entries);
    let mut pages = Vec::new();
    while remaining.len() > page_entries {
        let rest = remaining.split_off(page_entries);
        pages.push(remaining);
        remaining = rest;
    }
    pages.push(remaining);

    let mut pages = pages.into_iter();
    let mut first = pages.next().unwrap_or_default();
    let mut continuation = None;
    for mut entries in pages.rev() {
        if let Some(next) = continuation {
            entries.push(submenu_entry(
                "_More...",
                RuntimeSubmenu::Inline(Box::new(next)),
            ));
        }
        continuation = Some(RuntimeMenu {
            title: "More...".to_owned(),
            entries,
        });
    }
    if let Some(continuation) = continuation {
        first.push(submenu_entry(
            "_More...",
            RuntimeSubmenu::Inline(Box::new(continuation)),
        ));
    }
    menu.entries = first;
    menu
}

#[derive(Clone, Debug)]
pub(crate) struct MenuLevel {
    pub(crate) menu: RuntimeMenu,
    pub(crate) selected: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct MenuSession {
    pub(crate) levels: Vec<MenuLevel>,
    pub(crate) target: Option<ClientId>,
    pub(crate) anchor_x: i32,
    pub(crate) anchor_y: i32,
    pub(crate) centered: bool,
}

impl MenuSession {
    pub(crate) fn new(
        menu: RuntimeMenu,
        target: Option<ClientId>,
        anchor_x: i32,
        anchor_y: i32,
        centered: bool,
    ) -> Option<Self> {
        let selected = first_selectable(&menu.entries)?;
        Some(Self {
            levels: vec![MenuLevel { menu, selected }],
            target,
            anchor_x,
            anchor_y,
            centered,
        })
    }

    pub(crate) fn current(&self) -> &MenuLevel {
        self.levels
            .last()
            .expect("a menu session always retains its root")
    }

    pub(crate) fn current_mut(&mut self) -> &mut MenuLevel {
        self.levels
            .last_mut()
            .expect("a menu session always retains its root")
    }

    pub(crate) fn move_selection(&mut self, forward: bool) {
        let level = self.current_mut();
        let Some(next) = adjacent_selectable(&level.menu.entries, level.selected, forward) else {
            return;
        };
        level.selected = next;
    }

    pub(crate) fn select_edge(&mut self, last: bool) {
        let level = self.current_mut();
        let selected = if last {
            level
                .menu
                .entries
                .iter()
                .rposition(RuntimeMenuEntry::selectable)
        } else {
            first_selectable(&level.menu.entries)
        };
        if let Some(selected) = selected {
            level.selected = selected;
        }
    }

    pub(crate) fn select_accelerator(&mut self, character: char) -> usize {
        let level = self.current_mut();
        let character = character.to_ascii_lowercase();
        let matches = level
            .menu
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.accelerator() == Some(character))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if let Some(index) = matches
            .iter()
            .copied()
            .find(|index| *index > level.selected)
            .or_else(|| matches.first().copied())
        {
            level.selected = index;
        }
        matches.len()
    }
}

impl RuntimeMenuEntry {
    pub(crate) const fn selectable(&self) -> bool {
        !matches!(self, Self::Separator { .. })
    }

    pub(crate) const fn accelerator(&self) -> Option<char> {
        match self {
            Self::Item { accelerator, .. } | Self::Submenu { accelerator, .. } => *accelerator,
            Self::Separator { .. } => None,
        }
    }

    pub(crate) fn label(&self) -> Option<&str> {
        match self {
            Self::Item { label, .. } | Self::Submenu { label, .. } => Some(label),
            Self::Separator { label } => label.as_deref(),
        }
    }
}

pub(crate) fn configured_entry(entry: &MenuEntry) -> RuntimeMenuEntry {
    match entry {
        MenuEntry::Item { label, actions } => {
            let (label, accelerator) = display_label(label);
            RuntimeMenuEntry::Item {
                label,
                accelerator,
                action: RuntimeMenuAction::Configured(actions.clone()),
                target: None,
            }
        }
        MenuEntry::Submenu { label, menu } => {
            let (label, accelerator) = display_label(label);
            RuntimeMenuEntry::Submenu {
                label,
                accelerator,
                menu: RuntimeSubmenu::Named(menu.clone()),
            }
        }
        MenuEntry::Separator { label } => RuntimeMenuEntry::Separator {
            label: label.clone(),
        },
    }
}

pub(crate) fn action_entry(
    label: &str,
    action: RuntimeMenuAction,
    target: Option<ClientId>,
) -> RuntimeMenuEntry {
    let (label, accelerator) = display_label(label);
    RuntimeMenuEntry::Item {
        label,
        accelerator,
        action,
        target,
    }
}

pub(crate) fn submenu_entry(label: &str, menu: RuntimeSubmenu) -> RuntimeMenuEntry {
    let (label, accelerator) = display_label(label);
    RuntimeMenuEntry::Submenu {
        label,
        accelerator,
        menu,
    }
}

pub(crate) fn display_label(label: &str) -> (String, Option<char>) {
    let mut output = String::with_capacity(label.len());
    let mut characters = label.chars();
    let mut accelerator = None;
    while let Some(character) = characters.next() {
        if character == '_'
            && let Some(next) = characters.next()
        {
            if next == '_' {
                output.push('_');
            } else {
                accelerator.get_or_insert(next.to_ascii_lowercase());
                output.push(next);
            }
        } else {
            output.push(character);
        }
    }
    (output, accelerator)
}

fn first_selectable(entries: &[RuntimeMenuEntry]) -> Option<usize> {
    entries.iter().position(RuntimeMenuEntry::selectable)
}

fn adjacent_selectable(
    entries: &[RuntimeMenuEntry],
    current: usize,
    forward: bool,
) -> Option<usize> {
    if entries.is_empty() {
        return None;
    }
    for offset in 1..=entries.len() {
        let index = if forward {
            current.saturating_add(offset) % entries.len()
        } else {
            current.wrapping_add(entries.len()).wrapping_sub(offset) % entries.len()
        };
        if entries[index].selectable() {
            return Some(index);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_and_navigation_are_bounded_and_deterministic() {
        assert_eq!(
            display_label("_Open __ file"),
            ("Open _ file".to_owned(), Some('o'))
        );
        let mut session = MenuSession::new(
            RuntimeMenu {
                title: "Test".to_owned(),
                entries: vec![
                    RuntimeMenuEntry::Separator { label: None },
                    action_entry("_First", RuntimeMenuAction::Dismiss, None),
                    action_entry("_Final", RuntimeMenuAction::Dismiss, None),
                ],
            },
            None,
            0,
            0,
            true,
        )
        .unwrap();
        assert_eq!(session.current().selected, 1);
        session.move_selection(true);
        assert_eq!(session.current().selected, 2);
        session.move_selection(true);
        assert_eq!(session.current().selected, 1);
        assert_eq!(session.select_accelerator('f'), 2);
        assert_eq!(session.current().selected, 2);
    }

    #[test]
    fn overflow_uses_bounded_more_submenus_recursively() {
        let entries = (0..8)
            .map(|index| action_entry(&format!("Item {index}"), RuntimeMenuAction::Dismiss, None))
            .collect();
        let menu = paginate_runtime_menu(
            RuntimeMenu {
                title: "Long".to_owned(),
                entries,
            },
            4,
        );

        let mut page = &menu;
        let mut lengths = Vec::new();
        loop {
            lengths.push(page.entries.len());
            let Some(RuntimeMenuEntry::Submenu {
                label,
                menu: RuntimeSubmenu::Inline(next),
                ..
            }) = page.entries.last()
            else {
                break;
            };
            assert_eq!(label, "More...");
            page = next;
        }
        assert_eq!(lengths, vec![4, 4, 2]);
    }
}
