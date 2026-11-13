//! Accepting dropped items.
//!
//! GPUI's own drop handling only understands `CF_HDROP` — a list of paths to files that already
//! exist. Outlook (and Teams, and a zipped folder's contents) drags *virtual* files instead: the
//! mail is not on disk anywhere, it comes as a `FileGroupDescriptorW` naming the items plus a
//! `FileContents` stream per item, which the receiver is expected to write out itself. So this
//! replaces GPUI's drop target with one that handles both.

use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::core::{implement, Result, PCWSTR};
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::StructuredStorage::{StgCreateDocfile, IStorage};
use windows::Win32::System::Com::{
    IDataObject, IStream, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGC_DEFAULT, STGM_CREATE, STGM_READWRITE,
    STGM_SHARE_EXCLUSIVE, TYMED_HGLOBAL, TYMED_ISTORAGE, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    DragQueryFileW, CFSTR_FILECONTENTS, CFSTR_FILEDESCRIPTORW, FILEDESCRIPTORW, FILEGROUPDESCRIPTORW, HDROP,
};

use super::{emit, ShellEvent};

/// The folder a drop lands in. The UI keeps this in step as the user navigates; None at the
/// My Computer level, where there is nowhere to put anything.
static DROP_FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);

/// What DragEnter decided about the drag in progress. DragOver is handed no data object, so the
/// answer has to be remembered — and without it the cursor would go back to "no entry".
static ACCEPTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The window this drop target belongs to, so a copy it starts is owned by that window.
static OWNER: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// This window is the source of the drag in progress. Our own target would otherwise accept it
/// and copy the files into the folder they came from.
static DRAGGING_OUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_dragging_out(on: bool) {
    DRAGGING_OUT.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn set_drop_folder(folder: Option<PathBuf>) {
    if let Ok(mut g) = DROP_FOLDER.lock() {
        *g = folder;
    }
}

fn drop_folder() -> Option<PathBuf> {
    DROP_FOLDER.lock().ok().and_then(|g| g.clone())
}

/// Takes over drag-and-drop for the window. GPUI registered its own target when the window was
/// created, so that one is revoked first — a window can only have one.
pub fn enable_drop(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    OWNER.store(hwnd, std::sync::atomic::Ordering::Relaxed);
    let target: IDropTarget = DropTarget.into();
    unsafe {
        let _ = RevokeDragDrop(HWND(hwnd));
        // Windows keeps its own reference for as long as the window is registered, so ours can go.
        let _ = RegisterDragDrop(HWND(hwnd), &target);
    }
}

#[implement(IDropTarget)]
struct DropTarget;

impl DropTarget {
    /// What we would do with this data: copy it in, or nothing.
    unsafe fn effect(data: Option<&IDataObject>) -> DROPEFFECT {
        let Some(data) = data else { return DROPEFFECT_NONE };
        if DRAGGING_OUT.load(std::sync::atomic::Ordering::Relaxed) {
            return DROPEFFECT_NONE;
        }
        if drop_folder().is_none() {
            return DROPEFFECT_NONE; // My Computer: no destination
        }
        if offers_files(data) {
            DROPEFFECT_COPY
        } else {
            DROPEFFECT_NONE
        }
    }
}

impl IDropTarget_Impl for DropTarget {
    fn DragEnter(
        &self,
        data: Option<&IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        unsafe {
            // Never answer with an effect the drag source did not offer.
            let allowed = *effect;
            let e = Self::effect(data);
            let e = if allowed.0 & DROPEFFECT_COPY.0 != 0 { e } else { DROPEFFECT_NONE };
            *effect = e;
            let accepted = e == DROPEFFECT_COPY;
            ACCEPTING.store(accepted, std::sync::atomic::Ordering::Relaxed);
            emit(ShellEvent::DragHover(accepted));
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe {
            *effect = if ACCEPTING.load(std::sync::atomic::Ordering::Relaxed) {
                DROPEFFECT_COPY
            } else {
                DROPEFFECT_NONE
            };
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        ACCEPTING.store(false, std::sync::atomic::Ordering::Relaxed);
        emit(ShellEvent::DragHover(false));
        Ok(())
    }

    fn Drop(
        &self,
        data: Option<&IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> Result<()> {
        ACCEPTING.store(false, std::sync::atomic::Ordering::Relaxed);
        emit(ShellEvent::DragHover(false));
        unsafe { *effect = DROPEFFECT_NONE };
        let (Some(data), Some(dest)) = (data, drop_folder()) else { return Ok(()) };
        unsafe {
            // A plain drag of files that already exist on disk (from Explorer, another
            // miniExpo window, ...) offers only CF_HDROP: hand that to the shell's own copy
            // engine, with its progress and conflict UI. A mail client's attachment drag
            // (Outlook, Thunderbird, ...) additionally offers CFSTR_FILEDESCRIPTORW — the
            // format meant for exactly this case — and its CF_HDROP, when it also provides
            // one, is only a same-session compatibility fallback for receivers that don't
            // understand virtual files: Thunderbird's in particular renders a Unicode (e.g.
            // Chinese/Japanese) attachment name through the local codepage there, mangling it,
            // while its FileGroupDescriptorW keeps the real Unicode name. So whenever the
            // virtual-file format is offered at all, it takes priority over CF_HDROP.
            if !has_format(data, descriptor_format(), -1) {
                let paths = dropped_paths(data);
                if !paths.is_empty() {
                    let files: Vec<PathBuf> =
                        paths.into_iter().filter(|p| p.parent() != Some(dest.as_path())).collect();
                    if !files.is_empty() {
                        super::copy_files_into(OWNER.load(std::sync::atomic::Ordering::Relaxed), &files, &dest);
                    }
                    *effect = DROPEFFECT_COPY;
                    return Ok(());
                }
            }
            let written = write_virtual_files(data, &dest);
            if !written.is_empty() {
                *effect = DROPEFFECT_COPY;
                emit(ShellEvent::Arrived(written));
            }
        }
        Ok(())
    }
}

/// "FileGroupDescriptorW", registered once by the system and the same number for everyone.
fn descriptor_format() -> u32 {
    unsafe { RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW) }
}

fn contents_format() -> u32 {
    unsafe { RegisterClipboardFormatW(CFSTR_FILECONTENTS) }
}

fn format_etc(format: u32, index: i32, tymed: u32) -> FORMATETC {
    FORMATETC {
        cfFormat: format as u16,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: index,
        tymed,
    }
}

unsafe fn has_format(data: &IDataObject, format: u32, index: i32) -> bool {
    let etc = format_etc(format, index, TYMED_HGLOBAL.0 as u32 | TYMED_ISTREAM.0 as u32);
    data.QueryGetData(&etc).is_ok()
}

/// Whether this drag carries files of either kind. Some sources answer `QueryGetData` poorly, so
/// a negative answer is double-checked against the formats the object actually enumerates.
unsafe fn offers_files(data: &IDataObject) -> bool {
    let hdrop = CF_HDROP.0 as u32;
    let descriptor = descriptor_format();
    if has_format(data, hdrop, -1) || has_format(data, descriptor, -1) {
        return true;
    }
    let Ok(e) = data.EnumFormatEtc(DATADIR_GET.0 as u32) else { return false };
    for _ in 0..64 {
        let mut one = [FORMATETC::default(); 1];
        let mut got = 0u32;
        if e.Next(&mut one, Some(&mut got)).is_err() || got != 1 {
            break;
        }
        let cf = one[0].cfFormat as u32;
        if cf == hdrop || cf == descriptor {
            return true;
        }
    }
    false
}

/// The plain "files that exist on disk" case.
unsafe fn dropped_paths(data: &IDataObject) -> Vec<PathBuf> {
    let etc = format_etc(CF_HDROP.0 as u32, -1, TYMED_HGLOBAL.0 as u32);
    let Ok(mut medium) = data.GetData(&etc) else { return Vec::new() };
    let mut out = Vec::new();
    let hdrop = HDROP(medium.u.hGlobal.0 as isize);
    let count = DragQueryFileW(hdrop, u32::MAX, None);
    for i in 0..count {
        let len = DragQueryFileW(hdrop, i, None);
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let written = DragQueryFileW(hdrop, i, Some(&mut buf));
        if written > 0 {
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..written as usize])));
        }
    }
    ReleaseStgMedium(&mut medium);
    out
}

