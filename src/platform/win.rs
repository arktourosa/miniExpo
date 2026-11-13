//! Windows side of miniExpo: the system (shell) context menu, shell icons, drive list,
//! title-bar colour, taskbar number badge, "one taskbar button per window", and the
//! full-screen colour picker.
//!
//! GPUI owns the main window's message loop, and a native menu runs its own modal loop, so all
//! shell work happens on a dedicated thread with its own hidden window and message pump. The
//! UI thread only posts requests to it and reads the answers back as `ShellEvent`s.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::{c_void, CString};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use windows::core::{w, Interface, PCSTR, PCWSTR, PSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, BOOL, COLORREF, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, INVALID_HANDLE_VALUE,
    LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, ClientToScreen, CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush,
    DeleteDC, DeleteObject, EndPaint, FillRect, FrameRect, GetDC, GetDIBits, GetObjectW, GetStockObject,
    InvalidateRect, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, DEFAULT_GUI_FONT, DIB_RGB_COLORS, HBITMAP, HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT, SRCCOPY,
    TRANSPARENT,
};
use windows::Win32::Storage::FileSystem::{
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    FILE_FLAGS_AND_ATTRIBUTES,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IBindCtx, IDataObject, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{CreateFileMappingW, MapViewOfFile, FILE_MAP_ALL_ACCESS, PAGE_READWRITE};
use windows::Win32::System::Ole::{IDropSource, OleInitialize, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_SHIFT, RegisterHotKey, ReleaseCapture, SendInput, HOT_KEY_MODIFIERS, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_LMENU, VK_LWIN, VK_RWIN,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    IContextMenu, IContextMenu2, IContextMenu3, IFileOpenDialog, IFileOperation, IFileOperationProgressSink,
    IShellFolder, IShellItem, ITaskbarList3, SHCreateItemFromParsingName, SHCreateItemWithParent, FileOperation,
    FILEOPERATION_FLAGS, FOFX_RECYCLEONDELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, SHGetFileInfoW,
    SHBindToObject, SHDoDragDrop, SHParseDisplayName, SetCurrentProcessExplicitAppUserModelID, ShellExecuteW, FileOpenDialog,
    TaskbarList, CMIC_MASK_SHIFT_DOWN,
    CMF_CANRENAME, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_PTINVOKE, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX,
    FILEOPENDIALOGOPTIONS, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, GCS_VERBA, SHFILEINFOW,
    SHGFI_ICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetIconInfo, GetMenuItemCount, GetMenuItemInfoW, GetMenuStringW, SetMenuItemInfoW, MIIM_BITMAP, SM_CXSMICON, GetMessageW, GetSubMenu, MENUITEMINFOW, MFT_SEPARATOR, MIIM_FTYPE, RemoveMenu, GetSystemMetrics, InsertMenuW, LoadCursorW,
    BringWindowToTop, CallNextHookEx, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindow,
    KillTimer, PostMessageW, PostThreadMessageW, RegisterClassW, SendMessageW, SetCursor, SetForegroundWindow,
    SetTimer,
    SetWindowsHookExW, ShowWindow, TrackPopupMenuEx, TranslateMessage,
    HICON, HMENU, ICONINFO, IDC_CROSS, MF_BYPOSITION, MF_SEPARATOR, MF_STRING, MSG, SM_CXVIRTUALSCREEN,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_SHOW, SW_SHOWNORMAL, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_APP, WM_DRAWITEM, WM_INITMENUPOPUP, WM_KEYDOWN, WM_LBUTTONDOWN,
    HC_ACTION, HTCAPTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, WH_KEYBOARD_LL, WM_HOTKEY, WM_KEYUP, WM_MEASUREITEM,
    WM_MENUCHAR, WM_MOUSEMOVE, WM_NULL, WM_PAINT, WM_RBUTTONDOWN, WM_SETCURSOR, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_NCLBUTTONDOWN, WM_TIMER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, SW_RESTORE,
    GetCursorPos, GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
};

use super::{ContextMenuRequest, DriveInfo, MenuEntry, ShellEvent};

#[path = "dnd.rs"]
mod dnd;
#[path = "menu_icons.rs"]
mod menu_icons;
pub use dnd::{enable_drop, set_drop_folder};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn path_wide(p: &Path) -> Vec<u16> {
    wide(&p.to_string_lossy())
}

// ───────────────────────── instances, title colour, drives ─────────────────────────

/// Picks the lowest free instance number (0, 1, 2 …) and gives this process its own
/// AppUserModelID. Different IDs = the taskbar never groups the windows together.
pub fn init_instance() -> u32 {
    for n in 0..=98u32 {
        let name = wide(&format!("Local\\miniExpo.Instance.{n}"));
        unsafe {
            let Ok(handle) = CreateMutexW(None, false, PCWSTR(name.as_ptr())) else { continue };
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                continue;
            }
            // The mutex handle stays open until the process exits, which is what frees the number.
            let id = wide(&format!("miniExpo.Instance.{n}"));
            let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr()));
            return n;
        }
    }
    0
}

/// True if, right now, no other miniExpo process is running — `mine` is the number
/// `init_instance` just gave this one, so its own slot is skipped. Used once at start-up to
/// decide whether this is a cold start that should restore the last session's windows.
pub fn is_only_running_instance(mine: u32) -> bool {
    for n in 0..=98u32 {
        if n == mine {
            continue;
        }
        let name = wide(&format!("Local\\miniExpo.Instance.{n}"));
        unsafe {
            let Ok(handle) = CreateMutexW(None, false, PCWSTR(name.as_ptr())) else { continue };
            let taken = GetLastError() == ERROR_ALREADY_EXISTS;
            let _ = CloseHandle(handle);
            if taken {
                return false;
            }
        }
    }
    true
}

/// Marks preset colour `index` as taken by this window, for as long as it lives. Returns false
/// when another miniExpo already has it — the mutex dies with the process that holds it, so a
/// colour comes back into circulation when its window closes.
pub fn claim_color(index: usize) -> bool {
    let name = wide(&format!("Local\\miniExpo.Color.{index}"));
    unsafe {
        let Ok(handle) = CreateMutexW(None, false, PCWSTR(name.as_ptr())) else { return false };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return false;
        }
        let _ = handle; // kept open on purpose: closing it would free the colour
        true
    }
}

/// Colours the window's title bar (Windows 11 22H2+; on older Windows this is silently ignored).
pub fn set_title_color(hwnd: isize, rgb: Option<u32>) {
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
    if hwnd == 0 {
        return;
    }
    // DWMWA_BORDER_COLOR = 34, DWMWA_CAPTION_COLOR = 35, DWMWA_TEXT_COLOR = 36; 0xFFFFFFFF = default.
    let (caption, text): (u32, u32) = match rgb {
        None => (0xFFFF_FFFF, 0xFFFF_FFFF),
        Some(c) => {
            let (r, g, b) = ((c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF);
            let colorref = r | (g << 8) | (b << 16);
            let luma = (299 * r + 587 * g + 114 * b) / 1000;
            (colorref, if luma > 150 { 0x0000_0000 } else { 0x00FF_FFFF })
        }
    };
    unsafe {
        for (attr, value) in [(34, caption), (35, caption), (36, text)] {
            let _ = DwmSetWindowAttribute(
                HWND(hwnd),
                DWMWINDOWATTRIBUTE(attr),
                &value as *const u32 as *const c_void,
                size_of::<u32>() as u32,
            );
        }
    }
}

pub fn drives() -> Vec<DriveInfo> {
    let mut out = Vec::new();
    unsafe {
        let mask = GetLogicalDrives();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let root = format!("{}:\\", (b'A' + i as u8) as char);
            let w = wide(&root);
            let mut label = String::new();
            // 1 = DRIVE_NO_ROOT_DIR
            if GetDriveTypeW(PCWSTR(w.as_ptr())) > 1 {
                let mut buf = [0u16; 261];
                if GetVolumeInformationW(PCWSTR(w.as_ptr()), Some(&mut buf), None, None, None, None).is_ok() {
                    let len = buf.iter().position(|c| *c == 0).unwrap_or(0);
                    label = String::from_utf16_lossy(&buf[..len]);
                }
            }
            out.push(DriveInfo { root, label });
        }
    }
    out
}

pub fn client_to_screen(hwnd: isize, x: i32, y: i32) -> (i32, i32) {
    let mut pt = POINT { x, y };
    unsafe {
        let _ = ClientToScreen(HWND(hwnd), &mut pt);
    }
    (pt.x, pt.y)
}

