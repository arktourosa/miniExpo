// miniExpo — a small file manager (Rust + GPUI).
// Release builds on Windows run without a console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod fsops;
mod icons;
mod platform;
mod text_input;

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use gpui::{
    div, img, prelude::*, px, rgb, size, uniform_list, AnyElement, App, Application, Bounds, ClipboardItem,
    Context, DragMoveEvent, ElementInputHandler, EntityInputHandler, FocusHandle, FontWeight, Image, ImageFormat,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, NavigationDirection, Point, Pixels,
    Render, ScrollStrategy, SharedString, TitlebarOptions, UTF16Selection, UniformListScrollHandle, Window,
    WindowBounds, WindowOptions,
};

use fsops::{Entry, SessionWindow, SortKey};
use platform::{ContextMenuRequest, MenuCommand, MenuEntry, ShellEvent};
use text_input::{Outcome, TextInput};

// ───────────────────────── look ─────────────────────────
// Sizes follow Windows Explorer's details view at 100 % scale: Segoe UI 9 pt = 12 px text,
// 28 px rows (measured off the user's side-by-side screenshot).
const UI_FONT: &str = "Segoe UI";
const FONT_PX: f32 = 12.;
const ROW_H: f32 = 28.;
const HEADER_H: f32 = 28.;
const ADDR_H: f32 = 40.;
const STATUS_H: f32 = 28.;
const STATUS_BTN: f32 = 22.;
const CTRL_H: f32 = 28.;
const SIZE_W: f32 = 96.;
const DATE_W: f32 = 150.;
/// Blank rows kept below the last item so the list never ends flush with the window edge.
const TAIL_ROWS: usize = 5;
/// A second plain click on the already sole-selected row starts a rename only if it follows the
/// previous one within this long; any slower and it is just a normal (re-)selecting click.
const RENAME_CLICK_WINDOW: Duration = Duration::from_secs(2);
const SCROLLBAR_W: f32 = 10.;
const SCROLLBAR_MIN_THUMB: f32 = 24.;

const TEXT: u32 = 0x000000;
const HEADER_TEXT: u32 = 0x4C607A;
const LINE: u32 = 0xE5E5E5;
const BORDER: u32 = 0xC8C8C8;
const ACCENT: u32 = 0x0078D4;
const ERROR: u32 = 0xC42B1C;
const SEL_BORDER: u32 = 0x99D1FF;
const HOVER_BG: u32 = 0xE5F3FF;
/// Files modified within the last 30 minutes (sampled from the mock-up).
const RECENT_BG: u32 = 0x5C9E1B;
const RECENT_TEXT: u32 = 0xFFFFFF;
/// Alpha of the big number behind the file list, out of 255: the first for a dark colour, the
/// second for one that is nearly white.
const WATERMARK_ALPHA_MIN: u32 = 0x16;
const WATERMARK_ALPHA_MAX: u32 = 0x55;

/// The first swatch is the default: picking it hands the title bar back to Windows. This is also
/// the colour window 0 writes its background number in.
const DEFAULT_COLOR: u32 = 0x334155;

/// Title-bar colour presets, laid out five to a row in the colour popup.
const PRESET_COLORS: [u32; 30] = [
    DEFAULT_COLOR, 0xFF9100, 0x52B69A, 0xE5383B, 0xE9EDC9, //
    0x540B0E, 0x90A955, 0xA4133C, 0xCED4DA, 0xFFB703, //
    0x014F86, 0xFB6F92, 0x03045E, 0x3A86FF, 0xFF6D00, //
    0xC6D8FF, 0xB5E48C, 0xEE3324, 0xFF0000, 0x7B2CBF, //
    0xAFFC41, 0x1E73BE, 0x1BA648, 0xEA6F22, 0x390099, //
    0x2C6E49, 0x003566, 0x4C956C, 0xF6AE2D, 0xA8C256,
];
/// Swatches per row, and their size — the popup's width follows from these.
const SWATCH_COLS: usize = 5;
const SWATCH: f32 = 40.;
const SWATCH_GAP: f32 = 6.;
/// Taskbar number badge colour while no title colour is chosen.
const DEFAULT_BADGE: u32 = 0x0F6CBD;

/// Where the list is pointing. `None` is the "My Computer" level, which lists the drives.
type Loc = Option<PathBuf>;

const MY_COMPUTER: &str = "My Computer";

struct Rename {
    path: PathBuf,
    input: TextInput,
}

struct ColorPopup {
    input: TextInput,
    error: bool,
    /// The screen eyedropper is running.
    picking: bool,
}

/// The payload GPUI's drag system needs to keep tracking pointer moves past the scrollbar's own
/// bounds while the thumb is being dragged. It renders nothing — the thumb itself already tracks
/// the pointer every frame, so there's nothing useful to show as a drag preview.
struct ScrollDragGhost;

impl Render for ScrollDragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Marker for the rubber-band drag started on empty space.
struct MarqueeDrag;

/// A rubber-band selection in progress. x is in window pixels; y is measured from the top of the
/// list's *content*, so the corner stays put on the rows when the list scrolls under it.
struct Marquee {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    /// What was selected before (kept only when Ctrl / Shift is held, to add to it).
    base: BTreeSet<usize>,
    /// The pointer has actually moved: a plain click draws nothing.
    active: bool,
}

/// A left press on a row that may turn into dragging the selected files out of the window.
struct RowPress {
    at: Point<Pixels>,
    row: usize,
    /// Pressed on one of several selected rows: the others stay selected so they can all be
    /// dragged, and the selection collapses to this row only if the button comes up without a drag.
    collapse: bool,
    /// Pressed on the name of the item that was already the sole selection: as in Explorer,
    /// this starts an in-place rename if the button comes up here without a drag having started.
    rename_candidate: bool,
}

/// A paste (or other shell command that makes files) is under way: whatever turns up in the
/// folder that was not there before gets selected.
struct PendingPaste {
    cwd: Loc,
    before: HashMap<String, Option<std::time::SystemTime>>,
    until: Instant,
    found: usize,
}

struct MiniExpo {
    instance: u32,
    hwnd: isize,
    /// Current folder, or `None` for My Computer.
    cwd: Loc,
    entries: Vec<Entry>,
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    sort_key: SortKey,
    sort_asc: bool,
    marquee: Option<Marquee>,
    press: Option<RowPress>,
    pending_paste: Option<PendingPaste>,
    /// Row and time of the last plain (unmodified) single left-click on a row, so a second
    /// click on the already-selected row only starts a rename when it follows within
    /// `RENAME_CLICK_WINDOW` — like Explorer's slow-double-click rename, but reset once that
    /// window has passed, so a clearly separate click doesn't fall into renaming by accident.
    last_plain_click: Option<(usize, Instant)>,
    /// A right-button press not yet resolved into either the context menu or a window drag:
    /// recorded on mouse-down (wherever it landed — a row or blank space), consumed on
    /// mouse-up to finally show the menu, or cleared early if the pointer wanders far enough
    /// first, in which case a window drag starts instead and the menu never appears.
    pending_right_click: Option<MouseDownEvent>,
    /// A right-button hold-drag under way: the cursor's screen position and the window's
    /// screen position at the moment the drag started, so every later move can compute the
    /// same delta for both and reposition the window by hand. Windows' own move loop
    /// (`start_window_drag`) only terminates on the *left* button's release, so it can't be
    /// reused for a drag that is actually held with the right button — this moves the window
    /// directly instead, driven by our own mouse-move events.
    right_drag: Option<(i32, i32, i32, i32)>,

    addr: TextInput,
    addr_error: bool,
    addr_focus: FocusHandle,
    list_focus: FocusHandle,
    rename_focus: FocusHandle,
    color_focus: FocusHandle,
    scroll: UniformListScrollHandle,
    icons: icons::IconCache,
    /// Toolbar icons: open, favorites, new window, up, eyedropper, bigger, smaller, reset.
    ui_icons: [std::sync::Arc<Image>; 8],

    /// Where this window has been, for the mouse's back / forward buttons.
    back: Vec<Loc>,
    forward: Vec<Loc>,

    /// Something acceptable is being dragged over the window.
    drag_hover: bool,
    rename: Option<Rename>,
    color_popup: Option<ColorPopup>,
    /// Where the rename box was last painted, to turn a click into a caret position.
    rename_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
    /// Where the address bar / colour-hex box were last painted, for IME candidate-window
    /// placement (see `EntityInputHandler` below). Neither box supports click-to-position yet,
    /// so these only feed `bounds_for_range` / `character_index_for_point`.
    addr_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
    color_bounds: std::rc::Rc<std::cell::Cell<gpui::Bounds<Pixels>>>,
    title_color: Option<u32>,
    overlay_ok: bool,
    overlay_tries: u32,

    shell: platform::Shell,
    shell_rx: Receiver<ShellEvent>,
    watcher: Option<notify::RecommendedWatcher>,
    watch_tx: Sender<()>,
    watch_rx: Receiver<()>,
    dirty_since: Option<Instant>,
    /// A shell command is in flight: re-read for a while, since the work finishes after the
    /// call that started it returns.
    settle_until: Option<Instant>,
    /// Last time the folder was re-read, and the folder's own timestamp when we did.
    last_poll: Instant,
    last_reload: Instant,
    last_dir_stamp: Option<std::time::SystemTime>,
    was_active: bool,
    last_redraw: Instant,
    /// Window bounds as last observed / last written to the session file, and when they last
    /// changed: a move or resize is only persisted once the window has been still for a short
    /// while, not on every intermediate frame of a drag.
    last_seen_bounds: (i32, i32, i32, i32),
    last_saved_bounds: (i32, i32, i32, i32),
    bounds_settle_since: Option<Instant>,
    _ticker: gpui::Task<()>,
}

