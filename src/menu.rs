#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    Rename,
    Copy,
    Move,
    MkDir,
    NewFile,
    Delete,
    View,
    Edit,
    Help,
    Settings,
    Quit,
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
            Action::View => "View",
            Action::Edit => "Edit",
            Action::Help => "Help",
            Action::Settings => "Settings",
            Action::Quit => "Quit",
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
        items: &[
            Action::Open,
            Action::Rename,
            Action::Copy,
            Action::Move,
            Action::MkDir,
            Action::NewFile,
            Action::Delete,
        ],
    },
    MenuCategory {
        title: "Edit",
        items: &[Action::View, Action::Edit],
    },
    MenuCategory {
        title: "Options",
        items: &[Action::Settings],
    },
    MenuCategory {
        title: "Command",
        items: &[Action::Quit],
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
        label: "Settings",
        action: Some(Action::Settings),
    },
    FnKey {
        key: "F10",
        label: "Menu",
        action: None,
    },
];