/// Hands the window over to the system's own move loop, exactly as if the title bar had been
/// grabbed: edge snapping and multi-monitor behaviour come along with it. Returns once the user
/// lets go.
///
/// This only works for a drag actually driven by the *left* button: the trick is to fake a
/// left-button click on the caption (`WM_NCLBUTTONDOWN` + `HTCAPTION`), and Windows' own
/// move loop ends the drag on the left button's release, not the pointer's. Holding any other
/// button leaves that release condition already satisfied, so the loop exits on the spot and
/// the window never actually follows the pointer — hence `right_drag_*` below for the
/// right-button gesture, which moves the window by hand instead of asking Windows to.
pub fn start_window_drag(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        // The mouse is captured by the app at this point; the move loop needs it back.
        let _ = ReleaseCapture();
        SendMessageW(HWND(hwnd), WM_NCLBUTTONDOWN, WPARAM(HTCAPTION as usize), LPARAM(0));
    }
}

/// The mouse pointer's current position, in screen pixels.
pub fn cursor_pos() -> (i32, i32) {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    (pt.x, pt.y)
}

/// The window's current top-left corner, in screen pixels.
pub fn window_pos(hwnd: isize) -> (i32, i32) {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(HWND(hwnd), &mut rect);
    }
    (rect.left, rect.top)
}