impl MiniExpo {
    fn new(instance: u32, start: Loc, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Everything the shell thread reports — favorites, drops, deletes — reaches the window
        // through this loop, so it must not stop for anything but the window closing. Updating a
        // window takes it out of GPUI's slot for the duration, so any nested message pump (the
        // system's window-move loop, say) can poll this task while the slot is empty and get an
        // error back. That is transient; only a view that can no longer be upgraded is final.
        let ticker = cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(100)).await;
            if this.update_in(cx, |v, w, cx| v.tick(w, cx)).is_err() && this.upgrade().is_none() {
                break;
            }
        });
        let (shell_tx, shell_rx) = mpsc::channel();
        let (watch_tx, watch_rx) = mpsc::channel();
        let mut this = Self {
            instance,
            hwnd: window_hwnd(window),
            cwd: None,
            entries: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            sort_key: SortKey::Name,
            sort_asc: true,
            marquee: None,
            press: None,
            pending_paste: None,
            last_plain_click: None,
            pending_right_click: None,
            right_drag: None,
            addr: TextInput::default(),
            addr_error: false,
            addr_focus: cx.focus_handle(),
            list_focus: cx.focus_handle(),
            rename_focus: cx.focus_handle(),
            color_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            icons: icons::IconCache::default(),
            ui_icons: [
                include_bytes!("../assets/ico-open.png").as_slice(),
                include_bytes!("../assets/ico-fav.png").as_slice(),
                include_bytes!("../assets/ico-new.png").as_slice(),
                include_bytes!("../assets/ico-up.png").as_slice(),
                include_bytes!("../assets/eyedropper.png").as_slice(),
                include_bytes!("../assets/ico-bigger.png").as_slice(),
                include_bytes!("../assets/ico-smaller.png").as_slice(),
                include_bytes!("../assets/ico-reset.png").as_slice(),
            ]
            .map(|png| std::sync::Arc::new(Image::from_bytes(ImageFormat::Png, png.to_vec()))),
            back: Vec::new(),
            forward: Vec::new(),
            drag_hover: false,
            rename: None,
            color_popup: None,
            rename_bounds: Default::default(),
            addr_bounds: Default::default(),
            color_bounds: Default::default(),
            title_color: None,
            overlay_ok: false,
            overlay_tries: 0,
            shell: platform::Shell::start(shell_tx),
            shell_rx,
            watcher: None,
            watch_tx,
            watch_rx,
            dirty_since: None,
            settle_until: None,
            last_poll: Instant::now(),
            last_reload: Instant::now(),
            last_dir_stamp: None,
            was_active: true,
            last_redraw: Instant::now(),
            last_seen_bounds: (0, 0, 0, 0),
            last_saved_bounds: (0, 0, 0, 0),
            bounds_settle_since: None,
            _ticker: ticker,
        };
        // Let the other windows' hotkeys find this one.
        platform::register_window(instance, this.hwnd);
        // Our own drop target: GPUI's only accepts files that exist on disk, not the virtual
        // ones Outlook and friends drag.
        platform::enable_drop(this.hwnd);
        // Window 0 keeps the plain Windows title bar; the rest take a colour of their own so
        // several open windows are easy to tell apart.
        if let Some(c) = pick_window_color(instance) {
            this.title_color = Some(c);
            platform::set_title_color(this.hwnd, Some(c));
        }
        if !this.goto(start, None, window) {
            // The folder passed on the command line is gone: fall back to My Computer.
            this.goto(None, None, window);
        }
        window.focus(&this.list_focus);
        this
    }

    // ───────────────────────── navigation ─────────────────────────

    /// The entries of a location: a folder's contents, or the drive list for My Computer.
    fn read_loc(loc: &Loc) -> std::io::Result<Vec<Entry>> {
        match loc {
            Some(dir) => {
                let mut v = fsops::list_dir(dir)?;
                // ".." — sorting pins it to the top.
                v.push(Entry::parent_row(dir));
                Ok(v)
            }
            None => Ok(platform::drives()
                .into_iter()
                .map(|d| Entry {
                    // Explorer's wording: "Local Disk (C:)".
                    name: format!(
                        "{} ({})",
                        if d.label.is_empty() { "Local Disk".to_string() } else { d.label },
                        d.root.trim_end_matches(['\\', '/'])
                    ),
                    path: PathBuf::from(d.root),
                    is_dir: true,
                    is_drive: true,
                    is_parent: false,
                    size: 0,
                    modified: None,
                })
                .collect()),
        }
    }

    fn loc_title(loc: &Loc) -> String {
        match loc {
            Some(p) => fsops::folder_title(p),
            None => MY_COMPUTER.to_string(),
        }
    }

    fn loc_address(loc: &Loc) -> String {
        match loc {
            Some(p) => fsops::display_path(p),
            None => MY_COMPUTER.to_string(),
        }
    }

    /// Opens `loc` and remembers where we came from, so the mouse's back button can return.
    fn navigate(&mut self, loc: Loc, select: Option<String>, window: &mut Window) -> bool {
        let previous = self.cwd.clone();
        let moved = loc != previous;
        if !self.goto(loc, select, window) {
            return false;
        }
        if moved {
            self.back.push(previous);
            if self.back.len() > 64 {
                self.back.remove(0); // keep the most recent 64, drop the oldest
            }
            self.forward.clear();
        }
        true
    }

    /// Mouse back button / Alt+Left.
    fn go_back(&mut self, window: &mut Window) {
        let Some(loc) = self.back.pop() else { return };
        let here = self.cwd.clone();
        if self.goto(loc, None, window) {
            self.forward.push(here);
        }
    }

    /// Mouse forward button / Alt+Right.
    fn go_forward(&mut self, window: &mut Window) {
        let Some(loc) = self.forward.pop() else { return };
        let here = self.cwd.clone();
        if self.goto(loc, None, window) {
            self.back.push(here);
        }
    }

    /// Opens `loc` in the list. Returns false (and leaves everything as it was) on failure.
    fn goto(&mut self, loc: Loc, select: Option<String>, window: &mut Window) -> bool {
        let entries = match Self::read_loc(&loc) {
            Ok(e) => e,
            Err(_) => {
                self.addr_error = true;
                return false;
            }
        };
        self.addr_error = false;
        self.rename = None;
        self.cwd = loc;
        self.entries = entries;
        fsops::sort_entries(&mut self.entries, self.sort_key, self.sort_asc);
        self.selected.clear();
        self.anchor = None;
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        if let Some(name) = select {
            // `name` is a file name; drive rows are shown as "Local Disk (C:)" so match on the path too.
            let hit = self.entries.iter().position(|e| {
                e.name.eq_ignore_ascii_case(&name)
                    || e.path.file_name().map(|n| n.eq_ignore_ascii_case(&name)).unwrap_or(false)
                    || fsops::display_path(&e.path).eq_ignore_ascii_case(name.trim_end_matches(['\\', '/']))
            });
            if let Some(i) = hit {
                self.selected.insert(i);
                self.anchor = Some(i);
                self.scroll.scroll_to_item(i, ScrollStrategy::Center);
            }
        }
        platform::set_drop_folder(self.cwd.clone());
        self.addr.set_text(&Self::loc_address(&self.cwd));
        window.set_window_title(&format!("{} - {}", self.instance, Self::loc_title(&self.cwd)));
        self.watch();
        self.persist_session(window);
        true
    }

    /// Physical window bounds as `(x, y, w, h)`, for the session file.
    fn bounds_tuple(window: &Window) -> (i32, i32, i32, i32) {
        let b = window.bounds();
        (f32::from(b.origin.x) as i32, f32::from(b.origin.y) as i32, f32::from(b.size.width) as i32, f32::from(b.size.height) as i32)
    }

    /// Saves this window's current folder and bounds to the session file (opening it, and
    /// every navigation, call this directly; a move or resize instead goes through the
    /// debounced check in `tick`), and remembers what was written so `tick` can tell whether
    /// anything has changed since.
    fn persist_session(&mut self, window: &Window) {
        let b = Self::bounds_tuple(window);
        self.last_seen_bounds = b;
        self.last_saved_bounds = b;
        fsops::save_window_state(self.instance, self.cwd.as_deref(), b);
    }

    fn watch(&mut self) {
        use notify::Watcher;
        self.watcher = None;
        let Some(dir) = self.cwd.clone() else { return }; // nothing to watch at My Computer
        let tx = self.watch_tx.clone();
        if let Ok(mut w) = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
            let _ = tx.send(());
        }) {
            if w.watch(&dir, notify::RecursiveMode::NonRecursive).is_ok() {
                self.watcher = Some(w);
            }
        }
    }

    /// Re-reads the current location, keeping the selection (by name).
    /// Re-reads the current location, keeping the selection (by name).
    ///
    /// This must never be skipped. It used to bail out while a name was being edited, and an
    /// edit left open when the window lost focus then silently froze the listing: drops and
    /// deletes stopped showing up and F5 went into the edit box. Now an edit simply survives the
    /// re-read if its file is still there, and is dropped if it is not.
    fn reload(&mut self, window: &mut Window) {
        self.last_reload = Instant::now();
        let names: Vec<String> = self.selected.iter().filter_map(|i| self.entries.get(*i)).map(|e| e.name.clone()).collect();
        match Self::read_loc(&self.cwd) {
            Ok(entries) => {
                self.entries = entries;
                fsops::sort_entries(&mut self.entries, self.sort_key, self.sort_asc);
                self.restore_selection(&names);
                self.apply_pending_paste();
                let editing_gone = self
                    .rename
                    .as_ref()
                    .is_some_and(|r| !self.entries.iter().any(|e| e.path == r.path));
                if editing_gone {
                    self.rename = None;
                    window.focus(&self.list_focus);
                }
            }
            Err(_) => {
                // The folder vanished: go up to the nearest level that still exists.
                let mut loc = self.cwd.clone();
                loop {
                    match loc {
                        Some(ref p) if p.is_dir() => break,
                        Some(ref p) => loc = p.parent().map(Path::to_path_buf),
                        None => break,
                    }
                }
                self.goto(loc, None, window);
            }
        }
    }

    /// Selects these entries and scrolls the first into view — used after a drop, so the new
    /// arrivals are seen rather than left to be hunted for.
    fn select_names(&mut self, names: &[String]) {
        let hits: BTreeSet<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| names.iter().any(|n| n.eq_ignore_ascii_case(&e.name)))
            .map(|(i, _)| i)
            .collect();
        if let Some(first) = hits.iter().next().copied() {
            self.selected = hits;
            self.anchor = Some(first);
            self.scroll.scroll_to_item(first, ScrollStrategy::Center);
        }
    }

    /// Remembers what the folder holds now, so what a paste (or "New") adds can be told apart.
    fn arm_paste_watch(&mut self, secs: u64) {
        self.pending_paste = Some(PendingPaste {
            cwd: self.cwd.clone(),
            before: self.entries.iter().map(|e| (e.name.clone(), e.modified)).collect(),
            until: Instant::now() + Duration::from_secs(secs),
            found: 0,
        });
    }

    /// Selects what has appeared (or been overwritten) since `arm_paste_watch`. Called after every
    /// re-read, so a big copy that lands file by file is followed as it goes.
    fn apply_pending_paste(&mut self) {
        let Some(p) = self.pending_paste.as_mut() else { return };
        if Instant::now() > p.until || p.cwd != self.cwd {
            self.pending_paste = None;
            return;
        }
        let hits: BTreeSet<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.is_parent && p.before.get(&e.name).map_or(true, |old| *old != e.modified))
            .map(|(i, _)| i)
            .collect();
        let Some(first) = hits.iter().next().copied() else { return };
        let grew = hits.len() > p.found;
        if grew {
            p.found = hits.len();
            p.until = p.until.max(Instant::now() + Duration::from_secs(4));
        }
        self.selected = hits;
        if grew {
            self.anchor = Some(first);
            self.scroll.scroll_to_item(first, ScrollStrategy::Center);
        }
    }

    fn restore_selection(&mut self, names: &[String]) {
        self.selected = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| names.iter().any(|n| n == &e.name))
            .map(|(i, _)| i)
            .collect();
        // An anchor left pointing past the end would make shift-click select a bogus range.
        self.anchor = self.anchor.filter(|i| *i < self.entries.len());
    }

    /// One level up: a folder's parent, a drive root → My Computer. No-op at My Computer.
    fn go_up(&mut self, window: &mut Window) {
        let Some(cur) = self.cwd.clone() else { return };
        let parent = cur.parent().map(Path::to_path_buf);
        let came_from = match &parent {
            Some(_) => fsops::folder_title(&cur),
            // leaving a drive root: select that drive in the My Computer list
            None => fsops::display_path(&cur),
        };
        self.navigate(parent, Some(came_from), window);
    }

    /// The "Open" button / Enter in the address bar.
    fn open_typed(&mut self, window: &mut Window) {
        let text = self.addr.text.trim().to_string();
        if text.is_empty() || text.eq_ignore_ascii_case(MY_COMPUTER) {
            self.navigate(None, None, window);
            window.focus(&self.list_focus);
            return;
        }
        let mut p = fsops::parse_user_path(&text);
        // A bare name ("Documents") is taken as relative to the folder on screen.
        if !p.exists() && p.is_relative() {
            if let Some(base) = self.cwd.as_ref() {
                p = base.join(&p);
            }
        }
        if p.is_dir() {
            self.navigate(Some(p), None, window);
        } else if p.is_file() {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
            let parent = p.parent().map(Path::to_path_buf);
            self.navigate(parent, name, window);
        } else {
            // Leave what the user typed in place so they can see and correct it.
            self.addr_error = true;
            return;
        }
        window.focus(&self.list_focus);
    }

    /// Puts the caret in the address bar with the whole path selected, as Explorer does.
    fn focus_address(&mut self, window: &mut Window) {
        self.rename = None;
        self.color_popup = None;
        self.addr.select_all();
        window.focus(&self.addr_focus);
    }

    fn activate(&mut self, i: usize, window: &mut Window) {
        let Some(e) = self.entries.get(i).cloned() else { return };
        if e.is_parent {
            self.go_up(window);
        } else if e.is_dir {
            self.navigate(Some(e.path), None, window);
        } else {
            platform::open_path(&e.path);
        }
    }

    /// Multiplies the window's size, within the screen and the minimum size.
    fn scale_window(&self, factor: f32, window: &mut Window, cx: &App) {
        let b = window.bounds().size;
        self.size_window(f32::from(b.width) * factor, f32::from(b.height) * factor, window, cx);
    }

    fn size_window(&self, w: f32, h: f32, window: &mut Window, cx: &App) {
        let (mut max_w, mut max_h) = (f32::MAX, f32::MAX);
        if let Some(screen) = cx.primary_display().map(|d| d.bounds().size) {
            max_w = f32::from(screen.width) - 40.;
            max_h = f32::from(screen.height) - 80.;
        }
        window.resize(size(px(w.clamp(MIN_W, max_w.max(MIN_W))), px(h.clamp(MIN_H, max_h.max(MIN_H)))));
    }

    /// A free name in the current folder: "New folder", then "New folder (2)" and so on.
    /// `ext` includes the dot, or is empty for a folder.
    fn free_name(dir: &Path, base: &str, ext: &str) -> Option<(String, PathBuf)> {
        for n in 1..1000 {
            let name = if n == 1 { format!("{base}{ext}") } else { format!("{base} ({n}){ext}") };
            let path = dir.join(&name);
            if !path.exists() {
                return Some((name, path));
            }
        }
        None
    }

    /// Selects what was just created and opens its name for editing, as Explorer does. For a
    /// file the extension stays out of the selection, so typing replaces only the name.
    fn after_create(&mut self, name: String, path: PathBuf, window: &mut Window) {
        self.reload(window);
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.select_only(i);
            self.scroll.scroll_to_item(i, ScrollStrategy::Center);
        }
        self.start_rename(&path, window);
    }

    fn new_folder(&mut self, window: &mut Window) {
        let Some(dir) = self.cwd.clone() else { return }; // nothing to create at My Computer
        let Some((name, path)) = Self::free_name(&dir, "New folder", "") else { return };
        if std::fs::create_dir(&path).is_ok() {
            self.after_create(name, path, window);
        }
    }

    /// Creates an empty file of the given kind. A spreadsheet cannot be an empty file — Excel
    /// refuses to open one — so it starts from a minimal but complete workbook.
    fn new_file(&mut self, base: &str, ext: &str, template: &[u8], window: &mut Window) {
        let Some(dir) = self.cwd.clone() else { return };
        let Some((name, path)) = Self::free_name(&dir, base, ext) else { return };
        if std::fs::write(&path, template).is_ok() {
            self.after_create(name, path, window);
        }
    }

    /// Starts another miniExpo. `loc` is its start folder (My Computer when None).
    fn new_window(loc: &Loc) {
        let Ok(exe) = std::env::current_exe() else { return };
        let mut cmd = std::process::Command::new(exe);
        if let Some(p) = loc {
            cmd.arg(p);
        }
        let _ = cmd.spawn();
    }

    fn sort_by(&mut self, key: SortKey) {
        if self.sort_key == key {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort_key = key;
            // names and sizes start ascending; dates start with the newest on top
            self.sort_asc = key != SortKey::Modified;
        }
        let names: Vec<String> = self.selected.iter().filter_map(|i| self.entries.get(*i)).map(|e| e.name.clone()).collect();
        fsops::sort_entries(&mut self.entries, self.sort_key, self.sort_asc);
        self.restore_selection(&names);
    }

    // ───────────────────────── selection ─────────────────────────

    /// The selected items, minus the ".." row — it is a shortcut, not a file.
    fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected
            .iter()
            .filter_map(|i| self.entries.get(*i))
            .filter(|e| !e.is_parent)
            .map(|e| e.path.clone())
            .collect()
    }

    fn select_only(&mut self, i: usize) {
        self.selected.clear();
        self.selected.insert(i);
        self.anchor = Some(i);
    }

    fn move_selection(&mut self, delta: isize, extend: bool) {
        if self.entries.is_empty() {
            return;
        }
        self.pending_paste = None;
        let last = self.entries.len() as isize - 1;
        let cur = self.selected.iter().next_back().copied().map(|i| i as isize);
        let next = match cur {
            None => {
                if delta >= 0 {
                    0
                } else {
                    last
                }
            }
            Some(c) => (c + delta).clamp(0, last),
        } as usize;
        if extend {
            let a = self.anchor.unwrap_or(next).min(self.entries.len() - 1);
            self.selected = (a.min(next)..=a.max(next)).collect();
        } else {
            self.select_only(next);
        }
        self.scroll
            .scroll_to_item(next, if delta >= 0 { ScrollStrategy::Bottom } else { ScrollStrategy::Top });
    }

    fn row_mouse_down(&mut self, i: usize, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.rename = None;
        self.color_popup = None;
        window.focus(&self.list_focus);
        // Left Alt + left button moves the window, wherever in it the press lands.
        if ev.button == MouseButton::Left && platform::left_alt_pressed() {
            self.marquee = None;
            self.press = None;
            self.drag_window_later(cx);
            return;
        }
        self.pending_paste = None;
        self.press = None;
        match ev.button {
            MouseButton::Left => {
                if ev.modifiers.shift {
                    let a = self.anchor.unwrap_or(i);
                    self.selected = (a.min(i)..=a.max(i)).collect();
                } else if ev.modifiers.control {
                    if !self.selected.remove(&i) {
                        self.selected.insert(i);
                    }
                    self.anchor = Some(i);
                } else {
                    let draggable = self.entries.get(i).is_some_and(|e| !e.is_parent) && self.cwd.is_some();
                    // Whether this plain click follows another plain click on this very same row
                    // within RENAME_CLICK_WINDOW — the slow-double-click gesture Explorer uses to
                    // start a rename. A true double-click already has click_count == 2 and takes
                    // the "open it" path below instead; this is only for two separate, slower
                    // clicks. Recorded (and reset) on every plain click, so a click days later on
                    // a row that happens to still be the sole selection just reselects it instead
                    // of reopening the rename window.
                    let now = Instant::now();
                    let quick_second_click = ev.click_count == 1
                        && self.last_plain_click.is_some_and(|(r, t)| r == i && now.duration_since(t) < RENAME_CLICK_WINDOW);
                    if ev.click_count == 1 {
                        self.last_plain_click = Some((i, now));
                    }
                    if ev.click_count == 1 && self.selected.len() > 1 && self.selected.contains(&i) {
                        // Keep the group: it may be about to be dragged away.
                        self.anchor = Some(i);
                        self.press =
                            Some(RowPress { at: ev.position, row: i, collapse: true, rename_candidate: false });
                    } else if quick_second_click && draggable && self.selected.len() == 1 && self.selected.contains(&i) {
                        // A plain click landed on the name of the item that was already the sole
                        // selection, soon enough after the previous click: as in Explorer, this
                        // starts an in-place rename, but only if the button comes back up here
                        // without a drag having started (handled in row_mouse_up). A fast second
                        // click is a double-click (click_count==2) and opens it instead, below.
                        self.anchor = Some(i);
                        self.press =
                            Some(RowPress { at: ev.position, row: i, collapse: false, rename_candidate: true });
                    } else {
                        self.select_only(i);
                        if ev.click_count == 2 {
                            self.activate(i, window);
                            return;
                        }
                        if draggable {
                            self.press = Some(RowPress {
                                at: ev.position,
                                row: i,
                                collapse: false,
                                rename_candidate: false,
                            });
                        }
                    }
                }
            }
            MouseButton::Right => {
                let is_parent = self.entries.get(i).map(|e| e.is_parent).unwrap_or(false);
                if is_parent {
                    self.selected.clear(); // fall back to the folder's background menu
                } else if !self.selected.contains(&i) {
                    self.select_only(i);
                }
                // Selection is settled now, but the menu waits for mouse-up: holding and
                // dragging from here instead moves the window, the same as a right-click-hold
                // on blank space (see maybe_start_right_drag / finish_right_click).
                self.pending_right_click = Some(ev.clone());
            }
            _ => {}
        }
    }

    fn blank_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.rename = None;
        self.color_popup = None;
        window.focus(&self.list_focus);
        match ev.button {
            MouseButton::Left => {
                if platform::left_alt_pressed() {
                    self.marquee = None; // a stale band must not come back to life afterwards
                    self.press = None;
                    self.drag_window_later(cx);
                    return;
                }
                self.pending_paste = None;
                self.press = None;
                // A press on the scrollbar belongs to the scrollbar.
                let (track, max_off) = {
                    let st = self.scroll.0.borrow();
                    (st.base_handle.bounds(), st.base_handle.max_offset().height)
                };
                if max_off > px(0.) && ev.position.x >= track.origin.x + track.size.width - px(SCROLLBAR_W) {
                    return;
                }
                // Ctrl / Shift add to the selection; a plain press starts over.
                let additive = ev.modifiers.control || ev.modifiers.shift;
                let base = if additive { self.selected.clone() } else { BTreeSet::new() };
                if !additive {
                    self.selected.clear();
                }
                let (top, off) = {
                    let st = self.scroll.0.borrow();
                    (st.base_handle.bounds().origin.y, st.base_handle.offset().y)
                };
                let x = f32::from(ev.position.x);
                let y = f32::from(ev.position.y - top - off);
                self.marquee = Some(Marquee { x0: x, y0: y, x1: x, y1: y, base, active: false });
                if ev.click_count == 2 {
                    // double-click on empty space = one level up
                    self.go_up(window);
                }
            }
            MouseButton::Right => {
                self.selected.clear();
                // As on a row: the menu is deferred to mouse-up, so a hold-and-drag from here
                // moves the window instead (maybe_start_right_drag / finish_right_click).
                self.pending_right_click = Some(ev.clone());
            }
            _ => {}
        }
    }

    /// The pointer moved with the rubber band down: stretch it and select the rows it touches.
    fn marquee_update(&mut self, pos: Point<Pixels>) {
        let (top, off) = {
            let st = self.scroll.0.borrow();
            (st.base_handle.bounds().origin.y, st.base_handle.offset().y)
        };
        let Some(m) = self.marquee.as_mut() else { return };
        m.active = true;
        m.x1 = f32::from(pos.x);
        m.y1 = f32::from(pos.y - top - off);
        let (a, b) = (m.y0.min(m.y1), m.y0.max(m.y1));
        let mut hits = m.base.clone();
        if !self.entries.is_empty() {
            let first = (a / ROW_H).floor().max(0.) as usize;
            let last = ((b / ROW_H).floor().max(0.) as usize).min(self.entries.len() - 1);
            for i in first..=last {
                if self.entries.get(i).is_some_and(|e| !e.is_parent) {
                    hits.insert(i);
                }
            }
        }
        self.anchor = hits.iter().next().copied().or(self.anchor);
        self.selected = hits;
    }

    /// While the rubber band is held past the top or bottom of the list, keep scrolling.
    fn marquee_autoscroll(&mut self, window: &Window) -> bool {
        let pos = window.mouse_position();
        let (bounds, off, max_off) = {
            let st = self.scroll.0.borrow();
            (st.base_handle.bounds(), st.base_handle.offset().y, st.base_handle.max_offset().height)
        };
        let beyond = if pos.y < bounds.origin.y {
            f32::from(pos.y - bounds.origin.y)
        } else if pos.y > bounds.origin.y + bounds.size.height {
            f32::from(pos.y - bounds.origin.y - bounds.size.height)
        } else {
            return false;
        };
        let step = (beyond * 0.5).clamp(-ROW_H, ROW_H);
        let new_y = (f32::from(off) - step).clamp(-f32::from(max_off), 0.);
        if (new_y - f32::from(off)).abs() < 0.5 {
            return false;
        }
        self.scroll.0.borrow().base_handle.set_offset(gpui::point(px(0.), px(new_y)));
        self.marquee_update(pos);
        true
    }

    /// The button is down on a row and the pointer has wandered off: take the selected files out
    /// of the window with the system's drag-and-drop.
    fn maybe_start_file_drag(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        if ev.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let Some(p) = self.press.as_ref() else { return };
        let dx = f32::from(ev.position.x - p.at.x);
        let dy = f32::from(ev.position.y - p.at.y);
        if dx * dx + dy * dy < 25. {
            return;
        }
        let row = p.row;
        self.press = None;
        if !self.selected.contains(&row) {
            self.select_only(row);
        }
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let hwnd = self.hwnd;
        // Run it once GPUI has finished with this event: the drag has a message loop of its own.
        cx.defer(move |_| platform::drag_files_out(hwnd, &paths));
    }

    /// The button came up on a row without a drag having started: collapses a kept-together
    /// group back down to the one row, or starts the rename it was a candidate for.
    fn row_mouse_up(&mut self, window: &mut Window) {
        if let Some(p) = self.press.take() {
            if p.collapse {
                self.select_only(p.row);
            } else if p.rename_candidate {
                if let Some(path) = self.entries.get(p.row).map(|e| e.path.clone()) {
                    self.start_rename(&path, window);
                }
            }
        }
    }

    /// The right button is down (over a row or blank space). Once an in-progress hold-drag has
    /// moved the window far enough once, this just keeps it following the pointer on every
    /// later move. Otherwise it watches for the pointer to wander past the same threshold the
    /// left-button file drag uses and, the first time it does, starts the hold-drag: Windows'
    /// own move loop (`drag_window_later` / `start_window_drag`) only ends on the *left*
    /// button's release, so it can't be reused here — this moves the window by hand instead,
    /// tracking screen-position deltas between the cursor and the window from that moment on.
    /// Either way, clearing `pending_right_click` means the eventual mouse-up won't also pop
    /// the context menu it was waiting to show.
    fn maybe_start_right_drag(&mut self, ev: &MouseMoveEvent) {
        if ev.pressed_button != Some(MouseButton::Right) {
            return;
        }
        if let Some((cursor_x0, cursor_y0, win_x0, win_y0)) = self.right_drag {
            let (cx, cy) = platform::cursor_pos();
            platform::move_window(self.hwnd, win_x0 + (cx - cursor_x0), win_y0 + (cy - cursor_y0));
            return;
        }
        let Some(p) = self.pending_right_click.as_ref() else { return };
        let dx = f32::from(ev.position.x - p.position.x);
        let dy = f32::from(ev.position.y - p.position.y);
        if dx * dx + dy * dy < 25. {
            return;
        }
        self.pending_right_click = None;
        let (cursor_x0, cursor_y0) = platform::cursor_pos();
        let (win_x0, win_y0) = platform::window_pos(self.hwnd);
        self.right_drag = Some((cursor_x0, cursor_y0, win_x0, win_y0));
    }

    /// The right button came up: if a hold-drag was under way, it simply ends (the window
    /// stays where it landed); otherwise show the context menu the press was waiting for.
    fn finish_right_click(&mut self, window: &mut Window) {
        if self.right_drag.take().is_some() {
            return;
        }
        if let Some(ev) = self.pending_right_click.take() {
            self.show_context_menu(&ev, window);
        }
    }

    /// `at` is in screen pixels.
    /// Hands the window to the system's move loop once GPUI has finished the current update.
    /// Called from inside a listener, that loop would pump messages while this window is still
    /// checked out of GPUI, and every task that wanted it meanwhile would be turned away.
    fn drag_window_later(&self, cx: &mut Context<Self>) {
        let hwnd = self.hwnd;
        cx.defer(move |_| platform::start_window_drag(hwnd));
    }

    fn show_context_menu(&mut self, ev: &MouseDownEvent, window: &mut Window) {
        let sf = window.scale_factor();
        let x = (f32::from(ev.position.x) * sf) as i32;
        let y = (f32::from(ev.position.y) * sf) as i32;
        let at = platform::client_to_screen(self.hwnd, x, y);
        let targets = self.selected_paths();
        // "Paste" / "New" in the shell's own menu make files too.
        self.arm_paste_watch(20);
        // "Add/Remove Favorite" applies to the folder that was clicked, else to the open folder.
        let fav_target = match targets.as_slice() {
            [one] if one.is_dir() => Some(one.clone()),
            _ => self.cwd.clone(),
        };
        self.shell.context_menu(ContextMenuRequest {
            owner: self.hwnd,
            targets,
            folder: self.cwd.clone(),
            pt: at,
            fav_is_set: fav_target.as_deref().map(fsops::is_favorite).unwrap_or(false),
            fav_target,
            extended: ev.modifiers.shift,
        });
    }

    fn show_favorites_menu(&mut self, window: &mut Window) {
        let mut items = Vec::new();
        for d in platform::drives() {
            let text = if d.label.is_empty() { d.root.clone() } else { format!("{}  ({})", d.root, d.label) };
            items.push(MenuEntry::Item { text, path: PathBuf::from(&d.root) });
        }
        // A shortcut to the favorites.json file itself, so the user can edit it directly;
        // opens with its default app rather than being navigated into as a folder.
        items.push(MenuEntry::Separator);
        items.push(MenuEntry::OpenFile { text: "favorites.json".into(), path: fsops::favorites_file() });
        // Snapshot / restore every currently open window as a group, independent of the
        // regular session file that session.json tracks automatically.
        items.push(MenuEntry::Separator);
        items.push(MenuEntry::Action { text: "Update Session Group".into(), command: MenuCommand::UpdateSessionGroup });
        items.push(MenuEntry::Action { text: "Open Last Session Group".into(), command: MenuCommand::OpenLastSessionGroup });
        let mut favs = fsops::load_favorites();
        if !favs.is_empty() {
            favs.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
            items.push(MenuEntry::Separator);
            for f in favs {
                items.push(MenuEntry::Item { text: f.clone(), path: PathBuf::from(f) });
            }
        }
        // just under the Favorites button
        let sf = window.scale_factor();
        let win_w = f32::from(window.bounds().size.width);
        let x = (fav_button_left(win_w) * sf) as i32;
        let y = (ADDR_H * sf) as i32;
        self.shell.popup_list(self.hwnd, platform::client_to_screen(self.hwnd, x, y), items);
    }

    /// Right-click on the address bar: Copy / Paste / Paste and Go, at the click point.
    fn show_addr_menu(&mut self, ev: &MouseDownEvent, window: &mut Window) {
        let items = vec![
            MenuEntry::Action { text: "Copy".into(), command: MenuCommand::AddrCopy },
            MenuEntry::Action { text: "Paste".into(), command: MenuCommand::AddrPaste },
            MenuEntry::Action { text: "Paste and Go".into(), command: MenuCommand::AddrPasteAndGo },
        ];
        let sf = window.scale_factor();
        let x = (f32::from(ev.position.x) * sf) as i32;
        let y = (f32::from(ev.position.y) * sf) as i32;
        self.shell.popup_list(self.hwnd, platform::client_to_screen(self.hwnd, x, y), items);
    }

    /// An entry was picked from a popup menu's command list (the address bar's right-click
    /// menu, or the Favorites menu's "session group" buttons).
    fn handle_menu_command(&mut self, command: MenuCommand, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            MenuCommand::AddrCopy => cx.write_to_clipboard(ClipboardItem::new_string(self.addr.text.clone())),
            MenuCommand::AddrPaste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.addr_error = false;
                    self.addr.ime_replace(None, &text);
                    window.focus(&self.addr_focus);
                }
            }
            MenuCommand::AddrPasteAndGo => {
                if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                    self.addr_error = false;
                    self.addr.set_text(&text);
                    self.open_typed(window);
                }
            }
            MenuCommand::UpdateSessionGroup => fsops::update_session_group(),
            MenuCommand::OpenLastSessionGroup => fsops::open_session_group(),
        }
    }

    /// `shift` runs the verb as if Shift were held — Shift+Delete skips the Recycle Bin.
    fn shell_verb(&mut self, verb: &'static str, shift: bool) {
        let Some(folder) = self.cwd.clone() else { return };
        let targets = self.selected_paths();
        if targets.is_empty() {
            return;
        }
        self.shell.verb(self.hwnd, targets, folder, verb, shift);
    }

    fn start_rename(&mut self, path: &Path, window: &mut Window) {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return };
        let mut input = TextInput::new(&name);
        input.select_stem();
        self.rename = Some(Rename { path: path.to_path_buf(), input });
        window.focus(&self.rename_focus);
    }

    /// The byte offset in the rename box's text nearest to window x-position `x`.
    /// Shapes `text` the way the rename / address / colour boxes render it, so callers can map
    /// between a pixel x-position and a byte offset in either direction.
    fn shape_text(text: &str, window: &Window) -> gpui::ShapedLine {
        let run = gpui::TextRun {
            len: text.len(),
            font: gpui::font(UI_FONT),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        window.text_system().shape_line(SharedString::from(text.to_string()), px(FONT_PX), &[run], None)
    }

    fn rename_caret_at(&self, x: Pixels, window: &Window) -> Option<usize> {
        let r = self.rename.as_ref()?;
        let b = self.rename_bounds.get();
        // border (1) + padding (3) sit between the box's edge and its text
        let rel = (x - b.origin.x - px(4.)).max(px(0.));
        Some(Self::shape_text(&r.input.text, window).closest_index_for_x(rel))
    }

    // ───────────────────────── platform text input (IME) ─────────────────────────
    // Exactly one of the rename / address / colour-hex boxes can hold keyboard focus at a
    // time, and GPUI's `EntityInputHandler` is implemented once per view type, not once per
    // element — so these pick out whichever box is currently focused for the impl below.

    fn active_input(&self, window: &Window) -> Option<&TextInput> {
        if self.rename_focus.is_focused(window) {
            self.rename.as_ref().map(|r| &r.input)
        } else if self.color_focus.is_focused(window) {
            self.color_popup.as_ref().map(|p| &p.input)
        } else if self.addr_focus.is_focused(window) {
            Some(&self.addr)
        } else {
            None
        }
    }

    fn active_input_mut(&mut self, window: &Window) -> Option<&mut TextInput> {
        if self.rename_focus.is_focused(window) {
            self.rename.as_mut().map(|r| &mut r.input)
        } else if self.color_focus.is_focused(window) {
            self.color_popup.as_mut().map(|p| &mut p.input)
        } else if self.addr_focus.is_focused(window) {
            Some(&mut self.addr)
        } else {
            None
        }
    }

    fn active_bounds(&self, window: &Window) -> Bounds<Pixels> {
        if self.rename_focus.is_focused(window) {
            self.rename_bounds.get()
        } else if self.color_focus.is_focused(window) {
            self.color_bounds.get()
        } else {
            self.addr_bounds.get()
        }
    }

    fn commit_rename(&mut self, window: &mut Window) {
        let Some(r) = self.rename.take() else { return };
        let new_name = r.input.text.trim().to_string();
        let old_name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let valid = !new_name.is_empty() && !new_name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']);
        if valid && new_name != old_name {
            if let Some(parent) = r.path.parent() {
                if std::fs::rename(&r.path, parent.join(&new_name)).is_ok() {
                    self.reload(window);
                    if let Some(i) = self.entries.iter().position(|e| e.name == new_name) {
                        self.select_only(i);
                        self.scroll.scroll_to_item(i, ScrollStrategy::Center);
                    }
                }
            }
        }
        window.focus(&self.list_focus);
    }

    // ───────────────────────── title colour ─────────────────────────

    fn apply_color(&mut self, c: Option<u32>, window: &mut Window) {
        self.title_color = c;
        platform::set_title_color(self.hwnd, c);
        self.overlay_ok = false;
        self.overlay_tries = 0;
        self.color_popup = None;
        window.focus(&self.list_focus);
    }

    /// OK in the colour popup. An empty box clears the colour back to the Windows default.
    fn apply_typed_color(&mut self, window: &mut Window) {
        let Some(p) = self.color_popup.as_mut() else { return };
        if p.input.text.trim().is_empty() {
            self.apply_color(None, window);
            return;
        }
        match parse_hex(&p.input.text) {
            Some(c) => self.apply_color(Some(c), window),
            None => p.error = true,
        }
    }

    // ───────────────────────── periodic work ─────────────────────────

    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut redraw = false;

        while let Ok(ev) = self.shell_rx.try_recv() {
            redraw = true;
            match ev {
                ShellEvent::ToggleFavorite(p) => fsops::toggle_favorite(&p),
                ShellEvent::OpenFolder(p) => {
                    self.navigate(Some(p), None, window);
                }
                ShellEvent::Navigate(p) => {
                    self.navigate(Some(p), None, window);
                }
                ShellEvent::ReloadNow => self.reload(window),
                ShellEvent::NewWindow => Self::new_window(&self.cwd),
                // The global hotkey opens a fresh window at My Computer, like Win+E does.
                ShellEvent::HotkeyNewWindow => Self::new_window(&None),
                ShellEvent::Rename(p) => self.start_rename(&p, window),
                ShellEvent::ColorPicked(c) => {
                    if let Some(p) = self.color_popup.as_mut() {
                        p.picking = false;
                        p.error = false;
                        p.input.set_text(&hex_string(c));
                    }
                    self.apply_color(Some(c), window);
                }
                ShellEvent::ColorPickCancelled => {
                    if let Some(p) = self.color_popup.as_mut() {
                        p.picking = false;
                    }
                }
                ShellEvent::DragHover(over) => self.drag_hover = over,
                ShellEvent::Arrived(names) => {
                    self.reload(window);
                    self.select_names(&names);
                    self.settle_until = Some(Instant::now() + Duration::from_secs(3));
                }
                ShellEvent::Refresh => {
                    self.dirty_since = Some(Instant::now());
                    self.settle_until = Some(Instant::now() + Duration::from_secs(3));
                }
                ShellEvent::MenuCommand(command) => self.handle_menu_command(command, window, cx),
            }
        }

        let mut changed = false;
        while self.watch_rx.try_recv().is_ok() {
            changed = true;
        }
        if changed && self.dirty_since.is_none() {
            self.dirty_since = Some(Instant::now());
        }
        if let Some(t) = self.dirty_since {
            if t.elapsed() >= Duration::from_millis(120) {
                self.dirty_since = None;
                self.reload(window);
                redraw = true;
            }
        }

        // The watcher misses things often enough to be worth backing up: a shell command does its
        // work after returning, a network drive may report nothing at all, and the folder can
        // change while this window sits in the background.
        let active = window.is_window_active();
        if !active && self.was_active && self.rename.is_some() {
            // Clicking away from an edit commits it in Explorer; an edit left hanging would keep
            // taking the keys meant for the list.
            self.commit_rename(window);
            redraw = true;
        }
        if active && !self.was_active {
            self.reload(window); // bring it up to date the moment the window is looked at again
            redraw = true;
        }
        self.was_active = active;

        let settling = self.settle_until.is_some_and(|t| Instant::now() < t);
        if !settling {
            self.settle_until = None;
        }
        let poll_every = if active || settling { Duration::from_millis(500) } else { Duration::from_secs(5) };
        if self.last_poll.elapsed() >= poll_every {
            self.last_poll = Instant::now();
            // One syscall: a folder's own timestamp moves whenever an entry is added, removed or
            // renamed, so a full re-listing is only needed when it has.
            let stamp = self.cwd.as_ref().and_then(|d| std::fs::metadata(d).ok()).and_then(|m| m.modified().ok());
            let moved = stamp != self.last_dir_stamp;
            self.last_dir_stamp = stamp;
            // Last resort for what neither the watcher nor the timestamp reports — a file's own
            // contents changing, say — without re-reading a large folder every tick.
            let overdue = active && self.last_reload.elapsed() >= Duration::from_secs(6);
            if moved || settling || overdue {
                self.reload(window);
                redraw = true;
            }
        }

        if self.marquee.as_ref().is_some_and(|m| m.active) {
            if cx.has_active_drag() {
                if self.marquee_autoscroll(window) {
                    redraw = true;
                }
            } else {
                self.marquee = None; // the button came up
                redraw = true;
            }
        }

        // The taskbar button may not exist yet at start-up: retry for a few seconds.
        if !self.overlay_ok && self.overlay_tries < 60 && self.hwnd != 0 {
            self.overlay_tries += 1;
            self.overlay_ok = platform::set_overlay_number(self.hwnd, self.instance, self.title_color.unwrap_or(DEFAULT_BADGE));
        }

        // A move or resize is written to the session file once the window has been still for a
        // moment, not on every frame of the drag: each change restarts the settle timer, so it
        // only fires after the user actually lets go.
        let b = Self::bounds_tuple(window);
        if b != self.last_seen_bounds {
            self.last_seen_bounds = b;
            self.bounds_settle_since = Some(Instant::now());
        }
        if let Some(t) = self.bounds_settle_since {
            if t.elapsed() >= Duration::from_millis(300) {
                self.bounds_settle_since = None;
                if self.last_seen_bounds != self.last_saved_bounds {
                    self.last_saved_bounds = self.last_seen_bounds;
                    fsops::save_window_state(self.instance, self.cwd.as_deref(), self.last_saved_bounds);
                }
            }
        }

        // caret blink + the 30-minute highlight ageing out
        if redraw || self.last_redraw.elapsed() >= Duration::from_millis(500) {
            self.last_redraw = Instant::now();
            cx.notify();
        }
    }

    // ───────────────────────── key handling ─────────────────────────

    /// The clipboard is only read for Ctrl+V — reading it on every keystroke is needless work.
    fn paste_text(ev: &KeyDownEvent, cx: &mut Context<Self>) -> Option<String> {
        let m = ev.keystroke.modifiers;
        if (m.control || m.platform) && ev.keystroke.key == "v" {
            cx.read_from_clipboard().and_then(|c| c.text())
        } else {
            None
        }
    }

    /// Returns whether the key was ours to swallow. `false` (only for `Outcome::PassThrough`)
    /// means the caller must NOT call `cx.stop_propagation()`: the platform still needs to
    /// turn the keystroke into `WM_CHAR` / IME composition, which is how plain typing, the
    /// space bar and Chinese/Japanese/Korean input actually reach the text box (see the
    /// `EntityInputHandler` impl below).
    fn addr_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let paste = Self::paste_text(ev, cx);
        self.addr_error = false;
        match self.addr.handle_key(ev, paste) {
            Outcome::Enter => self.open_typed(window),
            Outcome::Escape => {
                self.addr.set_text(&Self::loc_address(&self.cwd));
                window.focus(&self.list_focus);
            }
            Outcome::Copy(s) => cx.write_to_clipboard(ClipboardItem::new_string(s)),
            Outcome::Changed | Outcome::None => {}
            Outcome::PassThrough => return false,
        }
        true
    }

    fn rename_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let paste = Self::paste_text(ev, cx);
        let Some(r) = self.rename.as_mut() else { return true };
        match r.input.handle_key(ev, paste) {
            Outcome::Enter => self.commit_rename(window),
            Outcome::Escape => {
                self.rename = None;
                window.focus(&self.list_focus);
            }
            Outcome::Copy(s) => cx.write_to_clipboard(ClipboardItem::new_string(s)),
            Outcome::Changed | Outcome::None => {}
            Outcome::PassThrough => return false,
        }
        true
    }

    fn color_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let paste = Self::paste_text(ev, cx);
        let Some(p) = self.color_popup.as_mut() else { return true };
        p.error = false;
        match p.input.handle_key(ev, paste) {
            Outcome::Enter => self.apply_typed_color(window),
            Outcome::Escape => {
                self.color_popup = None;
                window.focus(&self.list_focus);
            }
            Outcome::Copy(s) => cx.write_to_clipboard(ClipboardItem::new_string(s)),
            Outcome::Changed | Outcome::None => {}
            Outcome::PassThrough => return false,
        }
        true
    }

    fn list_key(&mut self, ev: &KeyDownEvent, window: &mut Window) {
        let m = ev.keystroke.modifiers;
        let ctrl = m.control;
        match ev.keystroke.key.as_str() {
            "c" if ctrl => self.shell_verb("copy", false),
            "x" if ctrl => self.shell_verb("cut", false),
            "v" if ctrl => {
                if let Some(folder) = self.cwd.clone() {
                    self.arm_paste_watch(15);
                    self.shell.verb(self.hwnd, Vec::new(), folder, "paste", false);
                }
            }
            "n" if ctrl => self.new_folder(window),
            "m" if ctrl => self.new_file("New Markdown", ".md", b"", window),
            "t" if ctrl => self.new_file("New Text Document", ".txt", b"", window),
            "e" if ctrl => self.new_file("New Workbook", ".xlsx", EMPTY_XLSX, window),
            // Explorer's shortcuts for "edit the address"
            "l" if ctrl => self.focus_address(window),
            "f4" => self.focus_address(window),
            "a" if ctrl => {
                self.selected =
                    self.entries.iter().enumerate().filter(|(_, e)| !e.is_parent).map(|(i, _)| i).collect()
            }
            // Delete → Recycle Bin, Shift+Delete → gone for good; Windows asks either way.
            "delete" => self.shell_verb("delete", m.shift),
            "f2" => {
                if let Some(p) = self.selected_paths().first().cloned() {
                    self.start_rename(&p, window);
                }
            }
            "f5" => self.reload(window),
            "backspace" => self.go_up(window),
            "escape" => self.selected.clear(),
            "enter" => {
                let sel: Vec<usize> = self.selected.iter().copied().collect();
                match sel.as_slice() {
                    [] => {}
                    [one] => self.activate(*one, window),
                    many => {
                        for i in many {
                            if let Some(e) = self.entries.get(*i) {
                                if !e.is_dir {
                                    platform::open_path(&e.path);
                                }
                            }
                        }
                    }
                }
            }
            "left" if m.alt => self.go_back(window),
            "right" if m.alt => self.go_forward(window),
            "up" => self.move_selection(-1, m.shift),
            "down" => self.move_selection(1, m.shift),
            "pageup" => self.move_selection(-10, m.shift),
            "pagedown" => self.move_selection(10, m.shift),
            "home" => self.move_selection(-(self.entries.len() as isize), m.shift),
            "end" => self.move_selection(self.entries.len() as isize, m.shift),
            _ => {}
        }
    }

    // ───────────────────────── rendering ─────────────────────────

    fn caret_on() -> bool {
        (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) / 530) % 2 == 0
    }

    /// A square toolbar button holding one of `ui_icons`.
    fn icon_button(&self, id: &'static str, icon: usize, side: f32) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_none()
            .size(px(side))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(0xFBFBFB))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(HOVER_BG)).border_color(rgb(SEL_BORDER)))
            .child(img(self.ui_icons[icon].clone()).size(px(side * 0.57)))
    }

    fn button(id: &'static str, label: &'static str, w: f32) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .flex_none()
            .w(px(w))
            .h(px(CTRL_H))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(0xFBFBFB))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(HOVER_BG)).border_color(rgb(SEL_BORDER)))
            .child(label)
    }

    fn render_address_bar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.addr_focus.is_focused(window);
        let show_address = visible_columns(window).2;
        let border = if self.addr_error {
            ERROR
        } else if focused {
            ACCENT
        } else {
            BORDER
        };
        // With no colour set the button shows the default colour itself, matching the first
        // swatch in the popup and the number drawn behind the list.
        let swatch = self.title_color.unwrap_or(DEFAULT_COLOR);

        let color_btn = div()
            .id("color-btn")
            .flex_none()
            .size(px(CTRL_H))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(0xFBFBFB))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(HOVER_BG)).border_color(rgb(SEL_BORDER)))
            .child(div().size(px(16.)).rounded(px(3.)).border_1().border_color(rgb(0x808080)).bg(rgb(swatch)))
            .on_click(cx.listener(|this, _, window, cx| {
                this.color_popup = match this.color_popup {
                    Some(_) => None,
                    None => {
                        let mut input = TextInput::new(&hex_string(this.title_color.unwrap_or(DEFAULT_COLOR)));
                        input.select_all();
                        Some(ColorPopup { input, error: false, picking: false })
                    }
                };
                if this.color_popup.is_some() {
                    window.focus(&this.color_focus);
                }
                cx.notify();
            }));

        let addr_box = visible_columns(window).2.then(|| div()
            .id("addr")
            .flex_1()
            .min_w(px(0.))
            .h(px(CTRL_H))
            .px(px(8.))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(4.))
            .border_1()
            .border_color(rgb(border))
            .bg(rgb(0xFFFFFF))
            .cursor_text()
            .track_focus(&self.addr_focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.rename = None;
                    this.color_popup = None;
                    if !this.addr_focus.is_focused(window) {
                        window.focus(&this.addr_focus);
                        this.addr.select_all();
                    }
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.rename = None;
                    this.color_popup = None;
                    if !this.addr_focus.is_focused(window) {
                        window.focus(&this.addr_focus);
                    }
                    this.show_addr_menu(ev, window);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if this.addr_key(ev, window, cx) {
                    cx.stop_propagation();
                }
                cx.notify();
            }))
            .relative()
            .child({
                let cell = self.addr_bounds.clone();
                let entity = cx.entity();
                let focus = self.addr_focus.clone();
                // See the matching comment on the rename box's canvas: this is what lets the
                // address bar accept IME composition and the space bar (folder names with
                // spaces included).
                gpui::canvas(
                    move |bounds, _, _| cell.set(bounds),
                    move |bounds, _, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity), &*cx);
                    },
                )
                .absolute()
                .size_full()
            })
            .child(self.addr.render(focused, Self::caret_on(), TEXT)));

        // Open = the system folder picker; what it returns is opened in the list below.
        let open_btn = self.icon_button("open-btn", ICO_OPEN, CTRL_H).on_click(cx.listener(|this, _, _, cx| {
            this.shell.pick_folder(this.hwnd, this.cwd.clone());
            cx.notify();
        }));
        let fav_btn = self.icon_button("fav-btn", ICO_FAV, CTRL_H).on_click(cx.listener(|this, _, window, cx| {
            this.show_favorites_menu(window);
            cx.notify();
        }));
        let new_btn = self.icon_button("new-btn", ICO_NEW, CTRL_H).on_click(cx.listener(|this, _, _, cx| {
            Self::new_window(&this.cwd);
            cx.notify();
        }));
        // one level up — same as a double-click on the blank area of the list
        let up_btn = self.icon_button("up-btn", ICO_UP, CTRL_H).on_click(cx.listener(|this, _, window, cx| {
            this.go_up(window);
            cx.notify();
        }));

        div()
            .flex_none()
            .h(px(ADDR_H))
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.))
            .bg(rgb(0xFFFFFF))
            .child(color_btn)
            .children(addr_box)
            // With no address box the buttons would bunch at the left, so push them right.
            .when(!show_address, |d| d.child(div().flex_1()))
            .child(open_btn)
            .child(fav_btn)
            .child(new_btn)
            .child(up_btn)
    }

    fn header_cell(&self, id: &'static str, label: &'static str, key: SortKey, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let arrow = if self.sort_key == key {
            if self.sort_asc {
                " \u{25B2}"
            } else {
                " \u{25BC}"
            }
        } else {
            ""
        };
        div()
            .id(id)
            .h_full()
            .flex()
            .items_center()
            .px(px(6.))
            .text_color(rgb(HEADER_TEXT))
            .whitespace_nowrap()
            .hover(|s| s.bg(rgb(HOVER_BG)))
            .child(format!("{label}{arrow}"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.sort_by(key);
                cx.notify();
            }))
    }

    fn render_header(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (show_size, show_date, _) = visible_columns(window);
        let name = self.header_cell("h-name", "Name", SortKey::Name, cx).flex_1().min_w(px(0.)).pl(px(31.));
        let size = show_size
            .then(|| self.header_cell("h-size", "Size", SortKey::Size, cx).flex_none().w(px(SIZE_W)).justify_end());
        let date = show_date
            .then(|| self.header_cell("h-date", "Date modified", SortKey::Modified, cx).flex_none().w(px(DATE_W)));
        div()
            .flex_none()
            .h(px(HEADER_H))
            .px(px(6.))
            .flex()
            .flex_row()
            .items_center()
            .border_b_1()
            .border_color(rgb(LINE))
            .bg(rgb(0xFFFFFF))
            .text_size(px(FONT_PX))
            .child(name)
            .children(size)
            .children(date)
    }

    fn rows(&mut self, range: std::ops::Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let caret_on = Self::caret_on();
        let (show_size, show_date, _) = visible_columns(window);
        let renaming = self.rename.as_ref().map(|r| r.path.clone());
        let mut out = Vec::with_capacity(range.len());
        for i in range {
            // Trailing blank rows: clicks fall through to the list's own blank-area handler.
            let Some(e) = self.entries.get(i).cloned() else {
                out.push(div().w_full().h(px(ROW_H)).into_any_element());
                continue;
            };
            let selected = self.selected.contains(&i);
            let recent = fsops::is_recent(e.modified);
            // The "changed in the last 30 minutes" mark sits behind the name alone, so the row
            // itself stays free to show the selection. Selected cells get the same solid accent
            // fill (and white text) Explorer uses for a focused selection; hovering a selected
            // row keeps that same fill (hover_bg == bg here) rather than fading to a lighter
            // tint, so the blue/white look doesn't flicker as the pointer passes over it.
            let (bg, hover_bg) = if selected { (Some(ACCENT), ACCENT) } else { (None, HOVER_BG) };
            let fresh = recent && !selected;
            let icon = self.icons.get(&e);

            let icon_el = div().flex_none().size(px(16.)).when_some(icon, |d, im| d.child(img(im).size(px(16.))));
            let name_el: AnyElement = match (&renaming, self.rename.as_ref()) {
                (Some(p), Some(r)) if *p == e.path => div()
                    .id("rename-box")
                    .flex_1()
                    .min_w(px(0.))
                    .h(px(ROW_H - 6.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .border_1()
                    .border_color(rgb(ACCENT))
                    .bg(rgb(0xFFFFFF))
                    .track_focus(&self.rename_focus)
                    .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                        if this.rename_key(ev, window, cx) {
                            cx.stop_propagation();
                        }
                        cx.notify();
                    }))
                    .relative()
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            let at = this.rename_caret_at(ev.position.x, window);
                            if let (Some(r), Some(at)) = (this.rename.as_mut(), at) {
                                if ev.click_count >= 2 {
                                    r.input.select_all();
                                } else {
                                    r.input.press_at(at);
                                }
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                        if ev.pressed_button == Some(MouseButton::Left) {
                            if let Some(at) = this.rename_caret_at(ev.position.x, window) {
                                if let Some(r) = this.rename.as_mut() {
                                    r.input.drag_to(at);
                                }
                                cx.notify();
                            }
                        }
                    }))
                    .child({
                        let cell = self.rename_bounds.clone();
                        let entity = cx.entity();
                        let focus = self.rename_focus.clone();
                        // Registering the input handler here (during paint) is what makes
                        // Chinese/Japanese/Korean IME composition and the space bar work in
                        // this box: GPUI's Windows backend delivers all typed/composed text
                        // exclusively through it, never through `KeyDownEvent.key_char` for
                        // this box's purposes. See `EntityInputHandler for MiniExpo` below.
                        gpui::canvas(
                            move |bounds, _, _| cell.set(bounds),
                            move |bounds, _, window, cx| {
                                window.handle_input(&focus, ElementInputHandler::new(bounds, entity), &*cx);
                            },
                        )
                        .absolute()
                        .size_full()
                    })
                    .child(r.input.render(self.rename_focus.is_focused(window), caret_on, TEXT))
                    .into_any_element(),
                // No flex_1 here: the name is content-sized, so the green only covers the text,
                // and min_w(0) + truncate still lets it shrink and ellipsize in a narrow window.
                _ => div()
                    .min_w(px(0.))
                    .truncate()
                    .px(px(3.))
                    .rounded(px(3.))
                    .when(fresh, |d| d.bg(rgb(RECENT_BG)).text_color(rgb(RECENT_TEXT)))
                    .child(SharedString::from(e.name.clone()))
                    .into_any_element(),
            };

            let is_renaming_this = renaming.as_ref() == Some(&e.path);
            // Only the icon + visible name selects this row. While not renaming, that pair sits
            // in its own content-sized box with the actual click handlers (and stops the click
            // from bubbling); the blank space after it — still inside the flex_1 name column, as
            // well as the size/date columns — carries no handler of its own, so a click there
            // falls through untouched to the list's blank-area handler: nothing gets selected,
            // and a double-click there still goes up a level. While renaming, the rename box
            // itself already fills the whole name column and handles its own clicks, so it needs
            // no extra wrapping here.
            let name_col = if is_renaming_this {
                div().flex_1().min_w(px(0.)).flex().flex_row().items_center().gap(px(6.)).child(icon_el).child(name_el)
            } else {
                div().flex_1().min_w(px(0.)).flex().flex_row().items_center().child(
                    div()
                        .id(("row-hit", i))
                        .flex_initial()
                        .min_w(px(0.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        // A little slack past the name's own right edge, so a click that lands
                        // just to the right of a short name still counts as hitting it, rather
                        // than needing to land on the text pixel-exactly.
                        .pr(px(20.))
                        .when_some(bg, |d, c| d.bg(rgb(c)))
                        // The hover tint only covers this box (icon + name + the same margin
                        // used for hit-testing), not the rest of the row: hovering the blank
                        // middle or the Size/Date cells shows nothing.
                        .hover(move |s| s.bg(rgb(hover_bg)))
                        .child(icon_el)
                        .child(name_el)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.row_mouse_down(i, ev, window, cx);
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.row_mouse_down(i, ev, window, cx);
                                cx.notify();
                            }),
                        ),
                )
            };

            let row = div()
                .id(("row", i))
                .w_full()
                .h(px(ROW_H))
                .px(px(6.))
                .flex()
                .flex_row()
                .items_center()
                .border_1()
                // Selected rows keep a border around the whole row, but only faded (20 % alpha)
                // across the blank middle; the name / size / date cells carry the real fill
                // below, so the border there mostly just reinforces their own edge.
                .border_color(if selected { gpui::rgba((SEL_BORDER << 8) | 0x33).into() } else { gpui::transparent_black() })
                .text_color(if selected { rgb(0xFFFFFF) } else { rgb(TEXT) })
                .child(name_col)
                .children(show_size.then(|| {
                    div()
                        .flex_none()
                        .w(px(SIZE_W))
                        .px(px(6.))
                        .flex()
                        .justify_end()
                        .whitespace_nowrap()
                        .when_some(bg, |d, c| d.bg(rgb(c)))
                        .child(if e.is_dir { String::new() } else { fsops::format_size(e.size) })
                }))
                .children(show_date.then(|| {
                    div()
                        .flex_none()
                        .w(px(DATE_W))
                        .px(px(6.))
                        .whitespace_nowrap()
                        .when_some(bg, |d, c| d.bg(rgb(c)))
                        .child(fsops::format_time(e.modified))
                }));
            out.push(row.into_any_element());
        }
        out
    }

    /// The window's number, drawn very faintly behind the file list so one glance tells the
    /// windows apart. It takes the window's own colour and scales with the window.
    fn render_watermark(&self, window: &Window) -> impl IntoElement {
        let label: SharedString = self.instance.to_string().into();
        let digits = label.chars().count().max(1) as f32;
        let bounds = window.bounds().size;
        let w = f32::from(bounds.width).max(1.);
        let h = (f32::from(bounds.height) - ADDR_H - HEADER_H - STATUS_H).max(1.);
        // Half the height of the list, but never wider than ~70 % of it; 0.62 em is about the
        // width of one digit.
        let by_height = h * 0.5;
        let by_width = w * 0.7 / (digits * 0.62);
        let size = by_height.min(by_width).clamp(40., 1200.);
        // Washed out enough to read file names straight through it. Pale presets need more of
        // themselves than dark ones to show up at all against the white background, so the
        // transparency follows the colour's brightness.
        let c = self.title_color.unwrap_or(DEFAULT_COLOR);
        let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
        let luma = (299 * r + 587 * g + 114 * b) / 1000;
        let alpha = WATERMARK_ALPHA_MIN + (WATERMARK_ALPHA_MAX - WATERMARK_ALPHA_MIN) * luma / 255;
        let color = gpui::rgba((c << 8) | alpha);

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .child(
                div()
                    .text_size(px(size))
                    .line_height(px(size))
                    .font_weight(FontWeight::BOLD)
                    .text_color(color)
                    .child(label),
            )
    }

    /// Moves the vertical scroll offset so the thumb sits under `pointer_y` — used for both a
    /// plain click on the track (jump there) and every move while the thumb is being dragged.
    /// Reads geometry straight off the scroll handle rather than the scrollbar's own element
    /// bounds, so a click and a drag that starts moving instantly agree on the same numbers.
    fn scrollbar_seek(&mut self, pointer_y: Pixels) {
        let (track, viewport_h, max_off) = {
            let state = self.scroll.0.borrow();
            (state.base_handle.bounds(), state.last_item_size.map(|s| s.item.height), state.base_handle.max_offset().height)
        };
        if max_off <= px(0.) {
            return;
        }
        let viewport_h = viewport_h.unwrap_or(track.size.height);
        let content_h = viewport_h + max_off;
        let thumb_h = (track.size.height * (viewport_h / content_h)).max(px(SCROLLBAR_MIN_THUMB)).min(track.size.height);
        let usable = (track.size.height - thumb_h).max(px(1.));
        let rel = (pointer_y - track.origin.y - thumb_h * 0.5).max(px(0.)).min(usable);
        let frac = rel / usable;
        self.scroll.0.borrow().base_handle.set_offset(gpui::point(px(0.), -(max_off * frac)));
    }

    /// A thin draggable gutter along the right edge of the list, drawn over it rather than
    /// reserving layout space. Hidden entirely when everything already fits.
    fn render_scrollbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let (track, viewport_h, max_off) = {
            let state = self.scroll.0.borrow();
            (state.base_handle.bounds(), state.last_item_size.map(|s| s.item.height), state.base_handle.max_offset().height)
        };
        if max_off <= px(0.) || track.size.height <= px(0.) {
            return div().into_any_element();
        }
        let viewport_h = viewport_h.unwrap_or(track.size.height);
        let content_h = viewport_h + max_off;
        let thumb_h = (track.size.height * (viewport_h / content_h)).max(px(SCROLLBAR_MIN_THUMB)).min(track.size.height);
        let usable = (track.size.height - thumb_h).max(px(1.));
        let offset = self.scroll.0.borrow().base_handle.offset().y;
        let frac = (-offset / max_off).clamp(0., 1.);
        let thumb_top = usable * frac;

        div()
            .id("scrollbar-track")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(SCROLLBAR_W))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, _window, cx| {
                    this.scrollbar_seek(ev.position.y);
                    cx.notify();
                }),
            )
            .on_drag((), |_, _offset, _window, cx| cx.new(|_| ScrollDragGhost))
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<()>, _window, cx| {
                this.scrollbar_seek(ev.event.position.y);
                cx.notify();
            }))
            .child(
                div()
                    .absolute()
                    .top(thumb_top)
                    .right_0()
                    .w(px(SCROLLBAR_W - 3.))
                    .h(thumb_h)
                    .rounded(px((SCROLLBAR_W - 3.) / 2.))
                    .bg(gpui::rgba(0x00000055)),
            )
            .into_any_element()
    }

    fn render_list(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("list-area")
            .flex_1()
            .min_h(px(0.))
            .relative()
            .bg(rgb(0xFFFFFF))
            .track_focus(&self.list_focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                this.list_key(ev, window);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.blank_mouse_down(ev, window, cx);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    this.blank_mouse_down(ev, window, cx);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    this.row_mouse_up(window);
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    this.finish_right_click(window);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                this.maybe_start_file_drag(ev, cx);
                this.maybe_start_right_drag(ev);
            }))
            .on_drag(MarqueeDrag, |_, _offset, _window, cx| cx.new(|_| ScrollDragGhost))
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<MarqueeDrag>, _window, cx| {
                this.marquee_update(ev.event.position);
                cx.notify();
            }))
            .when(self.drag_hover, |d| d.bg(rgb(HOVER_BG)))
            .child(self.render_watermark(window))
            .child(
                uniform_list(
                    "rows",
                    self.entries.len() + TAIL_ROWS,
                    cx.processor(|this, range, window, cx| this.rows(range, window, cx)),
                )
                .track_scroll(self.scroll.clone())
                .size_full(),
            )
            .children(self.render_marquee(cx))
            .child(self.render_scrollbar(cx))
    }

    /// The rubber band itself: a translucent blue box over the rows, clipped to the list.
    fn render_marquee(&self, cx: &App) -> Option<AnyElement> {
        let m = self.marquee.as_ref().filter(|m| m.active && cx.has_active_drag())?;
        let (bounds, off) = {
            let st = self.scroll.0.borrow();
            (st.base_handle.bounds(), f32::from(st.base_handle.offset().y))
        };
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let ox = f32::from(bounds.origin.x);
        let left = (m.x0.min(m.x1) - ox).clamp(0., w);
        let right = (m.x0.max(m.x1) - ox).clamp(0., w);
        let top = (m.y0.min(m.y1) + off).clamp(0., h);
        let bottom = (m.y0.max(m.y1) + off).clamp(0., h);
        Some(
            div()
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px((right - left).max(0.)))
                .h(px((bottom - top).max(0.)))
                .border_1()
                .border_color(rgb(ACCENT))
                .bg(gpui::rgba(0x0078D440))
                .into_any_element(),
        )
    }

    /// The bar along the bottom: what is in the folder on the left, window-size buttons on the
    /// right. Dragging its empty part moves the window, like the title bar.
    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let items = self.entries.iter().filter(|e| !e.is_parent).count();
        let chosen = self.selected.iter().filter_map(|i| self.entries.get(*i)).filter(|e| !e.is_parent).count();
        let text = match (items, chosen) {
            (_, 0) => format!("{items} items"),
            _ => format!("{items} items, {chosen} selected"),
        };

        // A click on a button must not also start dragging the window.
        let button = |this: &Self, id: &'static str, icon: usize| {
            this.icon_button(id, icon, STATUS_BTN)
                .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()))
        };
        let bigger = button(self, "win-bigger", ICO_BIGGER).on_click(cx.listener(|this, _, window, cx| {
            this.scale_window(1.25, window, cx);
            cx.notify();
        }));
        let smaller = button(self, "win-smaller", ICO_SMALLER).on_click(cx.listener(|this, _, window, cx| {
            this.scale_window(0.75, window, cx);
            cx.notify();
        }));
        let reset = button(self, "win-reset", ICO_RESET).on_click(cx.listener(|this, _, window, cx| {
            this.size_window(WIN_W, WIN_H, window, cx);
            cx.notify();
        }));

        div()
            .flex_none()
            .h(px(STATUS_H))
            .px(px(8.))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(4.))
            .border_t_1()
            .border_color(rgb(LINE))
            .bg(rgb(0xFAFAFA))
            .text_color(rgb(HEADER_TEXT))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| this.drag_window_later(cx)),
            )
            .child(div().flex_1().min_w(px(0.)).truncate().child(text))
            .child(bigger)
            .child(smaller)
            .child(reset)
    }

    fn render_color_popup(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.color_popup.as_ref()?;
        let focused = self.color_focus.is_focused(window);

        let input_box = div()
            .flex_1()
            .min_w(px(0.))
            .h(px(CTRL_H))
            .px(px(8.))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(4.))
            .border_1()
            .border_color(rgb(if p.error { ERROR } else if focused { ACCENT } else { BORDER }))
            .bg(rgb(0xFFFFFF))
            .cursor_text()
            .relative()
            .child({
                let cell = self.color_bounds.clone();
                let entity = cx.entity();
                let focus = self.color_focus.clone();
                // See the matching comment on the rename box's canvas.
                gpui::canvas(
                    move |bounds, _, _| cell.set(bounds),
                    move |bounds, _, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity), &*cx);
                    },
                )
                .absolute()
                .size_full()
            })
            .child(p.input.render(focused, Self::caret_on(), TEXT));

        let ok = Self::button("color-ok", "OK", 44.).on_click(cx.listener(|this, _, window, cx| {
            this.apply_typed_color(window);
            cx.notify();
        }));
        // Eyedropper: freezes the screen and picks the colour under the next click, anywhere.
        let pick = div()
            .id("color-pick")
            .flex_none()
            .size(px(CTRL_H))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .border_color(rgb(if p.picking { ACCENT } else { BORDER }))
            .bg(rgb(if p.picking { HOVER_BG } else { 0xFBFBFB }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(HOVER_BG)).border_color(rgb(SEL_BORDER)))
            .child(img(self.ui_icons[ICO_PICK].clone()).size(px(16.)))
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(p) = this.color_popup.as_mut() {
                    p.picking = true;
                }
                this.shell.pick_color(this.hwnd);
                cx.notify();
            }));

        // Fixed rows of five rather than wrapping, so the grid is the same at any window size.
        let swatch_rows = PRESET_COLORS.chunks(SWATCH_COLS).enumerate().map(|(row, colors)| {
            let cells = colors.iter().enumerate().map(|(col, c)| {
                let c = *c;
                let index = row * SWATCH_COLS + col;
                // Slot 0 is "back to the Windows default", so it is lit when no colour is set.
                let is_default = index == 0;
                let selected = if is_default { self.title_color.is_none() } else { self.title_color == Some(c) };
                div()
                    .id(("preset", index))
                    .size(px(SWATCH))
                    .rounded(px(4.))
                    .border_2()
                    .border_color(rgb(if selected { ACCENT } else { 0x9A9A9A }))
                    .bg(rgb(c))
                    .cursor_pointer()
                    .hover(|s| s.border_color(rgb(ACCENT)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.apply_color((!is_default).then_some(c), window);
                        cx.notify();
                    }))
            });
            div().flex().flex_row().gap(px(SWATCH_GAP)).children(cells)
        });

        let card = div()
            .id("color-card")
            .occlude()
            .absolute()
            .top(px(ADDR_H - 2.))
            .left(px(8.))
            .w(px(SWATCH_COLS as f32 * SWATCH + (SWATCH_COLS - 1) as f32 * SWATCH_GAP + 22.))
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(0xFFFFFF))
            .shadow(vec![gpui::BoxShadow {
                color: gpui::hsla(0., 0., 0., 0.25),
                offset: gpui::point(px(0.), px(4.)),
                blur_radius: px(14.),
                spread_radius: px(0.),
            }])
            .track_focus(&self.color_focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| {
                if this.color_key(ev, window, cx) {
                    cx.stop_propagation();
                }
                cx.notify();
            }))
            .child(div().flex().flex_row().items_center().gap(px(6.)).child(input_box).child(ok).child(pick))
            .child(div().flex().flex_col().gap(px(SWATCH_GAP)).children(swatch_rows));

        // click anywhere outside the card closes it
        Some(
            div()
                .id("color-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, window, cx| {
                        this.color_popup = None;
                        window.focus(&this.list_focus);
                        cx.notify();
                    }),
                )
                .child(card)
                .into_any_element(),
        )
    }
}

