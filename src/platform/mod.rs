//! Everything that talks to Windows lives behind this module. On other hosts a no-op
//! stub is used, so the UI code can still be built and type-checked there.

use std::path::PathBuf;

#[cfg(any(windows, miniexpo_check))]
#[path = "win.rs"]
mod imp;
#[cfg(not(any(windows, miniexpo_check)))]
#[path = "stub.rs"]
mod imp;

pub use imp::*;

pub struct DriveInfo {
    /// "C:\"
    pub root: String,
    /// Volume label, may be empty.
    pub label: String,
}

/// What the shell thread reports back to the UI.
pub enum ShellEvent {
    /// "Add / Remove Favorite" was chosen for this folder.
    ToggleFavorite(PathBuf),
    /// "Open" was chosen on a folder: browse into it inside miniExpo.
    OpenFolder(PathBuf),
    /// "Refresh" was chosen in the context menu: re-read the folder at once.
    ReloadNow,
    /// "New Window" was chosen in the context menu.
    NewWindow,
    /// The global hotkey was pressed: open a fresh window at My Computer.
    HotkeyNewWindow,
    /// "Rename" was chosen: edit the name in place.
    Rename(PathBuf),
    /// A drive or favorite was picked from the Favorites menu.
    Navigate(PathBuf),
    /// The screen eyedropper finished on this colour.
    ColorPicked(u32),
    /// The eyedropper was dismissed with Esc or a right-click.
    ColorPickCancelled,
    /// Something is being dragged over the window and we would accept it.
    DragHover(bool),
    /// A shell command ran; the folder may have changed.
    Refresh,
    /// These names just landed in the open folder (a drop): re-read, then select them so the
    /// arrival is visible.
    Arrived(Vec<String>),
    /// A command entry was picked from a popup menu (the address bar's right-click menu, or
    /// the Favorites menu's "session group" buttons).
    MenuCommand(MenuCommand),
}

pub struct ContextMenuRequest {
    /// The miniExpo window this menu belongs to: anything the shell opens from it is owned by
    /// that window, so closing a dialog hands the focus back there.
    pub owner: isize,
    /// Selected items; empty = right-click on the blank area of `folder`.
    pub targets: Vec<PathBuf>,
    /// The open folder, or None at the My Computer level.
    pub folder: Option<PathBuf>,
    /// Screen position (physical px).
    pub pt: (i32, i32),
    /// Folder that "Add/Remove Favorite" applies to, if any.
    pub fav_target: Option<PathBuf>,
    pub fav_is_set: bool,
    pub extended: bool,
}

pub enum MenuEntry {
    /// A drive or a user favorite: picking it navigates miniExpo into that folder.
    Item { text: String, path: PathBuf },
    /// A shortcut to a file on disk: picking it opens that file with its default app, rather
    /// than navigating into it. Used for the "favorites.json" entry in the Favorites menu.
    OpenFile { text: String, path: PathBuf },
    /// A plain command with no path of its own — used for the address bar's right-click menu
    /// and for the Favorites menu's "session group" buttons.
    Action { text: String, command: MenuCommand },
    Separator,
}

/// Commands offered by a popup menu entry that isn't tied to a path of its own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    /// Copies the address bar's current text to the clipboard.
    AddrCopy,
    /// Pastes the clipboard's text at the caret / over the current selection.
    AddrPaste,
    /// Pastes the clipboard's text over the whole address and navigates there at once.
    AddrPasteAndGo,
    /// "Update Session Group": snapshots every currently open window's folder and bounds.
    UpdateSessionGroup,
    /// "Open Last Session Group": reopens every window from that snapshot not already open.
    OpenLastSessionGroup,
}