/// Moves the window so its top-left corner is at `(x, y)` in screen pixels, without touching
/// its size or stacking order. Used to drag the window by hand (right-button hold), since the
/// left-button-only system move loop (`start_window_drag`) doesn't apply there.
pub fn move_window(hwnd: isize, x: i32, y: i32) {
    if hwnd == 0 {
        return;
    }
    unsafe {
        let _ = SetWindowPos(HWND(hwnd), HWND(0), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

pub fn open_path(path: &Path) {
    let file = path_wide(path);
    unsafe {
        ShellExecuteW(HWND(0), w!("open"), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// "PowerShell": a PowerShell window in the current folder — PowerShell 7 if it is installed,
/// else the Windows PowerShell that ships with the system. `admin` runs it elevated (Windows
/// asks for confirmation).
unsafe fn open_powershell(folder: Option<&Path>, admin: bool) {
    let Some(dir) = folder else { return };
    let dir = path_wide(dir);
    let verb = if admin { w!("runas") } else { w!("open") };
    for exe in [w!("pwsh.exe"), w!("powershell.exe")] {
        let r = ShellExecuteW(HWND(0), verb, exe, PCWSTR::null(), PCWSTR(dir.as_ptr()), SW_SHOWNORMAL);
        // ShellExecute reports success with a value above 32.
        if r.0 > 32 {
            return;
        }
    }
}

/// "Open in Explorer": the current folder, opened in the real Windows File Explorer, not miniExpo.
unsafe fn open_in_explorer(folder: Option<&Path>) {
    match folder {
        Some(path) => {
            let file = path_wide(path);
            ShellExecuteW(HWND(0), w!("open"), PCWSTR(file.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
        // My Computer level: there's no filesystem path to hand Explorer, so open This PC directly.
        None => {
            ShellExecuteW(
                HWND(0),
                w!("open"),
                w!("explorer.exe"),
                w!("shell:MyComputerFolder"),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

// ───────────────────────── icons ─────────────────────────

/// The shell's small icon for a folder / file type / single file, as PNG bytes.
pub fn file_icon_png(path: &Path, is_dir: bool, per_file: bool) -> Option<Vec<u8>> {
    unsafe {
        let mut info = SHFILEINFOW::default();
        let (name, attrs, flags) = if per_file {
            // ask the shell about this exact item (drive, .exe, shortcut …)
            (path_wide(path), FILE_FLAGS_AND_ATTRIBUTES(0), SHGFI_ICON | SHGFI_SMALLICON)
        } else if is_dir {
            // A fixed dummy name, never the real path: with SHGFI_USEFILEATTRIBUTES the shell
            // answers from the name alone, and a path that happens to be a drive root ("D:\",
            // which is what the ".." row carries) would come back as a disk icon.
            (wide("folder"), FILE_ATTRIBUTE_DIRECTORY, SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES)
        } else {
            let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
            (wide(&format!("file{ext}")), FILE_ATTRIBUTE_NORMAL, SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES)
        };
        let r = SHGetFileInfoW(PCWSTR(name.as_ptr()), attrs, Some(&mut info), size_of::<SHFILEINFOW>() as u32, flags);
        if r == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let png = icon_to_png(info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        png
    }
}

unsafe fn icon_to_png(icon: HICON) -> Option<Vec<u8>> {
    let mut ii = ICONINFO::default();
    GetIconInfo(icon, &mut ii).ok()?;
    let result = (|| {
        if ii.hbmColor.is_invalid() {
            return None;
        }
        let mut bm = BITMAP::default();
        if GetObjectW(HGDIOBJ(ii.hbmColor.0), size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void)) == 0 {
            return None;
        }
        let (w, h) = (bm.bmWidth, bm.bmHeight);
        if w <= 0 || h <= 0 {
            return None;
        }
        let hdc = GetDC(HWND(0));
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            ..Default::default()
        };
        let n = (w * h * 4) as usize;
        let mut color = vec![0u8; n];
        let mut mask = vec![0u8; n];
        let got = GetDIBits(hdc, ii.hbmColor, 0, h as u32, Some(color.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS);
        let got_mask = if ii.hbmMask.is_invalid() {
            0
        } else {
            GetDIBits(hdc, ii.hbmMask, 0, h as u32, Some(mask.as_mut_ptr() as *mut c_void), &mut bi, DIB_RGB_COLORS)
        };
        ReleaseDC(HWND(0), hdc);
        if got == 0 {
            return None;
        }
        // BGRA -> RGBA. Old-style icons have no alpha channel: take it from the AND mask instead.
        let has_alpha = color.chunks_exact(4).any(|p| p[3] != 0);
        let mut rgba = Vec::with_capacity(n);
        for (i, p) in color.chunks_exact(4).enumerate() {
            let a = if has_alpha {
                p[3]
            } else if got_mask != 0 && mask[i * 4] != 0 {
                0
            } else {
                255
            };
            rgba.extend_from_slice(&[p[2], p[1], p[0], a]);
        }
        encode_png(&rgba, w as u32, h as u32)
    })();
    let _ = DeleteObject(HGDIOBJ(ii.hbmColor.0));
    let _ = DeleteObject(HGDIOBJ(ii.hbmMask.0));
    result
}

fn encode_png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(out)
}

// ───────────────────────── taskbar number badge ─────────────────────────

/// 3x5 pixel digits for the badge.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];

/// 32x32 BGRA image: filled circle in `rgb` with the number on it.
fn badge_pixels(n: u32, rgb: u32) -> Vec<u8> {
    const S: i32 = 32;
    let (r, g, b) = (((rgb >> 16) & 0xFF) as u8, ((rgb >> 8) & 0xFF) as u8, (rgb & 0xFF) as u8);
    let luma = (299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000;
    let ink: u8 = if luma > 160 { 0 } else { 255 };
    let mut px = vec![0u8; (S * S * 4) as usize];
    let c = (S as f32 - 1.) / 2.;
    for y in 0..S {
        for x in 0..S {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let cover = (c + 0.5 - d).clamp(0., 1.);
            if cover > 0. {
                let i = ((y * S + x) * 4) as usize;
                px[i] = b;
                px[i + 1] = g;
                px[i + 2] = r;
                px[i + 3] = (cover * 255.) as u8;
            }
        }
    }
    let text = n.min(99).to_string();
    let digits: Vec<usize> = text.bytes().map(|d| (d - b'0') as usize).collect();
    let scale = if digits.len() == 1 { 5 } else { 3 };
    let width = (digits.len() as i32 * 4 - 1) * scale; // 3 px per digit + 1 px gap
    let height = 5 * scale;
    let (ox, oy) = ((S - width) / 2, (S - height) / 2);
    for (k, d) in digits.iter().enumerate() {
        for row in 0..5 {
            for col in 0..3 {
                if DIGITS[*d][row] & (0b100 >> col) == 0 {
                    continue;
                }
                for dy in 0..scale {
                    for dx in 0..scale {
                        let x = ox + (k as i32 * 4 + col as i32) * scale + dx;
                        let y = oy + row as i32 * scale + dy;
                        let i = ((y * S + x) * 4) as usize;
                        px[i] = ink;
                        px[i + 1] = ink;
                        px[i + 2] = ink;
                        px[i + 3] = 255;
                    }
                }
            }
        }
    }
    px
}

unsafe fn badge_icon(n: u32, rgb: u32) -> Option<HICON> {
    let pixels = badge_pixels(n, rgb);
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: 32,
        biHeight: -32,
        biPlanes: 1,
        biBitCount: 32,
        ..Default::default()
    };
    let mut bits: *mut c_void = null_mut();
    let color = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(HGDIOBJ(color.0));
        return None;
    }
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
    let mask = CreateBitmap(32, 32, 1, 1, None);
    let info = ICONINFO { fIcon: BOOL(1), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
    let icon = CreateIconIndirect(&info).ok();
    let _ = DeleteObject(HGDIOBJ(color.0));
    let _ = DeleteObject(HGDIOBJ(mask.0));
    icon
}

/// Puts the instance number on the window's taskbar button. Returns false while the button does
/// not exist yet (the caller retries for a few seconds after start-up).
pub fn set_overlay_number(hwnd: isize, n: u32, rgb: u32) -> bool {
    unsafe {
        let Ok(tb) = CoCreateInstance::<_, ITaskbarList3>(&TaskbarList, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER)
        else {
            return false;
        };
        if tb.HrInit().is_err() {
            return false;
        }
        let Some(icon) = badge_icon(n, rgb) else { return false };
        let desc = wide(&format!("miniExpo {n}"));
        let ok = tb.SetOverlayIcon(HWND(hwnd), icon, PCWSTR(desc.as_ptr())).is_ok();
        let _ = DestroyIcon(icon);
        ok
    }
}

// ───────────────────────── the open windows, shared between processes ─────────────────────────

/// Instance numbers run 0..=98, so 99 slots of (hwnd, pid) cover them all.
const SLOTS: usize = 99;

/// Address of the shared block, mapped on first use. Page-file backed and zero-filled, so the
/// first process to ask for it creates it and the rest attach to the same memory.
fn registry() -> *mut i64 {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static ADDR: AtomicUsize = AtomicUsize::new(0);
    let cached = ADDR.load(Ordering::Acquire);
    if cached != 0 {
        return cached as *mut i64;
    }
    unsafe {
        let name = wide("Local\\miniExpo.Windows");
        let bytes = (SLOTS * 2 * size_of::<i64>()) as u32;
        let Ok(mapping) = CreateFileMappingW(INVALID_HANDLE_VALUE, None, PAGE_READWRITE, 0, bytes, PCWSTR(name.as_ptr()))
        else {
            return null_mut();
        };
        let view = MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, 0);
        // The mapping handle stays open for the process's lifetime, which keeps the view valid.
        let addr = view.Value as usize;
        ADDR.store(addr, Ordering::Release);
        addr as *mut i64
    }
}

/// Publishes this window so the other instances' hotkeys can bring it to the front.
pub fn register_window(instance: u32, hwnd: isize) {
    let base = registry();
    if base.is_null() || instance as usize >= SLOTS {
        return;
    }
    unsafe {
        let slot = base.add(instance as usize * 2);
        *slot.add(1) = std::process::id() as i64;
        *slot = hwnd as i64; // written last: a reader never sees an hwnd without its pid
    }
}

/// The window registered for `instance`, if it is still alive. A dead process leaves its slot
/// behind, so the pid is checked too — an hwnd alone could have been handed to another window.
fn live_window(instance: usize) -> Option<HWND> {
    let base = registry();
    if base.is_null() || instance >= SLOTS {
        return None;
    }
    unsafe {
        let slot = base.add(instance * 2);
        let hwnd = HWND(*slot as isize);
        let pid = *slot.add(1) as u32;
        if hwnd.0 == 0 || !IsWindow(hwnd).as_bool() {
            return None;
        }
        let mut owner = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut owner));
        (owner == pid).then_some(hwnd)
    }
}

fn live_windows() -> Vec<HWND> {
    (0..SLOTS).filter_map(live_window).collect()
}

/// Brings `hwnd` to the front from a process that is not the foreground one, which Windows
/// otherwise refuses: borrowing the foreground thread's input state is the way around it.
unsafe fn activate_window(hwnd: HWND) {
    if IsIconic(hwnd).as_bool() {
        let _ = ShowWindow(hwnd, SW_RESTORE);
    }
    let ours = GetCurrentThreadId();
    let foreground = GetWindowThreadProcessId(GetForegroundWindow(), None);
    let attached = foreground != 0 && foreground != ours && AttachThreadInput(ours, foreground, true).as_bool();
    let _ = SetForegroundWindow(hwnd);
    let _ = BringWindowToTop(hwnd);
    if attached {
        let _ = AttachThreadInput(ours, foreground, false);
    }
}

/// Alt+Q: the next miniExpo window after whichever one is in front, wrapping around.
unsafe fn activate_next_window() {
    let windows = live_windows();
    if windows.is_empty() {
        return;
    }
    let front = GetForegroundWindow();
    let next = match windows.iter().position(|w| *w == front) {
        Some(i) => windows[(i + 1) % windows.len()],
        None => windows[0], // we are somewhere else entirely: start at the lowest number
    };
    activate_window(next);
}

// ───────────────────────── global hotkey: Win+Q ─────────────────────────

const VK_Q: u32 = 0x51;
const HOTKEY_ID: i32 = 0xA11;
const RETRY_TIMER: usize = 1;
/// An unassigned virtual key. Sending it keeps the Start menu shut when we swallow a Win+key
/// combination: the shell only opens Start if nothing happened between Win down and Win up.
const VK_FILLER: u16 = 0xE8;

/// Set while our Win+Q is held down, so auto-repeat opens one window, not thirty.
static HOTKEY_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Held by whichever process owns the hotkey; released when that process exits.
fn claim_hotkey_ownership() -> bool {
    let name = wide("Local\\miniExpo.Hotkey");
    unsafe {
        let Ok(handle) = CreateMutexW(None, false, PCWSTR(name.as_ptr())) else { return false };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            return false; // another miniExpo has it
        }
        // Never closed on purpose: the mutex must live as long as the process, and Windows
        // releases it when the process exits, which is what hands the hotkey to the next one.
        let _ = handle;
        true
    }
}

/// Takes Win+Q for this process. Windows keeps Win+Q for Search, so `RegisterHotKey` is refused
/// and we fall back to a low-level keyboard hook, which sees the key before the shell does and
/// swallows it. The hook lives on a thread of its own: Windows drops a hook whose thread stops
/// answering, and the shell thread blocks on file operations and modal dialogs.
unsafe fn try_register_hotkey(hwnd: HWND) -> bool {
    if !claim_hotkey_ownership() {
        return false;
    }
    // MOD_WIN | MOD_NOREPEAT — works only where nothing else claims Win+Q.
    if RegisterHotKey(hwnd, HOTKEY_ID, HOT_KEY_MODIFIERS(8 | 0x4000), VK_Q).is_ok() {
        return true;
    }
    let _ = std::thread::Builder::new().name("miniexpo-hotkey".into()).spawn(|| unsafe {
        if SetWindowsHookExW(WH_KEYBOARD_LL, Some(kbd_hook), HINSTANCE(0), 0).is_err() {
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
            if msg.message == WM_ACTIVATE_WINDOW {
                match msg.wParam.0 {
                    CYCLE => activate_next_window(),
                    n => {
                        if let Some(hwnd) = live_window(n) {
                            activate_window(hwnd);
                        }
                    }
                }
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
    true
}

fn win_key_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LWIN.0 as i32) | GetAsyncKeyState(VK_RWIN.0 as i32)) as u16 & 0x8000 != 0 }
}

/// Left Alt only — right Alt is AltGr on many layouts and must keep working.
fn left_alt_down() -> bool {
    unsafe { GetAsyncKeyState(VK_LMENU.0 as i32) as u16 & 0x8000 != 0 }
}

/// For the UI: is the Left Alt key held right now (Alt + drag moves the window).
pub fn left_alt_pressed() -> bool {
    left_alt_down()
}

/// Posted to the hook thread's own loop. Doing the work there instead of inside the hook
/// callback keeps the callback instant: Windows quietly drops a hook that answers too slowly.
const WM_ACTIVATE_WINDOW: u32 = WM_APP + 1;
/// wParam for "cycle to the next window"; 0..=8 mean that instance number (Alt+1 is instance 0,
/// the first window, through Alt+9 for instance 8). `usize::MAX` itself is already claimed by
/// Win+Q ("new window"), so this is the next value down — either way, distinct from every real
/// instance number, 0 included, so it cannot collide with Alt+1's jump target.
const CYCLE: usize = usize::MAX - 1;

unsafe fn send_filler_key() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(VK_FILLER), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    SendInput(&inputs, size_of::<INPUT>() as i32);
}

unsafe extern "system" fn kbd_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let msg = wp.0 as u32;
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        // Ignore anything we (or another tool) injected, or the filler key would loop.
        let injected = kb.flags.contains(LLKHF_INJECTED);
        // Win+Q opens a window; Left Alt+Q cycles through the open ones; Left Alt+0..9 jumps
        // straight to instance 0..9 (the digit matches the instance number). All three are
        // swallowed so nothing else reacts to them.
        let action = if kb.vkCode == VK_Q && win_key_down() {
            Some(usize::MAX) // new window
        } else if left_alt_down() && !win_key_down() {
            match kb.vkCode {
                VK_Q => Some(CYCLE),
                // '0'..'9' on the number row and on the numeric keypad
                v if (0x30..=0x39).contains(&v) => Some((v - 0x30) as usize),
                v if (0x60..=0x69).contains(&v) => Some((v - 0x60) as usize),
                _ => None,
            }
        } else {
            None
        };
        if let Some(action) = action.filter(|_| !injected) {
            use std::sync::atomic::Ordering;
            if down && !HOTKEY_HELD.swap(true, Ordering::Relaxed) {
                if action == usize::MAX {
                    emit(ShellEvent::HotkeyNewWindow);
                } else {
                    let _ = PostThreadMessageW(GetCurrentThreadId(), WM_ACTIVATE_WINDOW, WPARAM(action), LPARAM(0));
                }
                // Keeps the Start menu (Win) and the menu bar (Alt) from reacting to the modifier.
                send_filler_key();
            }
            if up {
                HOTKEY_HELD.store(false, Ordering::Relaxed);
            }
            if down || up {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

// ───────────────────────── shell thread ─────────────────────────

enum Request {
    ContextMenu(ContextMenuRequest),
    Verb { owner: isize, targets: Vec<PathBuf>, folder: PathBuf, verb: &'static str, shift: bool },
    PopupList { pt: (i32, i32), items: Vec<MenuEntry> },
    CopyIn { owner: isize, files: Vec<PathBuf>, dest: PathBuf },
    PickColor { owner: isize },
    PickFolder { owner: isize, start: Option<PathBuf> },
}

static QUEUE: Mutex<VecDeque<Request>> = Mutex::new(VecDeque::new());
/// The shell thread's hidden window, so other threads can hand it work.
static SHELL_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static EVENTS: Mutex<Option<Sender<ShellEvent>>> = Mutex::new(None);

fn emit(ev: ShellEvent) {
    if let Ok(g) = EVENTS.lock() {
        if let Some(tx) = g.as_ref() {
            let _ = tx.send(ev);
        }
    }
}

pub struct Shell {
    hwnd: isize,
}

impl Shell {
    pub fn start(events: Sender<ShellEvent>) -> Shell {
        if let Ok(mut g) = EVENTS.lock() {
            *g = Some(events);
        }
        let (tx, rx) = std::sync::mpsc::channel::<isize>();
        let _ = std::thread::Builder::new().name("miniexpo-shell".into()).spawn(move || unsafe {
            let _ = OleInitialize(None);
            let hinst = GetModuleHandleW(PCWSTR::null()).map(|m| HINSTANCE(m.0)).unwrap_or_default();
            HINST.with(|h| h.set(hinst.0));
            let class = w!("miniExpoShellHost");
            let wc = WNDCLASSW { lpfnWndProc: Some(wnd_proc), hInstance: hinst, lpszClassName: class, ..Default::default() };
            RegisterClassW(&wc);
            let picker = WNDCLASSW {
                lpfnWndProc: Some(picker_proc),
                hInstance: hinst,
                lpszClassName: w!("miniExpoPicker"),
                ..Default::default()
            };
            RegisterClassW(&picker);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!(""), WS_POPUP, 0, 0, 0, 0, HWND(0), HMENU(0), hinst, None);
            if !try_register_hotkey(hwnd) {
                // Another miniExpo has it: ask again every few seconds in case that one closes.
                SetTimer(hwnd, RETRY_TIMER, 3000, None);
            }
            SHELL_HWND.store(hwnd.0, std::sync::atomic::Ordering::Relaxed);
            let _ = tx.send(hwnd.0);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
        Shell { hwnd: rx.recv().unwrap_or(0) }
    }

    fn post(&self, req: Request) {
        if self.hwnd == 0 {
            return;
        }
        if let Ok(mut q) = QUEUE.lock() {
            q.push_back(req);
        }
        unsafe {
            let _ = PostMessageW(HWND(self.hwnd), WM_APP, WPARAM(0), LPARAM(0));
        }
    }

    pub fn context_menu(&self, req: ContextMenuRequest) {
        self.post(Request::ContextMenu(req));
    }

    /// Runs a shell verb ("copy", "cut", "paste", "delete") on the items, or — when `targets` is
    /// empty — on the folder background.
    /// `shift` runs the verb as if Shift were held: "delete" then bypasses the Recycle Bin.
    pub fn verb(&self, owner: isize, targets: Vec<PathBuf>, folder: PathBuf, verb: &'static str, shift: bool) {
        self.post(Request::Verb { owner, targets, folder, verb, shift });
    }

    pub fn popup_list(&self, _owner: isize, pt: (i32, i32), items: Vec<MenuEntry>) {
        self.post(Request::PopupList { pt, items });
    }

    /// Freezes the screen and reports the colour under the user's next click.
    pub fn pick_color(&self, owner: isize) {
        self.post(Request::PickColor { owner });
    }

    /// Shows the standard Windows "choose a folder" dialog, starting at `start`.
    pub fn pick_folder(&self, owner: isize, start: Option<PathBuf>) {
        self.post(Request::PickFolder { owner, start });
    }
}

struct ActiveMenu {
    cm2: Option<IContextMenu2>,
    cm3: Option<IContextMenu3>,
    /// The popup being shown, and how many of our own items sit in front of the shell's.
    top: HMENU,
    shift: u32,
}

thread_local! {
    static ACTIVE: RefCell<Option<ActiveMenu>> = const { RefCell::new(None) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static HINST: Cell<isize> = const { Cell::new(0) };
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_APP => {
            // A modal menu keeps pumping messages; don't start the next request inside it.
            if BUSY.with(|b| b.replace(true)) {
                return LRESULT(0);
            }
            loop {
                let next = QUEUE.lock().ok().and_then(|mut q| q.pop_front());
                let Some(req) = next else { break };
                handle(hwnd, req);
            }
            BUSY.with(|b| b.set(false));
            LRESULT(0)
        }
        WM_HOTKEY => {
            emit(ShellEvent::HotkeyNewWindow);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == RETRY_TIMER => {
            if try_register_hotkey(hwnd) {
                let _ = KillTimer(hwnd, RETRY_TIMER);
            }
            LRESULT(0)
        }
        WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR => {
            // Owner-drawn / lazily-filled items of the shell menu ("Send to", "Open with", icons).
            let mut lp = lp;
            let handled = ACTIVE.with(|a| {
                let a = a.borrow();
                // Our entries pushed the shell's down by `shift`; tell the extension the position
                // it originally put a first-level submenu at.
                if msg == WM_INITMENUPOPUP {
                    if let Some(m) = a.as_ref() {
                        let count = GetMenuItemCount(m.top);
                        for i in 0..count.max(0) {
                            if GetSubMenu(m.top, i).0 == wp.0 as isize {
                                let pos = (i as u32).saturating_sub(m.shift);
                                lp = LPARAM(((lp.0 as u32 & 0xFFFF_0000) | (pos & 0xFFFF)) as i32 as isize);
                                break;
                            }
                        }
                    }
                }
                let Some(m) = a.as_ref() else { return None };
                if let Some(cm3) = &m.cm3 {
                    let mut res = LRESULT(0);
                    let hr = cm3.HandleMenuMsg2(msg, wp, lp, Some(&mut res));
                    return hr.ok().map(|_| res);
                }
                if let Some(cm2) = &m.cm2 {
                    let hr = if msg != WM_MENUCHAR { cm2.HandleMenuMsg(msg, wp, lp) } else { Ok(()) };
                    if msg != WM_MENUCHAR && hr.is_ok() {
                        return Some(LRESULT(if msg == WM_INITMENUPOPUP { 0 } else { 1 }));
                    }
                    return None;
                }
                None
            });
            match handled {
                Some(r) => r,
                None => DefWindowProcW(hwnd, msg, wp, lp),
            }
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn handle(hwnd: HWND, req: Request) {
    match req {
        Request::ContextMenu(r) => do_context_menu(hwnd, r),
        // Delete goes straight to IFileOperation: synchronous, so the listing can be re-read the
        // moment it returns, and it skips building a context menu, whose first use in a process
        // loads every shell extension on the machine and can stall for seconds.
        Request::Verb { owner, targets, verb: "delete", shift, .. } => do_delete(owner, &targets, shift),
        Request::Verb { owner, targets, folder, verb, shift } => do_verb(hwnd, owner, &targets, &folder, verb, shift),
        Request::PopupList { pt, items } => do_popup_list(hwnd, pt, items),
        Request::CopyIn { owner, files, dest } => do_copy_in(owner, &files, &dest),
        Request::PickColor { owner } => do_pick_color(owner),
        Request::PickFolder { owner, start } => do_pick_folder(owner, start),
    }
}

/// The Open button: the system folder picker. The chosen folder is opened in the list.
unsafe fn do_pick_folder(owner: isize, start: Option<PathBuf>) {
    let dlg: IFileOpenDialog =
        match CoCreateInstance(&FileOpenDialog, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER) {
            Ok(d) => d,
            Err(_) => return,
        };
    let opts = dlg.GetOptions().unwrap_or(FILEOPENDIALOGOPTIONS(0));
    let _ = dlg.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST);
    let _ = dlg.SetTitle(w!("Open folder"));
    // Start the dialog where the window currently is.
    if let Some(p) = start {
        let wp = path_wide(&p);
        if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wp.as_ptr()), None::<&IBindCtx>) {
            let _ = dlg.SetFolder(&item);
        }
    }
    // Show returns an error when the user cancels.
    if dlg.Show(HWND(owner)).is_ok() {
        if let Ok(item) = dlg.GetResult() {
            if let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                if let Ok(path) = name.to_string() {
                    emit(ShellEvent::OpenFolder(PathBuf::from(path)));
                }
                CoTaskMemFree(Some(name.0 as *const c_void));
            }
        }
    }
    if owner != 0 {
        let _ = SetForegroundWindow(HWND(owner));
    }
}

// ── PIDL / IContextMenu plumbing ──

/// Owns the absolute PIDLs for the lifetime of a request.
struct Pidls(Vec<*mut ITEMIDLIST>);

impl Drop for Pidls {
    fn drop(&mut self) {
        for p in &self.0 {
            unsafe { CoTaskMemFree(Some(*p as *const c_void)) };
        }
    }
}

unsafe fn parse_pidl(path: &Path) -> Option<*mut ITEMIDLIST> {
    let w = path_wide(path);
    let mut pidl: *mut ITEMIDLIST = null_mut();
    SHParseDisplayName(PCWSTR(w.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None).ok()?;
    if pidl.is_null() {
        None
    } else {
        Some(pidl)
    }
}

/// The context menu object for the items (all in one folder), or — with no targets — for the
/// background of `folder`.
///
/// The folder is parsed from its path, but the items are parsed by their name *relative* to it:
/// `SHParseDisplayName` on a full path gives up past MAX_PATH (260 characters), which is easy to
/// hit with, say, an exported .msg whose name is the mail's subject — those items would come back
/// with no system menu at all.
unsafe fn menu_for(owner: HWND, targets: &[PathBuf], folder: Option<&Path>) -> Option<(IContextMenu, Pidls)> {
    let dir: PathBuf = match folder {
        Some(f) => f.to_path_buf(),
        None => targets.first()?.parent()?.to_path_buf(),
    };
    let mut pidls = Pidls(Vec::new());
    let dir_pidl = parse_pidl(&dir)?;
    pidls.0.push(dir_pidl);
    let shell_folder: IShellFolder = SHBindToObject(None::<&IShellFolder>, dir_pidl, None::<&IBindCtx>).ok()?;

    if targets.is_empty() {
        let cm: IContextMenu = shell_folder.CreateViewObject(owner).ok()?;
        return Some((cm, pidls));
    }

    let first_child = pidls.0.len();
    for t in targets {
        let Some(name) = t.file_name() else { continue };
        let w = wide(&name.to_string_lossy());
        let mut child: *mut ITEMIDLIST = null_mut();
        let mut attrs = 0u32;
        let ok = shell_folder
            .ParseDisplayName(owner, None::<&IBindCtx>, PCWSTR(w.as_ptr()), None, &mut child, &mut attrs)
            .is_ok();
        if ok && !child.is_null() {
            pidls.0.push(child);
        }
    }
    let kids: Vec<*const ITEMIDLIST> = pidls.0[first_child..].iter().map(|p| *p as *const ITEMIDLIST).collect();
    if kids.is_empty() {
        return None;
    }
    let cm: IContextMenu = shell_folder.GetUIObjectOf(owner, &kids, None).ok()?;
    Some((cm, pidls))
}

const ID_FIRST: u32 = 1;
const ID_LAST: u32 = 0x7FFF;
const ID_REFRESH: u32 = 0x8001;
const ID_NEW_WINDOW: u32 = 0x8002;
const ID_FAVORITE: u32 = 0x8003;
const ID_OPEN_EXPLORER: u32 = 0x8004;
const ID_POWERSHELL: u32 = 0x8005;

unsafe fn invoke(
    cm: &IContextMenu,
    owner: HWND,
    pt: (i32, i32),
    by_offset: Option<u32>,
    verb: Option<&str>,
    shift: bool,
    dir: Option<&Path>,
) {
    let mut info = CMINVOKECOMMANDINFOEX::default();
    info.cbSize = size_of::<CMINVOKECOMMANDINFOEX>() as u32;
    // 0x4000 = CMIC_MASK_UNICODE (lpVerbW / lpParametersW are valid)
    info.fMask = 0x4000 | CMIC_MASK_PTINVOKE;
    if shift {
        // What the shell reads to turn "delete" into a permanent delete, "open" into a new
        // window, and so on — the same as holding Shift while clicking the menu item.
        info.fMask |= CMIC_MASK_SHIFT_DOWN;
    }
    info.hwnd = owner;
    info.nShow = SW_SHOWNORMAL.0;
    info.ptInvoke = POINT { x: pt.0, y: pt.1 };
    // These two must outlive InvokeCommand.
    let verb_a;
    let verb_w;
    if let Some(off) = by_offset {
        // MAKEINTRESOURCE(offset): the command id relative to idCmdFirst
        info.lpVerb = PCSTR(off as usize as *const u8);
        info.lpVerbW = PCWSTR(off as usize as *const u16);
    } else if let Some(v) = verb {
        verb_a = CString::new(v).unwrap_or_default();
        verb_w = wide(v);
        info.lpVerb = PCSTR(verb_a.as_ptr() as *const u8);
        info.lpVerbW = PCWSTR(verb_w.as_ptr());
    }
    // Verbs on a folder's blank area ("Search Everything", "Open in Terminal", ...) expand %V from
    // the working directory; without it the shell has nothing to run them against and fails with
    // "No application is associated with the specified file" (0x80070483).
    let dir_a;
    let dir_w;
    if let Some(d) = dir {
        dir_a = CString::new(d.to_string_lossy().as_bytes()).unwrap_or_default();
        dir_w = path_wide(d);
        info.lpDirectory = PCSTR(dir_a.as_ptr() as *const u8);
        info.lpDirectoryW = PCWSTR(dir_w.as_ptr());
    }
    let _ = cm.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO);
}

unsafe fn command_verb(cm: &IContextMenu, id: u32) -> Option<String> {
    let mut buf = [0u8; 256];
    cm.GetCommandString((id - ID_FIRST) as usize, GCS_VERBA, None, PSTR(buf.as_mut_ptr()), buf.len() as u32).ok()?;
    let len = buf.iter().position(|b| *b == 0).unwrap_or(0);
    Some(String::from_utf8_lossy(&buf[..len]).to_lowercase())
}

/// A menu-sized bitmap of one of our own icons, in the premultiplied-alpha form Windows draws
/// next to a menu item. The caller deletes it once the menu is gone.
unsafe fn menu_bitmap(icon: menu_icons::Icon) -> Option<HBITMAP> {
    let size = (GetSystemMetrics(SM_CXSMICON).max(16)) as usize;
    let pixels = menu_icons::render(icon, size);
    let mut bmi = BITMAPINFO::default();
    bmi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size as i32,
        biHeight: -(size as i32), // top-down
        biPlanes: 1,
        biBitCount: 32,
        biCompression: 0, // BI_RGB
        ..Default::default()
    };
    let mut bits: *mut c_void = null_mut();
    let bmp = CreateDIBSection(HDC(0), &bmi, DIB_RGB_COLORS, &mut bits, HANDLE(0), 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(bmp);
        return None;
    }
    std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
    Some(bmp)
}

/// Puts `icon` next to the menu entry with command id `id`.
unsafe fn set_menu_icon(hmenu: HMENU, id: u32, icon: menu_icons::Icon, made: &mut Vec<HBITMAP>) {
    let Some(bmp) = menu_bitmap(icon) else { return };
    let mii = MENUITEMINFOW { cbSize: size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_BITMAP, hbmpItem: bmp, ..Default::default() };
    if SetMenuItemInfoW(hmenu, id, false, &mii).is_ok() {
        made.push(bmp);
    } else {
        let _ = DeleteObject(bmp);
    }
}

/// On a folder's blank area, two shell submenus come back empty in every host but Explorer
/// itself ("PowerShell 7" and "Give access to": they need Explorer's folder view to fill in), so
/// they are taken out rather than shown as dead ends. Separators left doubled up are dropped too.
unsafe fn drop_dead_blank_items(hmenu: HMENU) {
    let text_of = |i: i32| -> String {
        let mut buf = [0u16; 128];
        let n = GetMenuStringW(hmenu, i as u32, Some(&mut buf), MF_BYPOSITION);
        String::from_utf16_lossy(&buf[..n.max(0) as usize]).replace('&', "").to_lowercase()
    };
    let is_sep = |i: i32| -> bool {
        let mut mii = MENUITEMINFOW { cbSize: size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_FTYPE, ..Default::default() };
        GetMenuItemInfoW(hmenu, i as u32, true, &mut mii).is_ok() && (mii.fType.0 & MFT_SEPARATOR.0) != 0
    };
    let mut i = GetMenuItemCount(hmenu) - 1;
    while i >= 0 {
        if !GetSubMenu(hmenu, i).is_invalid() {
            let t = text_of(i);
            if t.starts_with("powershell") || t.starts_with("give access to") {
                let _ = RemoveMenu(hmenu, i as u32, MF_BYPOSITION);
            }
        }
        i -= 1;
    }
    let mut i = GetMenuItemCount(hmenu) - 1;
    while i >= 0 {
        let last = i == GetMenuItemCount(hmenu) - 1;
        if is_sep(i) && (i == 0 || last || is_sep(i + 1) || is_sep(i - 1)) {
            let _ = RemoveMenu(hmenu, i as u32, MF_BYPOSITION);
        }
        i -= 1;
    }
}

unsafe fn do_context_menu(hwnd: HWND, r: ContextMenuRequest) {
    let cm = menu_for(hwnd, &r.targets, r.folder.as_deref());
    let Ok(hmenu) = CreatePopupMenu() else { return };

    // The shell builds its menu first, at index 0 and alone in the popup — the shape every
    // extension is written against — and our own entries are put in front afterwards.
    //
    // Shell_MergeMenus was tried for this and is wrong: it *clones* submenus into new HMENUs, and
    // lazily-filled submenus ("PowerShell 7", WinRAR) are populated by whichever extension
    // recognises the HMENU in WM_INITMENUPOPUP — it never sees a clone it didn't create.
    //
    // Inserting in front moves every shell item down by `shift`. The submenu handles are
    // unchanged, but WM_INITMENUPOPUP also says *where* the submenu sits in its parent, and some
    // extensions match on that; the window procedure subtracts `shift` again (see ActiveMenu).
    if let Some((cm, _)) = &cm {
        let mut flags = CMF_NORMAL | CMF_CANRENAME;
        if r.extended {
            flags |= CMF_EXTENDEDVERBS;
        }
        let _ = cm.QueryContextMenu(hmenu, 0, ID_FIRST, ID_LAST, flags);
        let cm2 = cm.cast::<IContextMenu2>().ok();
        let cm3 = cm.cast::<IContextMenu3>().ok();
        if r.targets.is_empty() {
            drop_dead_blank_items(hmenu);
        }
        ACTIVE.with(|a| *a.borrow_mut() = Some(ActiveMenu { cm2, cm3, top: hmenu, shift: 0 }));
    } else {
    }

    let mut at = 0;
    let refresh = wide("Refresh\tF5");
    if InsertMenuW(hmenu, at, MF_BYPOSITION | MF_STRING, ID_REFRESH as usize, PCWSTR(refresh.as_ptr())).is_ok() {
        at += 1;
    }
    let open_explorer = wide("Open in Explorer");
    if InsertMenuW(hmenu, at, MF_BYPOSITION | MF_STRING, ID_OPEN_EXPLORER as usize, PCWSTR(open_explorer.as_ptr()))
        .is_ok()
    {
        at += 1;
    }
    let powershell = wide("PowerShell");
    if r.folder.is_some()
        && InsertMenuW(hmenu, at, MF_BYPOSITION | MF_STRING, ID_POWERSHELL as usize, PCWSTR(powershell.as_ptr())).is_ok()
    {
        at += 1;
    }
    let label = wide("New Window");
    if InsertMenuW(hmenu, at, MF_BYPOSITION | MF_STRING, ID_NEW_WINDOW as usize, PCWSTR(label.as_ptr())).is_ok() {
        at += 1;
    }
    let fav_label = wide(if r.fav_is_set { "Remove Favorite" } else { "Add Favorite" });
    if r.fav_target.is_some()
        && InsertMenuW(hmenu, at, MF_BYPOSITION | MF_STRING, ID_FAVORITE as usize, PCWSTR(fav_label.as_ptr())).is_ok()
    {
        at += 1;
    }
    if cm.is_some() && InsertMenuW(hmenu, at, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null()).is_ok() {
        at += 1;
    }
    ACTIVE.with(|a| {
        if let Some(m) = a.borrow_mut().as_mut() {
            m.shift = at as u32;
        }
    });
    let mut icons = Vec::new();
    set_menu_icon(hmenu, ID_REFRESH, menu_icons::Icon::Refresh, &mut icons);
    set_menu_icon(hmenu, ID_OPEN_EXPLORER, menu_icons::Icon::Explorer, &mut icons);
    set_menu_icon(hmenu, ID_POWERSHELL, menu_icons::Icon::PowerShell, &mut icons);
    set_menu_icon(hmenu, ID_NEW_WINDOW, menu_icons::Icon::NewWindow, &mut icons);
    set_menu_icon(hmenu, ID_FAVORITE, menu_icons::Icon::Favorite, &mut icons);
    let _ = SetForegroundWindow(hwnd);
    let picked = TrackPopupMenuEx(hmenu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, r.pt.0, r.pt.1, hwnd, None).0 as u32;
    // Shift held while choosing "PowerShell" = run it as administrator.
    let shift_held = GetAsyncKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000 != 0;
    ACTIVE.with(|a| *a.borrow_mut() = None);
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    // The menu was tracked on this thread's hidden window, which now holds the foreground.
    restore_focus(r.owner);

    match picked {
        0 => {}
        ID_REFRESH => emit(ShellEvent::ReloadNow),
        ID_OPEN_EXPLORER => open_in_explorer(r.folder.as_deref()),
        ID_POWERSHELL => open_powershell(r.folder.as_deref(), shift_held),
        ID_NEW_WINDOW => emit(ShellEvent::NewWindow),
        ID_FAVORITE => {
            if let Some(t) = r.fav_target.clone() {
                emit(ShellEvent::ToggleFavorite(t));
            }
        }
        id => {
            if let Some((cm, _)) = &cm {
                let verb = command_verb(cm, id);
                match verb.as_deref() {
                    // "Open" on a folder browses into it inside miniExpo instead of launching Explorer.
                    Some("open") if r.targets.len() == 1 && r.targets[0].is_dir() => {
                        emit(ShellEvent::OpenFolder(r.targets[0].clone()))
                    }
                    Some("rename") if r.targets.len() == 1 => emit(ShellEvent::Rename(r.targets[0].clone())),
                    _ => {
                        invoke(cm, HWND(r.owner), r.pt, Some(id - ID_FIRST), None, r.extended, r.folder.as_deref());
                        restore_focus(r.owner);
                        emit(ShellEvent::Refresh);
                    }
                }
            } else {
            }
        }
    }
    let _ = DestroyMenu(hmenu);
    for bmp in icons {
        let _ = DeleteObject(bmp);
    }
}

unsafe fn do_verb(hwnd: HWND, owner: isize, targets: &[PathBuf], folder: &Path, verb: &str, shift: bool) {
    let Some((cm, _pidls)) = menu_for(hwnd, targets, Some(folder)) else { return };
    let Ok(hmenu) = CreatePopupMenu() else { return };
    // Verbs only exist after the menu has been built once.
    if cm.QueryContextMenu(hmenu, 0, ID_FIRST, ID_LAST, CMF_NORMAL | CMF_CANRENAME).is_ok() {
        invoke(&cm, HWND(owner), (0, 0), None, Some(verb), shift, Some(folder));
        restore_focus(owner);
        emit(ShellEvent::Refresh);
    }
    let _ = DestroyMenu(hmenu);
}

/// What picking a `do_popup_list` entry should do once the menu closes.
enum PopupAction {
    /// Navigate miniExpo into this folder (a drive or a user favorite).
    Navigate(PathBuf),
    /// Open this file with its default app, e.g. the "favorites.json" shortcut.
    OpenFile(PathBuf),
    /// Report this command back to the UI thread, e.g. an address-bar menu action.
    Report(super::MenuCommand),
}

unsafe fn do_popup_list(hwnd: HWND, pt: (i32, i32), items: Vec<MenuEntry>) {
    let Ok(hmenu) = CreatePopupMenu() else { return };
    let mut actions: Vec<PopupAction> = Vec::new();
    for item in &items {
        match item {
            MenuEntry::Item { text, path } => {
                // a lone '&' would turn into a keyboard-shortcut underline
                let t = wide(&text.replace('&', "&&"));
                let id = 1000 + actions.len();
                let _ = AppendMenuW(hmenu, MF_STRING, id, PCWSTR(t.as_ptr()));
                actions.push(PopupAction::Navigate(path.clone()));
            }
            MenuEntry::OpenFile { text, path } => {
                let t = wide(&text.replace('&', "&&"));
                let id = 1000 + actions.len();
                let _ = AppendMenuW(hmenu, MF_STRING, id, PCWSTR(t.as_ptr()));
                actions.push(PopupAction::OpenFile(path.clone()));
            }
            MenuEntry::Action { text, command } => {
                let t = wide(&text.replace('&', "&&"));
                let id = 1000 + actions.len();
                let _ = AppendMenuW(hmenu, MF_STRING, id, PCWSTR(t.as_ptr()));
                actions.push(PopupAction::Report(*command));
            }
            MenuEntry::Separator => {
                let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
            }
        }
    }
    let _ = SetForegroundWindow(hwnd);
    let picked = TrackPopupMenuEx(hmenu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, pt.0, pt.1, hwnd, None).0 as usize;
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    if picked >= 1000 {
        match actions.get(picked - 1000) {
            Some(PopupAction::Navigate(p)) => emit(ShellEvent::Navigate(p.clone())),
            Some(PopupAction::OpenFile(p)) => open_path(p),
            Some(PopupAction::Report(a)) => emit(ShellEvent::MenuCommand(*a)),
            None => {}
        }
    }
    let _ = DestroyMenu(hmenu);
}

/// Queues a copy on the shell thread; used by the drop target, which runs on the UI thread.
pub(super) fn copy_files_into(owner: isize, files: &[PathBuf], dest: &Path) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push_back(Request::CopyIn { owner, files: files.to_vec(), dest: dest.to_path_buf() });
    }
    unsafe {
        let _ = PostMessageW(HWND(SHELL_HWND.load(std::sync::atomic::Ordering::Relaxed)), WM_APP, WPARAM(0), LPARAM(0));
    }
}

/// An IShellItem for `path`, built from its folder plus its *relative* name so that paths past
/// MAX_PATH work — parsing the full string, as `SHCreateItemFromParsingName` does, gives up there.
unsafe fn shell_item(path: &Path) -> Option<IShellItem> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        // A drive root has no parent to bind; its path is always short.
        let w = path_wide(path);
        return SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(w.as_ptr()), None::<&IBindCtx>).ok();
    };
    let dir_pidl = parse_pidl(parent)?;
    let _dir = Pidls(vec![dir_pidl]);
    let folder: IShellFolder = SHBindToObject(None::<&IShellFolder>, dir_pidl, None::<&IBindCtx>).ok()?;
    let w = wide(&name.to_string_lossy());
    let mut child: *mut ITEMIDLIST = null_mut();
    let mut attrs = 0u32;
    folder.ParseDisplayName(HWND(0), None::<&IBindCtx>, PCWSTR(w.as_ptr()), None, &mut child, &mut attrs).ok()?;
    if child.is_null() {
        return None;
    }
    let _child = Pidls(vec![child]);
    // The item keeps its own copy of the pidl, so ours can be freed.
    SHCreateItemWithParent(None, &folder, child).ok()
}

fn file_operation(owner: isize, flags: FILEOPERATION_FLAGS) -> Option<IFileOperation> {
    unsafe {
        let op: IFileOperation =
            CoCreateInstance(&FileOperation, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER).ok()?;
        let _ = op.SetOperationFlags(flags);
        // The miniExpo window, not this thread's hidden one: Windows hands the focus back to a
        // dialog's owner when it closes, and a hidden owner leaves the user clicking to return.
        let _ = op.SetOwnerWindow(HWND(owner));
        Some(op)
    }
}

/// Puts the keyboard focus back on the window the user was working in.
unsafe fn restore_focus(owner: isize) {
    if owner != 0 {
        let _ = SetForegroundWindow(HWND(owner));
    }
}

/// Delete → Recycle Bin, no question asked (it can be undone, and that is Windows' default);
/// Shift+Delete → gone for good, and Windows asks first.
unsafe fn do_delete(owner: isize, targets: &[PathBuf], permanent: bool) {
    let flags = if permanent { FILEOPERATION_FLAGS(0) } else { FOF_ALLOWUNDO | FOFX_RECYCLEONDELETE | FOF_NOCONFIRMATION };
    let Some(op) = file_operation(owner, flags) else { return };
    let mut queued = false;
    for t in targets {
        if let Some(item) = shell_item(t) {
            queued |= op.DeleteItem(&item, None::<&IFileOperationProgressSink>).is_ok();
        }
    }
    if queued {
        let _ = op.PerformOperations(); // returns once the files are gone (or the user said no)
        restore_focus(owner); // the confirmation dialog takes the focus with it otherwise
    }
    emit(ShellEvent::Refresh);
}

/// Copies dropped files into `dest`, with Explorer's own progress and name-conflict dialogs.
unsafe fn do_copy_in(owner: isize, files: &[PathBuf], dest: &Path) {
    let Some(op) = file_operation(owner, FOF_ALLOWUNDO) else { return };
    let Some(dest_item) = shell_item(dest) else { return };
    let mut names = Vec::new();
    for f in files {
        let Some(item) = shell_item(f) else { continue };
        if op.CopyItem(&item, &dest_item, PCWSTR::null(), None::<&IFileOperationProgressSink>).is_ok() {
            if let Some(n) = f.file_name() {
                names.push(n.to_string_lossy().into_owned());
            }
        }
    }
    if !names.is_empty() {
        let _ = op.PerformOperations();
        restore_focus(owner); // a name clash or an elevation prompt would otherwise keep it
    }
    emit(ShellEvent::Arrived(names));
}

/// Drags these files (all in one folder) out of the window: the shell's own data object and drag
/// loop, so Explorer, other miniExpo windows, mail clients and the like take them as they would
/// from Explorer — drag image, copy / move / link cursors and all. Returns when the button is
/// released. Must be called on the UI thread with the left button still down.
pub fn drag_files_out(owner: isize, paths: &[PathBuf]) {
    unsafe {
        let _ = OleInitialize(None);
        let Some(dir) = paths.first().and_then(|p| p.parent()) else { return };
        let Some(dir_pidl) = parse_pidl(dir) else { return };
        let mut pidls = Pidls(vec![dir_pidl]);
        let Ok(folder) = SHBindToObject::<_, _, IShellFolder>(None::<&IShellFolder>, dir_pidl, None::<&IBindCtx>) else {
            return;
        };
        for t in paths {
            let Some(name) = t.file_name() else { continue };
            let w = wide(&name.to_string_lossy());
            let mut child: *mut ITEMIDLIST = null_mut();
            let mut attrs = 0u32;
            let ok = folder
                .ParseDisplayName(HWND(owner), None::<&IBindCtx>, PCWSTR(w.as_ptr()), None, &mut child, &mut attrs)
                .is_ok();
            if ok && !child.is_null() {
                pidls.0.push(child);
            }
        }
        let kids: Vec<*const ITEMIDLIST> = pidls.0[1..].iter().map(|p| *p as *const ITEMIDLIST).collect();
        if kids.is_empty() {
            return;
        }
        let Ok(data) = folder.GetUIObjectOf::<_, IDataObject>(HWND(owner), &kids, None) else { return };
        dnd::set_dragging_out(true);
        let _ = SHDoDragDrop(
            HWND(owner),
            &data,
            None::<&IDropSource>,
            DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0 | DROPEFFECT_LINK.0),
        );
        dnd::set_dragging_out(false);
        restore_focus(owner);
        emit(ShellEvent::Refresh);
    }
}

// ───────────────────────── screen colour picker ─────────────────────────

/// A frozen copy of the whole (virtual) screen, used both as the overlay's backdrop and as the
/// source of the picked pixel — so what the user sees is exactly what gets picked.
struct Shot {
    dc: HDC,
    bmp: HBITMAP,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Shot {
    /// BGRX, top-down.
    fn pixel(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return None;
        }
        let i = ((y as isize * self.w as isize + x as isize) * 4) as usize;
        unsafe {
            let p = self.bits.add(i);
            Some(((*p.add(2) as u32) << 16) | ((*p.add(1) as u32) << 8) | *p as u32)
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Pick {
    Running,
    Done(u32),
    Cancelled,
}

thread_local! {
    static SHOT: RefCell<Option<Shot>> = const { RefCell::new(None) };
    static PICK: Cell<Pick> = const { Cell::new(Pick::Cancelled) };
    static CURSOR: Cell<(i32, i32)> = const { Cell::new((-1000, -1000)) };
}

/// The preview swatch drawn next to the cursor.
const SW_DX: i32 = 18;
const SW_DY: i32 = 18;
const SW_W: i32 = 112;
const SW_H: i32 = 26;

fn swatch_rect(x: i32, y: i32) -> RECT {
    RECT { left: x + SW_DX, top: y + SW_DY, right: x + SW_DX + SW_W, bottom: y + SW_DY + SW_H }
}

unsafe fn capture_screen() -> Option<(Shot, i32, i32)> {
    let x0 = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let y0 = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let w = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
    let h = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
    let screen = GetDC(HWND(0));
    let dc = CreateCompatibleDC(screen);
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w,
        biHeight: -h, // top-down
        biPlanes: 1,
        biBitCount: 32,
        ..Default::default()
    };
    let mut bits: *mut c_void = null_mut();
    let bmp = CreateDIBSection(screen, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok();
    let Some(bmp) = bmp.filter(|_| !bits.is_null()) else {
        let _ = DeleteDC(dc);
        ReleaseDC(HWND(0), screen);
        return None;
    };
    SelectObject(dc, HGDIOBJ(bmp.0));
    let ok = BitBlt(dc, 0, 0, w, h, screen, x0, y0, SRCCOPY).is_ok();
    ReleaseDC(HWND(0), screen);
    if !ok {
        let _ = DeleteDC(dc);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        return None;
    }
    Some((Shot { dc, bmp, bits: bits as *mut u8, w, h }, x0, y0))
}

unsafe fn do_pick_color(owner: isize) {
    let Some((shot, x0, y0)) = capture_screen() else {
        emit(ShellEvent::ColorPickCancelled);
        return;
    };
    let (w, h) = (shot.w, shot.h);
    SHOT.with(|s| *s.borrow_mut() = Some(shot));
    PICK.with(|p| p.set(Pick::Running));
    CURSOR.with(|c| c.set((-1000, -1000)));

    let hinst = HINSTANCE(HINST.with(|h| h.get()));
    let overlay = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        w!("miniExpoPicker"),
        w!("miniExpo colour picker"),
        WS_POPUP,
        x0,
        y0,
        w,
        h,
        HWND(0),
        HMENU(0),
        hinst,
        None,
    );
    if overlay.0 != 0 {
        let _ = ShowWindow(overlay, SW_SHOW);
        let _ = SetForegroundWindow(overlay);
        let mut msg = MSG::default();
        while PICK.with(|p| p.get()) == Pick::Running {
            if !GetMessageW(&mut msg, HWND(0), 0, 0).as_bool() {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyWindow(overlay);
    }

    SHOT.with(|s| {
        if let Some(shot) = s.borrow_mut().take() {
            let _ = DeleteDC(shot.dc);
            let _ = DeleteObject(HGDIOBJ(shot.bmp.0));
        }
    });
    match PICK.with(|p| p.get()) {
        Pick::Done(c) => emit(ShellEvent::ColorPicked(c)),
        _ => emit(ShellEvent::ColorPickCancelled),
    }
    if owner != 0 {
        let _ = SetForegroundWindow(HWND(owner));
    }
}

unsafe extern "system" fn picker_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            if let Ok(c) = LoadCursorW(HINSTANCE(0), IDC_CROSS) {
                SetCursor(c);
            }
            LRESULT(1)
        }
        WM_MOUSEMOVE => {
            let (x, y) = (lp.0 as i16 as i32, (lp.0 >> 16) as i16 as i32);
            let (ox, oy) = CURSOR.with(|c| c.replace((x, y)));
            let old = swatch_rect(ox, oy);
            let new = swatch_rect(x, y);
            let _ = InvalidateRect(hwnd, Some(&old), false);
            let _ = InvalidateRect(hwnd, Some(&new), false);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = (lp.0 as i16 as i32, (lp.0 >> 16) as i16 as i32);
            let picked = SHOT.with(|s| s.borrow().as_ref().and_then(|shot| shot.pixel(x, y)));
            PICK.with(|p| p.set(picked.map(Pick::Done).unwrap_or(Pick::Cancelled)));
            LRESULT(0)
        }
        WM_RBUTTONDOWN => {
            PICK.with(|p| p.set(Pick::Cancelled));
            LRESULT(0)
        }
        WM_KEYDOWN => {
            if wp.0 as u32 == 0x1B {
                // VK_ESCAPE
                PICK.with(|p| p.set(Pick::Cancelled));
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_picker(hdc, &ps.rcPaint);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn paint_picker(hdc: HDC, clip: &RECT) {
    let (cx, cy) = CURSOR.with(|c| c.get());
    SHOT.with(|s| {
        let s = s.borrow();
        let Some(shot) = s.as_ref() else { return };
        let (x, y) = (clip.left, clip.top);
        let (w, h) = (clip.right - clip.left, clip.bottom - clip.top);
        let _ = BitBlt(hdc, x, y, w, h, shot.dc, x, y, SRCCOPY);

        // colour preview beside the cursor
        let Some(color) = shot.pixel(cx, cy) else { return };
        let r = swatch_rect(cx, cy);
        let bgr = COLORREF(((color & 0xFF) << 16) | (color & 0xFF00) | (color >> 16));
        let fill: HBRUSH = CreateSolidBrush(bgr);
        let back: HBRUSH = CreateSolidBrush(COLORREF(0x00FF_FFFF));
        let frame: HBRUSH = CreateSolidBrush(COLORREF(0x0020_2020));
        FillRect(hdc, &r, back);
        let chip = RECT { left: r.left + 4, top: r.top + 4, right: r.left + 22, bottom: r.bottom - 4 };
        FillRect(hdc, &chip, fill);
        FrameRect(hdc, &chip, frame);
        FrameRect(hdc, &r, frame);
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x0020_2020));
        SelectObject(hdc, GetStockObject(DEFAULT_GUI_FONT));
        let text: Vec<u16> = format!("#{:06X}", color).encode_utf16().collect();
        let _ = TextOutW(hdc, r.left + 28, r.top + 5, &text);
        let _ = DeleteObject(HGDIOBJ(fill.0));
        let _ = DeleteObject(HGDIOBJ(back.0));
        let _ = DeleteObject(HGDIOBJ(frame.0));
    });
}