/// A complete, empty workbook: Excel will not open a zero-byte .xlsx, since the format is a zip
/// of XML parts.
const EMPTY_XLSX: &[u8] = include_bytes!("../assets/empty.xlsx");

/// Size of a freshly opened window, in logical pixels.
const WIN_W: f32 = 780.;
const WIN_H: f32 = 1200.;
/// How small the window may be dragged.
const MIN_W: f32 = 200.;
const MIN_H: f32 = 260.;

/// What still fits at the current window width. Narrow windows drop the columns on the right
/// rather than squeezing the names down to nothing, and the address box goes once there is no
/// room left for it either.
fn visible_columns(window: &Window) -> (bool, bool, bool) {
    let w = f32::from(window.bounds().size.width);
    let show_size = w >= 300.;
    let show_date = w >= 460.;
    let show_address = w >= 330.;
    (show_size, show_date, show_address)
}

/// Indices into `MiniExpo::ui_icons`.
const ICO_OPEN: usize = 0;
const ICO_FAV: usize = 1;
const ICO_NEW: usize = 2;
const ICO_UP: usize = 3;
const ICO_PICK: usize = 4;
const ICO_BIGGER: usize = 5;
const ICO_SMALLER: usize = 6;
const ICO_RESET: usize = 7;

/// Left edge of the Favorites button, counted back from the window's right edge:
/// … | Favorites | New Window | ↑ | padding — all square, all `CTRL_H` wide.
fn fav_button_left(window_width: f32) -> f32 {
    window_width - 8. - 3. * CTRL_H - 2. * 6.
}

