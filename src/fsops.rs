//! File-system helpers: directory listing, sorting, formatting, favorites storage.

use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local};

#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    /// A drive in the My Computer list ("Local Disk (C:)"), not an ordinary folder.
    pub is_drive: bool,
    /// The ".." row that goes one level up. Always listed first, never a target of
    /// copy / rename / delete.
    pub is_parent: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

impl Entry {
    /// The ".." row shown at the top of every folder.
    pub fn parent_row(dir: &Path) -> Entry {
        Entry {
            name: "..".to_string(),
            path: dir.parent().unwrap_or(dir).to_path_buf(),
            is_dir: true,
            is_drive: false,
            is_parent: true,
            size: 0,
            modified: None,
        }
    }

    /// What this row sorts by in the Name column. Drives sort by their letter rather than by
    /// the volume label, so the list always reads C:, D:, E: … whatever the disks are called.
    pub fn sort_name(&self) -> &str {
        if self.is_drive {
            self.path.to_str().unwrap_or(&self.name)
        } else {
            &self.name
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Modified,
}

#[cfg(windows)]
fn is_hidden(md: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM — hidden like Explorer's default view.
    md.file_attributes() & (0x2 | 0x4) != 0
}

#[cfg(not(windows))]
fn is_hidden(_: &fs::Metadata) -> bool {
    false
}

/// Lists a folder. Hidden / system items are skipped (Explorer's default).
pub fn list_dir(dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for item in fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let Ok(md) = item.metadata() else { continue };
        if is_hidden(&md) {
            continue;
        }
        let path = item.path();
        let is_dir = if md.file_type().is_symlink() {
            fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false)
        } else {
            md.is_dir()
        };
        out.push(Entry {
            name: item.file_name().to_string_lossy().into_owned(),
            path,
            is_dir,
            is_drive: false,
            is_parent: false,
            size: if is_dir { 0 } else { md.len() },
            modified: md.modified().ok(),
        });
    }
    Ok(out)
}

