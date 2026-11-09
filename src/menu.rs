#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Rename,
    Copy,
    Move,
    MkDir,
    NewFile,
    Delete,
    DeletePermanently,
    View,
    Edit,
    Help,
    Settings,
    About,
    ShowTerminal,
    ShowLogs,
    QuickSearch,
    SortByName,
    SortByExtension,
    SortBySize,
    SortByDate,
    ShowProgress,
    RenameSelected,
    SelectFile,
    SelectFiles,
    UnselectFiles,
    InvertSelection,
    Quit,
    /// The built-in editor's own: File > Save/Close and the Edit menu,
    /// shown only while it's open.
    Save,
    SaveAs,
    Close,
    /// Command > Open Editor: the built-in editor on no file yet.
    OpenEditor,
    WrapLines,
    SelectAll,
    Find,
    FindNext,
    /// Not an action: a horizontal divider between groups in a pulldown
    /// menu. Never enabled, so menu navigation steps over it.
    Separator,
}

impl Action {
    pub fn label(&self) -> &'static str {
        match self {
            Action::Open => "Open",
            Action::Rename => "Rename",
            Action::Copy => "Copy",
            Action::Move => "Move",
            Action::MkDir => "MkDir",
            Action::NewFile => "New File",
            Action::Delete => "Delete",
            Action::DeletePermanently => "Delete Permanently",
            Action::View => "View",
            Action::Edit => "Edit",
            Action::Help => "Help",
            Action::Settings => "Settings",
            Action::About => "About",
            Action::ShowTerminal => "Show Terminal",
            Action::ShowLogs => "Show Logs",
            Action::QuickSearch => "Quick Search",
            Action::SortByName => "Sort by Name",
            Action::SortByExtension => "Sort by Type",
            Action::SortBySize => "Sort by Size",
            Action::SortByDate => "Sort by Date",
            Action::ShowProgress => "Show Progress",
            Action::RenameSelected => "Rename Selected",
            Action::SelectFile => "Select File",
            Action::SelectFiles => "Select Files",
            Action::UnselectFiles => "Unselect Files",
            Action::InvertSelection => "Invert Selection",
            Action::Quit => "Quit",
            Action::Save => "Save",
            Action::SaveAs => "Save As...",
            Action::OpenEditor => "Open Editor",
            Action::Close => "Close",
            Action::WrapLines => "Wrap Lines",
            Action::SelectAll => "Select All",
            Action::Find => "Find...",
            Action::FindNext => "Find Next",
            Action::Separator => "",
        }
    }

    /// The keybinding shown next to this action in the pulldown menu.
    /// Derived from `FN_KEYS` so the two never drift apart, with the couple
    /// of bindings that aren't plain F-keys (Open, New File) hardcoded.
    pub fn shortcut(&self) -> Option<&'static str> {
        match self {
            Action::Open => Some("Enter"),
            Action::NewFile => Some("Shift+F4"),
            Action::DeletePermanently => Some("Shift+F8"),
            Action::SelectFile => Some("Insert"),
            Action::ShowTerminal => Some("Ctrl+O"),
            Action::ShowLogs => Some("Ctrl+L"),
            Action::Settings => Some("Ctrl+S"),
            Action::ShowProgress => Some("Ctrl+B"),
            Action::QuickSearch => Some("Ctrl+F"),
            Action::Save => Some("Ctrl+S"),
            Action::Close => Some("Esc"),
            Action::SelectAll => Some("Ctrl+A"),
            Action::Find => Some("Ctrl+F"),
            Action::FindNext => Some("F3"),
            other => FN_KEYS
                .iter()
                .find(|fn_key| fn_key.action == Some(*other))
                .map(|fn_key| fn_key.key),
        }
    }
}

pub struct MenuCategory {
    pub title: &'static str,
    pub items: &'static [Action],
}

const OPTIONS: MenuCategory = MenuCategory {
    title: "Options",
    items: &[Action::Settings],
};

/// Options while F3's quick view is open: Wrap Lines too, which it follows
/// as the editor does (the editor has it in its own Edit menu).
const PREVIEW_OPTIONS: MenuCategory = MenuCategory {
    title: "Options",
    items: &[Action::WrapLines, Action::Settings],
};

const FILE: MenuCategory = MenuCategory {
    title: "File",
    // Ordered by keybinding (Enter, then F2..F8) to match the F1 help
    // screen, in groups split by separators.
    items: &[
        Action::Open,
        Action::Rename,
        Action::RenameSelected,
        Action::View,
        Action::Edit,
        Action::NewFile,
        Action::Separator,
        Action::Copy,
        Action::Move,
        Action::MkDir,
        Action::Separator,
        Action::Delete,
        Action::DeletePermanently,
        Action::Separator,
        Action::SelectFile,
        Action::SelectFiles,
        Action::UnselectFiles,
        Action::InvertSelection,
    ],
};

const COMMAND: MenuCategory = MenuCategory {
    title: "Command",
    items: &[
        Action::OpenEditor,
        Action::QuickSearch,
        Action::Separator,
        Action::SortByName,
        Action::SortByExtension,
        Action::SortBySize,
        Action::SortByDate,
        Action::Separator,
        Action::ShowTerminal,
        Action::ShowLogs,
        Action::ShowProgress,
        Action::Separator,
        Action::About,
        Action::Quit,
    ],
};