/// Backs GPUI's platform text input for whichever of the rename / address / colour-hex boxes
/// currently has focus (see `active_input` above). This is what makes typing in those boxes
/// go through `WM_CHAR` / IME composition on Windows rather than `KeyDownEvent.key_char`,
/// which is the only path that delivers Chinese/Japanese/Korean composition and the space bar
/// at all — see `TextInput`'s `ime_*` methods for the byte-offset core this wraps.
///
/// Every range here is in UTF-16 code units (`InputHandler`'s contract, inherited from
/// `NSTextInputClient`); `TextInput` itself stays byte-offset throughout, so each method
/// converts at the boundary using `text_input::{utf16_to_byte, byte_to_utf16}`.
impl EntityInputHandler for MiniExpo {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let input = self.active_input(window)?;
        let start = text_input::utf16_to_byte(&input.text, range_utf16.start);
        let end = text_input::utf16_to_byte(&input.text, range_utf16.end);
        let (text, actual) = input.text_in_range(start..end);
        *adjusted_range =
            Some(text_input::byte_to_utf16(&input.text, actual.start)..text_input::byte_to_utf16(&input.text, actual.end));
        Some(text)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let input = self.active_input(window)?;
        let r = input.selection_range();
        let range = text_input::byte_to_utf16(&input.text, r.start)..text_input::byte_to_utf16(&input.text, r.end);
        Some(UTF16Selection { range, reversed: input.selection_reversed() })
    }

    fn marked_text_range(&self, window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        let input = self.active_input(window)?;
        let r = input.marked_range()?;
        Some(text_input::byte_to_utf16(&input.text, r.start)..text_input::byte_to_utf16(&input.text, r.end))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.active_input_mut(window) {
            input.ime_unmark();
        }
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.active_input_mut(window) else { return };
        let range = range_utf16.map(|r| {
            text_input::utf16_to_byte(&input.text, r.start)..text_input::utf16_to_byte(&input.text, r.end)
        });
        input.ime_replace(range, text);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.active_input_mut(window) else { return };
        let range = range_utf16.map(|r| {
            text_input::utf16_to_byte(&input.text, r.start)..text_input::utf16_to_byte(&input.text, r.end)
        });
        // `new_selected_range_utf16` is relative to `new_text` itself, not the whole document.
        let new_selected = new_selected_range_utf16.map(|r| {
            text_input::utf16_to_byte(new_text, r.start)..text_input::utf16_to_byte(new_text, r.end)
        });
        input.ime_replace_and_mark(range, new_text, new_selected);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let input = self.active_input(window)?;
        let start = text_input::utf16_to_byte(&input.text, range_utf16.start);
        let end = text_input::utf16_to_byte(&input.text, range_utf16.end);
        let line = Self::shape_text(&input.text, window);
        // border (1) + padding (3) sit between the box's edge and its text, matching
        // `rename_caret_at` / the boxes' own `px(3.)` padding plus their 1px border.
        let pad = px(4.);
        let x0 = element_bounds.origin.x + pad + line.x_for_index(start);
        let x1 = element_bounds.origin.x + pad + line.x_for_index(end);
        Some(Bounds {
            origin: gpui::point(x0, element_bounds.origin.y),
            size: size(x1 - x0, element_bounds.size.height),
        })
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.active_bounds(window);
        let input = self.active_input(window)?;
        let rel = (point.x - bounds.origin.x - px(4.)).max(px(0.));
        let byte = Self::shape_text(&input.text, window).closest_index_for_x(rel);
        Some(text_input::byte_to_utf16(&input.text, byte))
    }
}