/// Writes out the items a virtual-file drag carries; returns the names written.
unsafe fn write_virtual_files(data: &IDataObject, dest: &Path) -> Vec<String> {
    let etc = format_etc(descriptor_format(), -1, TYMED_HGLOBAL.0 as u32);
    let Ok(mut medium) = data.GetData(&etc) else { return Vec::new() };
    let names = read_descriptors(medium.u.hGlobal.0 as *mut std::ffi::c_void);
    ReleaseStgMedium(&mut medium);

    let mut written = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let Some(path) = unique_path(dest, name) else { continue };
        if save_item(data, index as i32, &path) {
            if let Some(n) = path.file_name() {
                written.push(n.to_string_lossy().into_owned());
            }
        }
    }
    written
}

/// Saves one item of a virtual-file drag.
///
/// A mail arrives as `TYMED_ISTORAGE` — a .msg is a compound file, not a flat blob — while
/// attachments and most other sources hand over a stream or a block of memory. Outlook lists both
/// ISTREAM and ISTORAGE in its formats but only really answers for ISTORAGE, so all three media
/// are asked for at once and whatever comes back is handled.
unsafe fn save_item(data: &IDataObject, index: i32, path: &Path) -> bool {
    let tymed = TYMED_ISTORAGE.0 as u32 | TYMED_ISTREAM.0 as u32 | TYMED_HGLOBAL.0 as u32;
    let etc = format_etc(contents_format(), index, tymed);
    let Ok(mut medium) = data.GetData(&etc) else { return false };
    let ok = if medium.tymed == TYMED_ISTORAGE.0 as u32 {
        match medium.u.pstg.as_ref() {
            Some(storage) => copy_storage(storage, path),
            None => false,
        }
    } else {
        let bytes = if medium.tymed == TYMED_ISTREAM.0 as u32 {
            medium.u.pstm.as_ref().map(|s| read_stream(s))
        } else if medium.tymed == TYMED_HGLOBAL.0 as u32 {
            use windows::Win32::Foundation::HGLOBAL;
            let handle = HGLOBAL(medium.u.hGlobal.0);
            let p = GlobalLock(handle) as *const u8;
            let out = (!p.is_null()).then(|| std::slice::from_raw_parts(p, GlobalSize(handle)).to_vec());
            let _ = GlobalUnlock(handle);
            out
        } else {
            None
        };
        match bytes {
            Some(b) => std::fs::write(path, &b).is_ok(),
            None => false,
        }
    };
    ReleaseStgMedium(&mut medium);
    ok
}

