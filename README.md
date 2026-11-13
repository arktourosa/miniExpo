# miniExpo

A small file manager for Windows — Rust + GPUI.
Full specification (English is normative; also in 简体中文, 繁體中文, 日本語, Français): [`specification/SPEC.en.md`](specification/SPEC.en.md).

## Build

* `2-build-and-run.bat` — release build (log in `logs\build-log.txt`), then starts it.
* `3-package.bat` — builds and copies `miniExpo.exe` to `dist\miniExpo`.
* Start in a folder: `miniexpo.exe D:\some\folder`. With no argument the window opens at My Computer.

## Layout of the code

| File | What's in it |
|---|---|
| `src/main.rs` | the whole UI: address bar, column header, file list, colour popup, keys |
| `src/fsops.rs` | listing, sorting, size/date formatting, favorites file |
| `src/icons.rs` | shell icon cache (per file type, per file for drives/.exe/.lnk) |
| `src/text_input.rs` | the small single-line text field (GPUI ships none) |
| `src/platform/win.rs` | everything Windows: context menu, menus, title colour, taskbar badge, colour picker |
| `src/platform/stub.rs` | no-op versions so the UI builds on Linux/macOS |

## task-01

| Spec item | Where |
|---|---|
| Narrow title bar, folder name left, min/max/close right | `main.rs` (`TitlebarOptions`, `set_window_title`) |
| Instance number in the title + badge on the taskbar button, one button per window | `platform/win.rs` (`init_instance`, `set_overlay_number`) |
| Address bar + **Open** + **Favorites** (drives, separator, saved folders) | `main.rs`, `win.rs` (`do_popup_list`) |
| Enter in the address bar also navigates (accepts a bare sub-folder name, `%VAR%`, a file path) | `open_typed` |
| Colour button: hex input + 10 presets → title bar colour | `PRESET_COLORS`, `set_title_color` |
| Name / Size / Date modified, Explorer's font and row height | `render_header`, `rows` |
| Double-click on empty space = parent folder | `blank_mouse_down` |
| Green mark on names changed in the last 30 minutes | `RECENT_BG`, `fsops::is_recent` |
| System context menu | `win.rs` (`do_context_menu`) |
| Ctrl+C / Ctrl+X / Ctrl+V, Delete, Shift+Delete | shell verbs run on the items' pidls (`list_key`, `do_verb`) |
| Drop files into the window → copied into the open folder | `platform/dnd.rs` |
| Pasting (Ctrl+V, or Paste / New in the context menu) selects the files that turn up | `arm_paste_watch`, `apply_pending_paste` (diff of the folder before / after) |
| Left-drag on empty space draws a selection rectangle (Ctrl / Shift adds; scrolls at the edges) | `Marquee`, `marquee_update`, `marquee_autoscroll` |
| Left-drag from a file or folder drags the selected items out — to Explorer, another miniExpo window, mail… | `maybe_start_file_drag`, `win.rs` `drag_files_out` (`SHDoDragDrop` on the shell's own data object) |

## task-02

| Spec item | Where |
|---|---|
| 1. Context menu starts with **Refresh**, **Open in Explorer**, **PowerShell** (Shift = Administrator) and **New Window**, each with a small icon | `win.rs` `ID_REFRESH` / `ID_NEW_WINDOW` |

Our three entries go into the menu *before* `QueryContextMenu`, which is then told to start below
them. Adding them afterwards, at the top, shifts every shell entry down: handlers that remember
the position they put an item at — rather than the menu handle they created — then fill the wrong
submenu, and entries like "PowerShell 7" or "Give access to" open empty while "New" still works.
| 2. **Open** shows the Windows folder picker; the chosen folder opens in the list | `win.rs` `do_pick_folder` |
| 3. **↑** (one level up) and **New Window** buttons on the toolbar | `render_address_bar` |
| 4. 5 blank rows kept below the last item | `TAIL_ROWS` |
| 5. A window with no folder argument starts at **My Computer** (the drive list) | `Loc = Option<PathBuf>`, `read_loc` |
| 6. Row height raised 22 → 28 px to match Explorer at 100 % | `ROW_H` |
| 7. Colour popup: hint line gone, **Default** replaced by an eyedropper | `render_color_popup`, `win.rs` `do_pick_color` |

The eyedropper freezes the screen, shows the hex next to the cursor, and picks the colour where you
click — anywhere, including inside a browser. Esc or right-click cancels.
The first swatch is the default: picking it hands the title bar back to Windows, and it is lit
whenever no colour is set. Clearing the hex box and pressing OK does the same. New windows are
only ever given one of the other 29.

The window goes down to 200 px wide. Narrow windows drop what no longer fits rather than
squeezing the names away: the Date column below 460 px, Size below 300 px, and the address box
below 330 px, leaving the toolbar buttons (`visible_columns`).

Every folder lists `..` first (pinned there whichever column sorts the list); double-click or
Enter on it goes up. It is never picked up by copy / rename / delete or by Ctrl+A.

Holding **Left Alt** and pressing the left mouse button anywhere in the window — on blank space,
a file or a folder — drags the window, by handing over to the system's own move loop
(`start_window_drag`), the same one the title bar uses, so edge snapping and multi-monitor behaviour
come free. A plain left click keeps its normal meaning (select / double-click to open). The status
bar still drags on a plain press. It is started from `cx.defer`, once GPUI has finished the update
the click arrived in: updating a window takes it out of GPUI's slot, and that loop pumps messages,
so anything polled meanwhile would find the window missing.
The 100 ms ticker guards against the same thing from the other side — it treats a failed update as
transient and only stops once the view can no longer be upgraded. Every shell result (favorites,
drops, deletes) arrives through that ticker, so a ticker that quits silently takes all of them with
it. A double-click on empty space
still goes up a level.

**Ctrl+N** creates a folder here and opens its name for editing, as Explorer does; **Ctrl+M**,
**Ctrl+T** and **Ctrl+E** do the same for a `.md`, a `.txt` and an `.xlsx`. A clash becomes
"name (2)", and for a file the extension stays out of the selection so typing replaces only the
name. The spreadsheet is written from `assets/empty.xlsx`, a complete empty workbook — an .xlsx is
a zip of XML parts, and Excel refuses to open a zero-byte one. (Ctrl+N used to open a new window;
the toolbar button, the context menu and Win+Q still do that.)

The status bar along the bottom shows what is in the folder and carries three window-size buttons:
+ grows the window to 1.25x, − shrinks it to 0.75x, and the arrow puts it back to the size a new
window opens at. All three stay within the screen and the 200x260 minimum.

The toolbar is icons only: open folder, star (favorites), new window, and — at the far right —
one level up. The mouse's side buttons go back and forward through this window's history
(Alt+← / Alt+→ do the same).

Dropping works for files on disk *and* for the virtual files Outlook drags: a mail is not a file
anywhere, it arrives as a `FileGroupDescriptorW` naming the items plus `FileContents` per item,
which the receiver writes out itself. GPUI only understands `CF_HDROP`, so `platform/dnd.rs`
registers its own `IDropTarget` in place of GPUI's and handles both.

Two things about Outlook are worth knowing before touching `save_item`. Its `FileContents` is
advertised as `TYMED_ISTREAM | TYMED_ISTORAGE`, but a mail is only ever delivered as
**`TYMED_ISTORAGE`** — ask for the stream alone and `GetData` fails outright. And a `.msg` is a
compound file, so it is written with `StgCreateDocfile` + `IStorage::CopyTo`, not by writing bytes.
Attachments and other sources do give a stream or a memory block, so all three media are requested
at once and whatever arrives is handled. Outlook also answers `QueryGetData` unreliably, so a
refusal is double-checked against the formats the data object enumerates.

Names are sanitised (a mail subject can hold characters a path cannot) and never overwrite: a
clash becomes "name (2)".

The listing is kept current from four directions, because a watcher alone misses too much: the
`notify` watcher, a half-second `stat` of the folder (its timestamp moves whenever an entry is
added, removed or renamed, so a full re-listing only happens when it has), a burst of re-reads for
three seconds after any shell command (copy, delete and friends finish their work after the call
that starts them returns), and a re-read the moment the window is activated. A focused window also
re-lists every six seconds as a last resort. **Refresh** in the context menu and **F5** do it on
demand. A window in the background polls every five seconds and costs nothing.

The "modified in the last 30 minutes" mark is a green pill behind the file name only — Size and
Date modified stay plain — so it never competes with the blue of a selected row. A selected row
drops the green entirely.

Delete puts the selection in the Recycle Bin without asking (it can be undone, and that is the
Windows default); Shift+Delete deletes for good after Windows' own confirmation. Both — and copying
dropped files in — go through `IFileOperation` rather than a context-menu verb: it is synchronous,
so the listing is re-read the moment the work is really done, and it skips building a context
menu, whose first use in a process loads every shell extension on the machine and can stall for
seconds. Items are handed over as shell items built from their folder plus their relative name, so
paths past MAX_PATH work too. Every dialog the shell puts up — the Shift+Delete confirmation, a
name clash, an elevation prompt — is owned by the miniExpo window and the focus is put back there
afterwards; owned by the shell thread's hidden window instead, closing one would leave the user
clicking to get back. After a drop the new arrivals are selected and scrolled into view.

A name being edited never blocks a re-read: the edit survives if its file is still there and is
dropped if not, and an edit left open when the window loses focus is committed, as in Explorer.
(An earlier version skipped re-reads during an edit; an edit abandoned by switching windows then
froze the listing and swallowed F5 and Delete.)

Items are handed to the shell as pidls parsed relative to their folder, not as full path strings:
`SHParseDisplayName` stops at MAX_PATH, which an exported `.msg` named after a mail subject passes
easily, and such items would otherwise get no system context menu.

Global hotkeys (all swallowed, so nothing else reacts to them):

| Key | What it does |
|---|---|
| **Win+Q** | open a fresh window at My Computer |
| **Left Alt+Q** | bring the next miniExpo window to the front, wrapping around — Alt+Tab, but only among these windows |
| **Left Alt+1** … **Alt+9** | jump straight to the window with that number (the one in its title and on its taskbar badge) |

Right Alt is left alone, since it is AltGr on many layouts. Open windows find each other through a
small shared memory block (`registry()`) holding each instance's window handle and process id; the
pid is what tells a live window from a slot left behind by a process that has exited. Bringing
another process's window to the front needs the foreground thread's input state borrowed for a
moment (`activate_window`), which is what Windows requires.

The hotkey is **Win+Q**: it opens a fresh window at My Computer from anywhere. Windows keeps
Win+Q for Search, so `RegisterHotKey` is refused and a low-level keyboard hook takes the key before
the shell sees it and swallows it — Win+Q no longer opens Search (Win+S still does). The hook runs
on a thread of its own, because Windows drops a hook whose thread stops answering and the shell
thread blocks on file operations. A named mutex picks one owner process; it hands over within three
seconds when that process exits.

Extras: click a column header to sort, arrow keys, F2 rename, Delete, F5, Backspace / ↑ for the
parent folder, Ctrl+N new window, Ctrl+L or F4 to edit the address.
Favorites live in `%APPDATA%\miniExpo\favorites.json` and are shared by all windows.

Each window draws its own number very faintly behind the file list (`render_watermark`), in the
window's colour, centred and scaled to the window — half the list's height, capped so a two-digit
number still fits across. A pale preset is given more opacity than a dark one, since it would
otherwise vanish against the white background.

The first window keeps the plain Windows title bar; every window after it takes a random preset
colour that no other open window is using (`pick_window_color`). A colour is claimed with a named
mutex, so it returns to the pool when its window closes. The taskbar number badge follows the same
colour. Picking one by hand in the colour popup overrides it.

The 30 title-bar presets are in `PRESET_COLORS` (`src/main.rs`), shown five to a row in the
order they are listed there.

## Checking the Windows-only code on another OS

`RUSTFLAGS="--cfg miniexpo_check" cargo check` type-checks `platform/win.rs` on Linux/macOS too.

## License

Apache License, Version 2.0 — see [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE).
Copyright 2026 JIAW.