impl Render for MiniExpo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .bg(rgb(0xFFFFFF))
            .font_family(UI_FONT)
            .text_size(px(FONT_PX))
            .font_weight(FontWeight::NORMAL)
            .text_color(rgb(TEXT))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.go_back(window);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.go_forward(window);
                    cx.notify();
                }),
            )
            .child(self.render_address_bar(window, cx))
            .child(self.render_header(window, cx))
            .child(self.render_list(window, cx))
            .child(self.render_status_bar(cx))
            .children(self.render_color_popup(window, cx))
    }
}

// ───────────────────────── helpers ─────────────────────────

fn window_hwnd(window: &Window) -> isize {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match HasWindowHandle::window_handle(window).map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

/// A preset colour no other miniExpo window is using, or None for the first window.
fn pick_window_color(instance: u32) -> Option<u32> {
    if instance == 0 {
        return None;
    }
    // Slot 0 is the default look, which window 0 already has: hand out the real colours only.
    let mut order: Vec<usize> = (1..PRESET_COLORS.len()).collect();
    // Shuffle with a cheap PRNG seeded from the clock, the instance and the process id — no need
    // for a dependency just to pick a colour.
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ ((instance as u64) << 32)
        ^ std::process::id() as u64;
    for i in (1..order.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        order.swap(i, (seed >> 33) as usize % (i + 1));
    }
    // Every colour taken (more than 30 windows): leave this one plain.
    order.into_iter().find(|i| platform::claim_color(*i)).map(|i| PRESET_COLORS[i])
}

fn hex_string(c: u32) -> String {
    format!("#{:06X}", c & 0xFFFFFF)
}

/// "#RRGGBB", "RRGGBB", "#RGB".
fn parse_hex(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('#');
    match s.len() {
        6 => u32::from_str_radix(s, 16).ok(),
        3 => {
            let v = u32::from_str_radix(s, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF);
            Some((r * 17) << 16 | (g * 17) << 8 | (b * 17))
        }
        _ => None,
    }
}

/// A folder may be passed on the command line (that is how New Window opens the current
/// folder). Without one, the window starts at My Computer.
fn start_location() -> Loc {
    let arg = std::env::args().nth(1)?;
    let p = fsops::parse_user_path(&arg);
    p.is_dir().then_some(p)
}

/// This process was spawned by `fsops::spawn_restore_window` to reopen one window from a saved
/// session, at its saved position and size.
struct RestoreArgs {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    folder: Loc,
}

fn parse_restore_args() -> Option<RestoreArgs> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--restore-window") {
        return None;
    }
    let x = args.next()?.parse().ok()?;
    let y = args.next()?.parse().ok()?;
    let w = args.next()?.parse().ok()?;
    let h = args.next()?.parse().ok()?;
    let folder_arg = args.next().unwrap_or_default();
    let folder = (!folder_arg.is_empty()).then(|| fsops::parse_user_path(&folder_arg)).filter(|p| p.is_dir());
    Some(RestoreArgs { x, y, w, h, folder })
}

