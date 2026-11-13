//! Non-Windows stand-ins (used only for development on other hosts).

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use super::{ContextMenuRequest, DriveInfo, MenuEntry, ShellEvent};

pub fn init_instance() -> u32 {
    1
}
pub fn is_only_running_instance(_mine: u32) -> bool {
    true
}
pub fn claim_color(_index: usize) -> bool {
    true
}
pub fn register_window(_instance: u32, _hwnd: isize) {}
pub fn enable_drop(_hwnd: isize) {}
pub fn set_drop_folder(_folder: Option<PathBuf>) {}
pub fn set_title_color(_hwnd: isize, _rgb: Option<u32>) {}
pub fn set_overlay_number(_hwnd: isize, _n: u32, _rgb: u32) -> bool {
    true
}
pub fn drives() -> Vec<DriveInfo> {
    vec![DriveInfo { root: "/".into(), label: String::new() }]
}
pub fn file_icon_png(_path: &Path, _is_dir: bool, _per_file: bool) -> Option<Vec<u8>> {
    None
}
pub fn client_to_screen(_hwnd: isize, x: i32, y: i32) -> (i32, i32) {
    (x, y)
}
pub fn open_path(_path: &Path) {}
pub fn start_window_drag(_hwnd: isize) {}
pub fn cursor_pos() -> (i32, i32) {
    (0, 0)
}
pub fn window_pos(_hwnd: isize) -> (i32, i32) {
    (0, 0)
}
pub fn move_window(_hwnd: isize, _x: i32, _y: i32) {}
pub fn drag_files_out(_owner: isize, _paths: &[PathBuf]) {}
pub fn left_alt_pressed() -> bool {
    false
}

pub struct Shell;

impl Shell {
    pub fn start(_events: Sender<ShellEvent>) -> Shell {
        Shell
    }
    pub fn context_menu(&self, _req: ContextMenuRequest) {}
    /// Runs a shell verb ("copy", "cut", "paste", "delete") on the items, or — when
    /// `targets` is empty — on the folder background.
    pub fn verb(&self, _owner: isize, _targets: Vec<PathBuf>, _folder: PathBuf, _verb: &'static str, _shift: bool) {}
    pub fn popup_list(&self, _owner: isize, _pt: (i32, i32), _items: Vec<MenuEntry>) {}
    pub fn pick_color(&self, _owner: isize) {}
    pub fn pick_folder(&self, _owner: isize, _start: Option<PathBuf>) {}
}
