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

pub const MENU_BAR: &[MenuCategory] = &[
    MenuCategory {
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
    },
    MenuCategory {
        title: "Options",
        items: &[Action::Settings],
    },
    MenuCategory {
        title: "Command",
        items: &[
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
    },
];

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