pub const MENU_BAR: &[MenuCategory] = &[FILE, OPTIONS, COMMAND];

/// `MENU_BAR` with the quick view open: the same categories, so an index
/// into one is good in the other.
pub const PREVIEW_MENU_BAR: &[MenuCategory] = &[FILE, PREVIEW_OPTIONS, COMMAND];

/// The menu bar while the built-in editor is open: only what makes sense
/// there — the panes' items would act on the listing hidden behind it, so
/// they aren't offered at all. Settings stays (its dialog works over the
/// editor, and the editor's own setting is there), and so do Show Logs (where
/// "Saved" lands), About and Quit, in Command as everywhere else. Every
/// title still starts with its own letter, for the Alt+letter shortcuts.
pub const EDITOR_MENU_BAR: &[MenuCategory] = &[
    MenuCategory {
        title: "File",
        items: &[Action::Save, Action::SaveAs, Action::Rename, Action::Close],
    },
    EDIT,
    OPTIONS,
    MenuCategory {
        title: "Command",
        items: &[
            Action::ShowLogs,
            Action::Separator,
            Action::About,
            Action::Quit,
        ],
    },
];

const EDIT: MenuCategory = MenuCategory {
    title: "Edit",
    items: &[
        Action::WrapLines,
        Action::SelectAll,
        Action::Find,
        Action::FindNext,
    ],
};

/// `pce`'s menu bar: the editor's, without what belongs to the file
/// manager around it (its logs).
pub const PCE_MENU_BAR: &[MenuCategory] = &[
    MenuCategory {
        title: "File",
        items: &[Action::Save, Action::SaveAs, Action::Rename, Action::Close],
    },
    EDIT,
    OPTIONS,
    MenuCategory {
        title: "Command",
        items: &[Action::About, Action::Quit],
    },
];

/// The menu bar to show: `EDITOR_MENU_BAR` while the editor is open,
/// `PREVIEW_MENU_BAR` while the quick view is.
pub fn menu_bar(editing: bool, previewing: bool) -> &'static [MenuCategory] {
    match (editing, previewing) {
        (true, _) => EDITOR_MENU_BAR,
        (false, true) => PREVIEW_MENU_BAR,
        (false, false) => MENU_BAR,
    }
}

pub struct FnKey {
    pub key: &'static str,
    pub label: &'static str,
    /// `None` means this key opens the pulldown menu instead of running an action directly.
    pub action: Option<Action>,
}

pub const FN_KEYS: &[FnKey] = &[
    FnKey {
        key: "F1",
        label: "Help",
        action: Some(Action::Help),
    },
    FnKey {
        key: "F2",
        label: "Rename",
        action: Some(Action::Rename),
    },
    FnKey {
        key: "F3",
        label: "View",
        action: Some(Action::View),
    },
    FnKey {
        key: "F4",
        label: "Edit",
        action: Some(Action::Edit),
    },
    FnKey {
        key: "F5",
        label: "Copy",
        action: Some(Action::Copy),
    },
    FnKey {
        key: "F6",
        label: "Move",
        action: Some(Action::Move),
    },
    FnKey {
        key: "F7",
        label: "MkDir",
        action: Some(Action::MkDir),
    },
    FnKey {
        key: "F8",
        label: "Delete",
        action: Some(Action::Delete),
    },
    FnKey {
        key: "F9",
        label: "Menu",
        action: None,
    },
    FnKey {
        key: "F10",
        label: "Quit",
        action: Some(Action::Quit),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_menus_offer_only_the_editors_own_items() {
        let items: Vec<Action> = EDITOR_MENU_BAR
            .iter()
            .flat_map(|category| category.items.iter().copied())
            .filter(|&action| action != Action::Separator)
            .collect();
        assert_eq!(
            items,
            [
                Action::Save,
                Action::SaveAs,
                Action::Rename,
                Action::Close,
                Action::WrapLines,
                Action::SelectAll,
                Action::Find,
                Action::FindNext,
                Action::Settings,
                Action::ShowLogs,
                Action::About,
                Action::Quit,
            ]
        );
        assert_eq!(Action::Close.shortcut(), Some("Esc"));
        // Outside the editor, Wrap Lines is in Options only with the quick
        // view open, the bar otherwise the same.
        assert_eq!(MENU_BAR[1].items, [Action::Settings]);
        assert_eq!(
            PREVIEW_MENU_BAR[1].items,
            [Action::WrapLines, Action::Settings]
        );
        assert_eq!(MENU_BAR.len(), PREVIEW_MENU_BAR.len());
        for (plain, preview) in MENU_BAR.iter().zip(PREVIEW_MENU_BAR) {
            assert_eq!(plain.title, preview.title);
        }
        assert_eq!(menu_bar(true, true)[1].title, "Edit");
        assert_eq!(Action::Quit.shortcut(), Some("F10"));
    }

    #[test]
    fn menu_titles_start_with_different_letters() {
        for bar in [MENU_BAR, PREVIEW_MENU_BAR, EDITOR_MENU_BAR] {
            let mut letters: Vec<char> = bar
                .iter()
                .map(|category| category.title.chars().next().unwrap())
                .collect();
            letters.sort();
            letters.dedup();
            assert_eq!(letters.len(), bar.len());
        }
    }
}