/// Case-insensitive "natural" comparison (file2 < file10), like Explorer.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(&c) = ai.peek() {
                    if c.is_ascii_digit() {
                        na.push(c);
                        ai.next();
                    } else {
                        break;
                    }
                }
                let mut nb = String::new();
                while let Some(&c) = bi.peek() {
                    if c.is_ascii_digit() {
                        nb.push(c);
                        bi.next();
                    } else {
                        break;
                    }
                }
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                let o = x.to_lowercase().cmp(y.to_lowercase());
                if o != Ordering::Equal {
                    return o;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// Folders always come first; inside each group sort by the chosen key.
pub fn sort_entries(entries: &mut [Entry], key: SortKey, asc: bool) {
    entries.sort_by(|a, b| {
        // ".." stays at the top whatever the column or direction.
        if a.is_parent || b.is_parent {
            return b.is_parent.cmp(&a.is_parent);
        }
        if a.is_dir != b.is_dir {
            return if a.is_dir { Ordering::Less } else { Ordering::Greater };
        }
        let o = match key {
            SortKey::Name => natural_cmp(a.sort_name(), b.sort_name()),
            SortKey::Size if !a.is_dir => {
                a.size.cmp(&b.size).then_with(|| natural_cmp(a.sort_name(), b.sort_name()))
            }
            SortKey::Modified => {
                a.modified.cmp(&b.modified).then_with(|| natural_cmp(a.sort_name(), b.sort_name()))
            }
            _ => natural_cmp(a.sort_name(), b.sort_name()),
        };
        if asc {
            o
        } else {
            o.reverse()
        }
    });
}

/// "191,235 KB" — Explorer rounds sizes up to whole KB.
pub fn format_size(bytes: u64) -> String {
    let kb = bytes.div_ceil(1024).to_string();
    let mut out = String::new();
    for (i, ch) in kb.chars().enumerate() {
        if i > 0 && (kb.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    format!("{out} KB")
}

/// "11/7/2026 3:38 PM"
pub fn format_time(t: Option<SystemTime>) -> String {
    match t {
        Some(t) => DateTime::<Local>::from(t).format("%-m/%-d/%Y %-I:%M %p").to_string(),
        None => String::new(),
    }
}

/// Modified within the last 30 minutes (the green highlight).
pub fn is_recent(t: Option<SystemTime>) -> bool {
    match t.and_then(|t| SystemTime::now().duration_since(t).ok()) {
        Some(age) => age <= Duration::from_secs(30 * 60),
        // modified "in the future" (clock skew) counts as just now
        None => t.is_some(),
    }
}

/// Folder title shown in the window title: the folder's name, or "C:\" for a drive root.
pub fn folder_title(path: &Path) -> String {
    match path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

/// Path as shown in the address bar (no trailing separator except for a drive root).
pub fn display_path(path: &Path) -> String {
    let s = path.to_string_lossy().into_owned();
    if path.parent().is_none() {
        return s;
    }
    s.trim_end_matches(['\\', '/']).to_string()
}

/// Turns whatever the user typed into a path: trims quotes/spaces, expands `%VAR%`,
/// and turns "D:" into "D:\".
pub fn parse_user_path(text: &str) -> PathBuf {
    let mut s = text.trim().trim_matches('"').trim().to_string();
    if s.contains('%') {
        let mut out = String::new();
        let mut rest = s.as_str();
        while let Some(i) = rest.find('%') {
            out.push_str(&rest[..i]);
            let after = &rest[i + 1..];
            match after.find('%') {
                Some(j) => {
                    match std::env::var(&after[..j]) {
                        Ok(v) => out.push_str(&v),
                        Err(_) => {
                            out.push('%');
                            out.push_str(&after[..j]);
                            out.push('%');
                        }
                    }
                    rest = &after[j + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        s = out;
    }
    let b = s.as_bytes();
    if b.len() == 2 && b[1] == b':' {
        s.push('\\');
    }
    PathBuf::from(s)
}

// ───────────────────────── favorites ─────────────────────────

pub fn favorites_file() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("miniExpo").join("favorites.json")
}

fn same_path(a: &str, b: &str) -> bool {
    a.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

/// Favorites are shared by all running instances: always read from / written to disk.
pub fn load_favorites() -> Vec<String> {
    fs::read_to_string(favorites_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .unwrap_or_default()
}

fn save_favorites(list: &[String]) {
    let file = favorites_file();
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(list) {
        let tmp = file.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &file);
        }
    }
}

pub fn is_favorite(path: &Path) -> bool {
    let p = path.to_string_lossy();
    load_favorites().iter().any(|f| same_path(f, &p))
}

/// Adds the folder, or removes it when it is already a favorite.
pub fn toggle_favorite(path: &Path) {
    let p = display_path(path);
    let mut list = load_favorites();
    if let Some(i) = list.iter().position(|f| same_path(f, &p)) {
        list.remove(i);
    } else {
        list.push(p);
    }
    save_favorites(&list);
}

// ───────────────────────── session (window positions) ─────────────────────────

/// One open window's folder and screen position/size, as last reported. `id` is the instance
/// number that owned the window when this was written — meaningful only as a key for updating
/// or removing this one entry within a single run, never across restarts (a fresh process picks
/// its own instance number independently of what is stored here).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionWindow {
    id: u32,
    /// Absolute folder path, or empty for My Computer.
    folder: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl SessionWindow {
    pub fn folder(&self) -> Option<PathBuf> {
        (!self.folder.is_empty()).then(|| PathBuf::from(&self.folder))
    }
}

fn session_file() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("miniExpo").join("session.json")
}

/// Shared by all running instances, same as favorites: always read from / written to disk.
pub fn load_session() -> Vec<SessionWindow> {
    fs::read_to_string(session_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<SessionWindow>>(&s).ok())
        .unwrap_or_default()
}

fn save_session(list: &[SessionWindow]) {
    let file = session_file();
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(list) {
        let tmp = file.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &file);
        }
    }
}

/// Reads the saved session and clears the file in the same step, so last run's windows are only
/// ever replayed once: each one that reopens writes its own fresh entry right back as it starts,
/// under whatever instance number it is given this time, instead of leaving the old entries
/// behind to be replayed again — and piling up — on a later cold start.
pub fn take_session() -> Vec<SessionWindow> {
    let list = load_session();
    if !list.is_empty() {
        save_session(&[]);
    }
    list
}

/// Records this window's current folder and bounds: called when it opens, navigates, is moved
/// or is resized (after the user stops dragging, not on every intermediate frame).
pub fn save_window_state(id: u32, folder: Option<&Path>, bounds: (i32, i32, i32, i32)) {
    let mut list = load_session();
    let folder = folder.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let (x, y, w, h) = bounds;
    match list.iter_mut().find(|e| e.id == id) {
        Some(e) => {
            e.folder = folder;
            e.x = x;
            e.y = y;
            e.w = w;
            e.h = h;
        }
        None => list.push(SessionWindow { id, folder, x, y, w, h }),
    }
    save_session(&list);
}

/// Drops this window's entry: called when the user actively closes it, so a later cold start
/// does not bring it back.
pub fn forget_window(id: u32) {
    let mut list = load_session();
    list.retain(|e| e.id != id);
    save_session(&list);
}

/// Starts a miniExpo process at a specific folder, position and size — shared by session
/// restore (a cold start) and the session-group buttons (an explicit user request).
fn spawn_window_at(folder: &str, x: i32, y: i32, w: i32, h: i32) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--restore-window").arg(x.to_string()).arg(y.to_string()).arg(w.to_string()).arg(h.to_string()).arg(folder);
    let _ = cmd.spawn();
}

/// Starts a miniExpo process to reopen one window from a saved session, at its saved folder,
/// position and size.
pub fn spawn_restore_window(w: &SessionWindow) {
    spawn_window_at(&w.folder, w.x, w.y, w.w, w.h);
}

// ───────────────────────── session group (manual snapshot) ─────────────────────────

/// One window's folder and bounds, as captured by "Update Session Group". Distinct from the
/// auto-tracked `session.json`: this file only changes when the user explicitly asks, via the
/// two buttons in the Favorites menu, so it can be reopened later even after the regular
/// session has moved on to something else.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct GroupWindow {
    /// Absolute folder path, or empty for My Computer.
    folder: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn session_group_file() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("miniExpo").join("last_session_group.json")
}

fn load_session_group() -> Vec<GroupWindow> {
    fs::read_to_string(session_group_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<GroupWindow>>(&s).ok())
        .unwrap_or_default()
}

fn save_session_group(list: &[GroupWindow]) {
    let file = session_group_file();
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(list) {
        let tmp = file.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(&tmp, &file);
        }
    }
}

/// "Update Session Group": every currently open window keeps its own entry in `session.json`
/// up to date live, so the snapshot is simply a copy of that, taken right now.
pub fn update_session_group() {
    let list: Vec<GroupWindow> =
        load_session().into_iter().map(|w| GroupWindow { folder: w.folder, x: w.x, y: w.y, w: w.w, h: w.h }).collect();
    save_session_group(&list);
}

/// "Open Last Session Group": reopens every window from the last snapshot whose folder does not
/// match one already open (same comparison as favorites), so nothing already open is duplicated.
pub fn open_session_group() {
    let group = load_session_group();
    if group.is_empty() {
        return;
    }
    let open: Vec<String> = load_session().into_iter().map(|w| w.folder).collect();
    for w in &group {
        if !open.iter().any(|f| same_path(f, &w.folder)) {
            spawn_window_at(&w.folder, w.x, w.y, w.w, w.h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        assert_eq!(natural_cmp("file2", "file10"), Ordering::Less);
        assert_eq!(natural_cmp("B", "a"), Ordering::Greater);
        assert_eq!(natural_cmp("Pasted image 2", "pasted image 10"), Ordering::Less);
    }

    #[test]
    fn sizes_round_up_like_explorer() {
        assert_eq!(format_size(0), "0 KB");
        assert_eq!(format_size(1), "1 KB");
        assert_eq!(format_size(2640), "3 KB");
        assert_eq!(format_size(191_235 * 1024), "191,235 KB");
    }

    #[test]
    fn user_paths() {
        assert_eq!(parse_user_path("  \"D:\"  "), PathBuf::from("D:\\"));
        assert_eq!(display_path(Path::new("/")), "/");
        assert_eq!(display_path(Path::new("/a/b/")), "/a/b");
    }

    fn entry(name: &str, path: &str, is_drive: bool) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from(path),
            is_dir: true,
            is_drive,
            is_parent: false,
            size: 0,
            modified: None,
        }
    }

    /// ".." is pinned to the top, ascending or descending.
    #[test]
    fn parent_row_stays_first() {
        for asc in [true, false] {
            let mut v = vec![
                entry("beta", "/x/beta", false),
                Entry::parent_row(Path::new("/x")),
                entry("alpha", "/x/alpha", false),
            ];
            sort_entries(&mut v, SortKey::Name, asc);
            assert_eq!(v[0].name, "..");
        }
    }

    /// Drives are listed by letter, not by the volume label shown in the Name column.
    #[test]
    fn drives_sort_by_letter() {
        let mut v = vec![
            entry("ADATA (E:)", "E:\\", true),
            entry("Windows-SSD (C:)", "C:\\", true),
            entry("Q (F:)", "F:\\", true),
            entry("SSDQ (D:)", "D:\\", true),
        ];
        sort_entries(&mut v, SortKey::Name, true);
        let letters: Vec<&str> = v.iter().map(|e| &e.path.to_str().unwrap()[..1]).collect();
        assert_eq!(letters, ["C", "D", "E", "F"]);
    }

    /// Ordinary folders keep sorting by the name on screen.
    #[test]
    fn folders_sort_by_name() {
        let mut v = vec![entry("beta", "/x/beta", false), entry("Alpha", "/x/Alpha", false)];
        sort_entries(&mut v, SortKey::Name, true);
        assert_eq!(v[0].name, "Alpha");
    }

    #[test]
    fn recent_window() {
        let now = SystemTime::now();
        assert!(is_recent(Some(now - Duration::from_secs(29 * 60))));
        assert!(!is_recent(Some(now - Duration::from_secs(31 * 60))));
    }
}