fn main() {
    // Must run before any window exists: picks this process's instance number and gives it its own
    // AppUserModelID, so the taskbar never merges two miniExpo windows into one button.
    let instance = platform::init_instance();
    let restore = parse_restore_args();
    // A plain cold start — launched with no folder argument and not one of our own
    // `--restore-window` re-spawns — while no other miniExpo is running at all: the moment to
    // bring back every window from the end of the last run, where it was left.
    let cold_start = restore.is_none() && std::env::args().nth(1).is_none() && platform::is_only_running_instance(instance);
    let saved_session = if cold_start { fsops::take_session() } else { Vec::new() };
    let own_saved = saved_session.first().cloned();
    let rest_saved: Vec<fsops::SessionWindow> = saved_session.into_iter().skip(1).collect();
    let start =
        restore.as_ref().map(|r| r.folder.clone()).or_else(|| own_saved.as_ref().map(SessionWindow::folder)).unwrap_or_else(start_location);

    Application::new().run(move |cx: &mut App| {
        // Never taller or wider than the screen, so the window stays usable on a small display.
        let screen = cx.primary_display().map(|d| d.bounds().size);
        let (mut w, mut h) = (WIN_W, WIN_H);
        if let Some(s) = screen {
            w = w.min(f32::from(s.width) - 40.);
            h = h.min(f32::from(s.height) - 80.);
        }
        let restored = restore
            .as_ref()
            .map(|r| (r.x, r.y, r.w, r.h))
            .or_else(|| own_saved.as_ref().map(|s| (s.x, s.y, s.w, s.h)));
        let bounds = if let Some((x, y, bw, bh)) = restored {
            Bounds { origin: gpui::point(px(x as f32), px(y as f32)), size: size(px(bw as f32), px(bh as f32)) }
        } else {
            let mut b = Bounds::centered(None, size(px(w), px(h)), cx);
            // cascade extra instances a little so they don't sit exactly on top of each other
            let shift = px(28. * (instance % 8) as f32);
            b.origin.x += shift;
            b.origin.y += shift;
            b
        };
        // GPUI otherwise measures a restored window's bounds against the primary monitor alone
        // and silently re-centers it there if that check fails — so on a multi-monitor setup, a
        // window saved on a secondary display would always snap back to the primary one. Telling
        // it which display the saved center point actually falls on fixes that.
        let display_id = restored.and_then(|(x, y, bw, bh)| {
            let center = gpui::point(px(x as f32 + bw as f32 / 2.), px(y as f32 + bh as f32 / 2.));
            cx.displays().into_iter().find(|d| d.bounds().contains(&center)).map(|d| d.id())
        });
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(MIN_W), px(MIN_H))),
                display_id,
                titlebar: Some(TitlebarOptions {
                    title: Some(format!("{instance} - miniExpo").into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| cx.new(|cx| MiniExpo::new(instance, start.clone(), window, cx)),
        )
        .unwrap();

        // Bring back the rest of last run's windows too — each a fresh process of its own.
        for sw in &rest_saved {
            fsops::spawn_restore_window(sw);
        }

        cx.on_window_closed(move |cx| {
            // The user closed this window (rather than the process crashing or being killed):
            // so it should not be reopened at the next cold start.
            fsops::forget_window(instance);
            cx.quit();
        })
        .detach();
        cx.activate(true);
    });
}