/// Writes an IStorage out as a compound file — this is what turns a dragged mail into a .msg.
unsafe fn copy_storage(source: &IStorage, path: &Path) -> bool {
    let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
    let mode = STGM_CREATE | STGM_READWRITE | STGM_SHARE_EXCLUSIVE;
    let Ok(dest) = StgCreateDocfile(PCWSTR(wide.as_ptr()), mode, 0) else { return false };
    if source.CopyTo(None, None, &dest).is_err() {
        return false;
    }
    dest.Commit(STGC_DEFAULT.0 as u32).is_ok()
}

/// FILEGROUPDESCRIPTORW: a count, then that many fixed-size descriptors.
unsafe fn read_descriptors(hglobal: *mut std::ffi::c_void) -> Vec<String> {
    use windows::Win32::Foundation::HGLOBAL;
    let handle = HGLOBAL(hglobal);
    let base = GlobalLock(handle) as *const u8;
    if base.is_null() {
        return Vec::new();
    }
    let size = GlobalSize(handle);
    let mut out = Vec::new();
    if size >= size_of::<FILEGROUPDESCRIPTORW>() {
        // The struct is packed, so every read goes through read_unaligned.
        let count = std::ptr::read_unaligned(base as *const u32) as usize;
        let first = base.add(size_of::<u32>()) as *const FILEDESCRIPTORW;
        // Trust the buffer's real size over the count it declares.
        let room = (size - size_of::<u32>()) / size_of::<FILEDESCRIPTORW>();
        for i in 0..count.min(room) {
            // addr_of! so no reference into the packed struct is ever created.
            let name: [u16; 260] = std::ptr::read_unaligned(std::ptr::addr_of!((*first.add(i)).cFileName));
            let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
            out.push(String::from_utf16_lossy(&name[..len]));
        }
    }
    let _ = GlobalUnlock(handle);
    out
}

unsafe fn read_stream(stream: &IStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let mut read = 0u32;
        let hr = stream.Read(chunk.as_mut_ptr() as *mut _, chunk.len() as u32, Some(&mut read));
        if read > 0 {
            out.extend_from_slice(&chunk[..read as usize]);
        }
        // S_FALSE (or a short read) means the end of the stream.
        if hr.is_err() || read == 0 {
            break;
        }
    }
    out
}

/// A free path for `name` inside `dest`, adding " (2)", " (3)" … rather than overwriting.
fn unique_path(dest: &Path, name: &str) -> Option<PathBuf> {
    // Outlook builds the name from the subject, which can carry characters a path cannot.
    let safe: String = name.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
    let safe = safe.trim().trim_end_matches('.');
    if safe.is_empty() {
        return None;
    }
    let path = Path::new(safe);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| safe.to_string());
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 1..1000 {
        let candidate = if n == 1 { format!("{stem}{ext}") } else { format!("{stem} ({n}){ext}") };
        let full = dest.join(&candidate);
        if !full.exists() {
            return Some(full);
        }
    }
    None
}
