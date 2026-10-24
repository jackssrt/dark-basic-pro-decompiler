//! Pulling `_virtual.dat` out of a compiled DBP game.
//!
//! Handles every container DBP can emit:
//!
//! * normal exe -- `[win32 exe section][attached files][20 bytes extra]`
//! * exe + `.pck` sibling -- `[win32 exe section] + [<name>.pck][20 bytes extra]`
//! * `.pck` standalone -- `[attached files]`
//! * compressed variants -- `[..][compress.dll record][raw blob][20 bytes extra]`
//!
//! Attached-file record layout (both DBP and DBC):
//!
//! ```text
//! uint32 name_len
//! bytes  name      (dbc: null terminated; dbp: not)
//! uint32 data_len
//! bytes  data
//! ```
//!
//! Compressed payloads need the game's own `compress.dll`, so that path only
//! works on Windows.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::container::{self, latin1};

/// The blob every compiled program carries.
pub const TARGET_NAME: &str = "_virtual.dat";
/// The DLL that carries the compressed-payload decompressor.
pub const COMPRESS_DLL_NAME: &str = "compress.dll";
/// DBP exe footer length.
pub const EXE_EXTRA_TAIL: usize = 20;
/// DBC footer length.
pub const DBC_EXTRA_TAIL: usize = 4;

/// No attached-file table could be located, or one record is inconsistent.
#[derive(Debug)]
pub enum BadFormat {
    Truncated {
        what: &'static str,
        offset: usize,
    },
    RecordTooLong {
        name: String,
        claimed: u32,
        left: usize,
    },
    NoTable {
        tried: Vec<String>,
    },
    NoTarget(String),
    NoSiblingPck(String),
    Compressed(String),
}

impl fmt::Display for BadFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { what, offset } => write!(f, "truncated {what} at 0x{offset:x}"),
            Self::RecordTooLong {
                name,
                claimed,
                left,
            } => {
                write!(
                    f,
                    "record {name:?} claims {claimed} bytes, only {left} left"
                )
            }
            Self::NoTable { tried } => write!(
                f,
                "no attached-file table found (tried {})",
                if tried.is_empty() {
                    "nothing".to_string()
                } else {
                    tried.join(", ")
                }
            ),
            Self::NoTarget(p) => write!(f, "no {TARGET_NAME} in {p}"),
            Self::NoSiblingPck(name) => {
                write!(f, "no attached files in the exe and no {name} next to it")
            }
            Self::Compressed(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for BadFormat {}

/// Where one attached file lives inside the container.
#[derive(Clone, Debug)]
pub struct AttachedFile {
    /// Path as the container spells it.
    pub name: String,
    /// Offset of the payload inside the container.
    pub offset: usize,
    /// Payload length.
    pub size: usize,
}

impl AttachedFile {
    /// The final path component, lowercased -- what the callers match on.
    pub fn base_name(&self) -> String {
        self.name
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_lowercase()
    }
}

/// Options for the extraction.
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// A `compress.dll` to use instead of the one inside the container.
    pub compress_dll: Option<PathBuf>,
}

fn u32_at(buf: &[u8], off: usize) -> Result<u32, BadFormat> {
    container::read_u32(buf, off).map_err(|_| BadFormat::Truncated {
        what: "int",
        offset: off,
    })
}

/// Filename bytes to a printable name; DBC null-terminates, DBP does not.
fn clean_name(raw: &[u8]) -> String {
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    latin1(&raw[..end])
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 255 && name.chars().all(|c| (' '..'\x7f').contains(&c))
}

/// Parse the attached-file table at `start`, returning it and the offset just
/// past it.
pub fn parse_records(data: &[u8], start: usize) -> Result<(Vec<AttachedFile>, usize), BadFormat> {
    let mut files = Vec::new();
    let mut off = start;
    while off + 4 <= data.len() {
        let name_len = u32_at(data, off)? as usize;
        if name_len == 0 || name_len > 255 || off + 4 + name_len > data.len() {
            break;
        }
        let name = clean_name(&data[off + 4..off + 4 + name_len]);
        if !valid_name(&name) {
            break;
        }
        off += 4 + name_len;
        let data_len = u32_at(data, off)? as usize;
        off += 4;
        if data_len > data.len() - off {
            return Err(BadFormat::RecordTooLong {
                name,
                claimed: data_len as u32,
                left: data.len() - off,
            });
        }
        files.push(AttachedFile {
            name,
            offset: off,
            size: data_len,
        });
        off += data_len;
    }
    Ok((files, off))
}

/// Offset past the win32 exe section, from the PE headers, or `None` if the
/// container is not a PE.
pub fn pe_overlay_offset(data: &[u8]) -> Option<usize> {
    if data.len() < 0x40 || data.get(..2) != Some(b"MZ") {
        return None;
    }
    let e_lfanew = container::read_u32(data, 0x3C).ok()? as usize;
    if e_lfanew + 24 > data.len() || data.get(e_lfanew..e_lfanew + 4) != Some(b"PE\0\0") {
        return None;
    }
    let word = |at: usize| -> Option<usize> {
        let b = data.get(at..at + 2)?;
        Some(u16::from_le_bytes([b[0], b[1]]) as usize)
    };
    let num_sections = word(e_lfanew + 6)?;
    let opt_hdr_size = word(e_lfanew + 20)?;
    let table = e_lfanew + 24 + opt_hdr_size;
    let mut end = u32_at(data, e_lfanew + 24 + 60).ok()? as usize;
    for i in 0..num_sections {
        let sec = table + i * 40;
        if sec + 40 > data.len() {
            return None;
        }
        let raw_size = u32_at(data, sec + 16).ok()?;
        let raw_ptr = u32_at(data, sec + 20).ok()?;
        if raw_size != 0 && raw_ptr != 0 {
            end = end.max(raw_ptr as usize + raw_size as usize);
        }
    }
    (end <= data.len()).then_some(end)
}

/// DarkBASIC Classic: the exe section size is stored as an int32 at the very
/// end.
pub fn dbc_overlay_offset(data: &[u8]) -> Option<usize> {
    if data.len() < DBC_EXTRA_TAIL + 4 {
        return None;
    }
    let size = u32_at(data, data.len() - DBC_EXTRA_TAIL).ok()? as usize;
    (size > 0 && size < data.len()).then_some(size)
}

/// Last-resort probe near `center` for a shifted table start; icon and version
/// edits move the exe section around. Bounded so big exes stay fast.
fn scan_for_table(data: &[u8], center: usize) -> Option<(Vec<AttachedFile>, usize, usize, String)> {
    let lo = center.saturating_sub(0x1000);
    let hi = data.len().saturating_sub(8).min(center + 0x1000);
    for off in lo..hi {
        let Ok(name_len) = u32_at(data, off) else {
            continue;
        };
        if name_len == 0 || name_len > 255 {
            continue;
        }
        let name_end = off + 4 + name_len as usize;
        if name_end > data.len() || !valid_name(&clean_name(&data[off + 4..name_end])) {
            continue;
        }
        let Ok((files, end)) = parse_records(data, off) else {
            continue;
        };
        if files.len() > 1 && data.len().saturating_sub(end) <= EXE_EXTRA_TAIL {
            return Some((
                files,
                end,
                data.len() - end,
                format!("table scan @0x{off:x}"),
            ));
        }
    }
    None
}

/// Locate the attached-file table: the files, the end offset, the footer
/// length, and where it was found.
pub fn load_table(
    data: &[u8],
    path: &Path,
) -> Result<(Vec<AttachedFile>, usize, usize, String), BadFormat> {
    let mut candidates: Vec<(usize, String)> = Vec::new();
    let mut pck_path: Option<PathBuf> = None;
    let name = path.to_string_lossy().to_lowercase();

    if name.ends_with(".pck") {
        candidates.push((0, "pck file".to_string()));
    } else {
        if let Some(pe_end) = pe_overlay_offset(data)
            && pe_end < data.len()
        {
            candidates.push((pe_end, format!("PE section end (0x{pe_end:x})")));
        }
        // The "exe with .pck" type ships an exe carrying no appended files.
        pck_path = Some(path.with_extension("pck"));
        if let Some(dbc) = dbc_overlay_offset(data)
            && candidates.iter().all(|(c, _)| *c != dbc)
        {
            candidates.push((dbc, format!("classic trailer int (0x{dbc:x})")));
        }
    }

    for (off, source) in &candidates {
        let Ok((files, end)) = parse_records(data, *off) else {
            continue;
        };
        if files.is_empty() {
            continue;
        }
        let tail = data.len() - end;
        if files.len() == 1 && files[0].base_name() == COMPRESS_DLL_NAME && tail >= EXE_EXTRA_TAIL {
            // What follows is one compressed blob, not a file table.
            return Ok((files, end, EXE_EXTRA_TAIL, format!("{source} (compressed)")));
        }
        // The footer is 20 bytes (DBP exe) or 4 bytes (DBC); a bigger tail
        // means we landed on garbage, so keep looking.
        if tail <= EXE_EXTRA_TAIL || source == "pck file" {
            return Ok((files, end, tail, source.clone()));
        }
    }

    if let Some(pck_path) = &pck_path
        && pck_path.is_file()
        && let Ok(pck) = fs::read(pck_path)
        && let Ok((files, end)) = parse_records(&pck, 0)
        && !files.is_empty()
    {
        let base = pck_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if files.len() == 1 && files[0].base_name() == COMPRESS_DLL_NAME {
            return Ok((files, end, 0, format!("sibling .pck (compressed) ({base})")));
        }
        return Ok((
            files,
            end,
            pck.len() - end,
            format!("sibling .pck ({base})"),
        ));
    }

    for (off, _) in &candidates {
        if let Some(hit) = scan_for_table(data, *off) {
            return Ok(hit);
        }
    }

    match pck_path {
        Some(p) => Err(BadFormat::NoSiblingPck(
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        )),
        None => Err(BadFormat::NoTable {
            tried: candidates.iter().map(|(_, s)| s.clone()).collect(),
        }),
    }
}

/// Run the game's own `compress.dll`: `decompress_block(data, size) -> HGLOBAL`.
///
/// The DLL is loaded with `LoadLibraryA` and its entry point fetched with
/// `GetProcAddress`; the result is a moveable global block read with
/// `GlobalSize`/`GlobalLock`, exactly as the compiler's own loader does.
#[cfg(windows)]
fn decompress_native(blob: &[u8], dll_path: &Path) -> Result<Vec<u8>, BadFormat> {
    use std::ffi::{c_char, c_int, c_void};

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(handle: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(handle: *mut c_void) -> i32;
        fn GlobalSize(handle: *mut c_void) -> usize;
        fn GlobalFree(handle: *mut c_void) -> *mut c_void;
        fn LoadLibraryA(name: *const c_char) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const c_char) -> *mut c_void;
    }

    const GMEM_MOVEABLE: u32 = 0x0002;
    type DecompressFn = unsafe extern "system" fn(*mut c_void, c_int) -> *mut c_void;

    let mut dll_name = dll_path.as_os_str().as_encoded_bytes().to_vec();
    dll_name.push(0);

    // SAFETY: `dll_name` is NUL-terminated and outlives every call below; each
    // global handle is owned here and freed before returning; the `from_raw_parts`
    // slice is copied out before the block is unlocked and freed.
    unsafe {
        let module = LoadLibraryA(dll_name.as_ptr().cast::<c_char>());
        if module.is_null() {
            return Err(BadFormat::Compressed(format!(
                "cannot load {}",
                dll_path.display()
            )));
        }
        let sym = b"decompress_block\0";
        let entry = GetProcAddress(module, sym.as_ptr().cast::<c_char>());
        if entry.is_null() {
            return Err(BadFormat::Compressed(format!(
                "{} exports no decompress_block",
                dll_path.display()
            )));
        }
        // SAFETY: the export's signature is documented as `(void*, int) -> HGLOBAL`.
        let decompress: DecompressFn = std::mem::transmute(entry);

        let src = GlobalAlloc(GMEM_MOVEABLE, blob.len().max(1));
        if src.is_null() {
            return Err(BadFormat::Compressed("GlobalAlloc failed".to_string()));
        }
        std::ptr::copy_nonoverlapping(blob.as_ptr(), src.cast::<u8>(), blob.len());
        let handle = decompress(src, blob.len() as c_int);
        GlobalFree(src);
        if handle.is_null() {
            return Err(BadFormat::Compressed(
                "decompress_block failed (returned NULL)".to_string(),
            ));
        }
        let size = GlobalSize(handle);
        let locked = GlobalLock(handle);
        if locked.is_null() {
            GlobalFree(handle);
            return Err(BadFormat::Compressed("GlobalLock failed".to_string()));
        }
        let out = std::slice::from_raw_parts(locked.cast::<u8>(), size).to_vec();
        GlobalUnlock(handle);
        GlobalFree(handle);
        Ok(out)
    }
}

/// Off Windows there is no way to run the bundled DLL.
#[cfg(not(windows))]
fn decompress_native(_blob: &[u8], _dll_path: &Path) -> Result<Vec<u8>, BadFormat> {
    Err(BadFormat::Compressed(format!(
        "compressed payload needs the bundled {COMPRESS_DLL_NAME} (Windows/Wine only); \
         re-run under Windows or pass --compress-dll"
    )))
}

/// Locate the table, expanding a compressed payload on the way.
pub fn resolve_table(
    data: &[u8],
    path: &Path,
    opts: &Options,
) -> Result<(Vec<AttachedFile>, String), BadFormat> {
    let (files, end, tail, source) = load_table(data, path)?;

    if files.len() == 1 && files[0].base_name() == COMPRESS_DLL_NAME {
        let blob_end = data.len() - tail;
        let blob = data.get(end..blob_end).ok_or(BadFormat::Truncated {
            what: "compressed blob",
            offset: end,
        })?;
        let plain = match &opts.compress_dll {
            Some(dll) => decompress_native(blob, dll)?,
            None => write_dll_to_temp(data, &files[0]).and_then(|tmp| {
                let out = decompress_native(blob, &tmp);
                let _ = fs::remove_file(&tmp);
                out
            })?,
        };
        let (files, inner_source) =
            load_table(&plain, Path::new("unpacked.pck")).map(|t| (t.0, t.3))?;
        return Ok((
            files,
            format!("decompressed {COMPRESS_DLL_NAME} ({inner_source})"),
        ));
    }
    Ok((files, source))
}

/// Write the embedded `compress.dll` out so it can be loaded, then hand the
/// path to the caller through the closure.
fn write_dll_to_temp(data: &[u8], file: &AttachedFile) -> Result<PathBuf, BadFormat> {
    let dir = std::env::temp_dir();
    for n in 0..64u32 {
        let path = dir.join(format!("dbp-compress-{}-{n}.dll", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut fh) => {
                use std::io::Write as _;
                fh.write_all(&data[file.offset..file.offset + file.size])
                    .map_err(|e| BadFormat::Compressed(e.to_string()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(BadFormat::Compressed(e.to_string())),
        }
    }
    Err(BadFormat::Compressed(
        "no free temporary name for compress.dll".to_string(),
    ))
}

/// Find `_virtual.dat` in a container and return its payload.
pub fn extract(data: &[u8], path: &Path, opts: &Options) -> Result<Vec<u8>, BadFormat> {
    let (files, _source) = resolve_table(data, path, opts)?;
    let hit = files
        .iter()
        .find(|f| f.base_name() == TARGET_NAME)
        .ok_or_else(|| BadFormat::NoTarget(path.display().to_string()))?;
    Ok(data[hit.offset..hit.offset + hit.size].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut v = (name.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn parses_a_file_table_and_its_offset() {
        let mut data = record(TARGET_NAME, b"hello");
        data.extend_from_slice(&[0; 20]);
        let (files, end) = parse_records(&data, 0).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size, 5);
        assert_eq!(&data[files[0].offset..files[0].offset + 5], b"hello");
        assert_eq!(end, data.len() - 20);
    }

    #[test]
    fn a_record_longer_than_the_container_is_an_error() {
        let mut data = 1u32.to_le_bytes().to_vec();
        data.push(b'a');
        data.extend_from_slice(&9999u32.to_le_bytes());
        assert!(matches!(
            parse_records(&data, 0),
            Err(BadFormat::RecordTooLong { .. })
        ));
    }

    #[test]
    fn a_pck_table_starts_at_zero() {
        let data = record("a.txt", b"xx");
        let (files, end, _tail, source) = load_table(&data, Path::new("game.pck")).unwrap();
        assert_eq!(source, "pck file");
        assert_eq!(files[0].name, "a.txt");
        assert_eq!(end, data.len());
    }

    #[test]
    fn backslash_paths_are_matched_on_their_tail() {
        let data = record("db\\data\\_virtual.dat", b"xx");
        let (files, _) = parse_records(&data, 0).unwrap();
        assert_eq!(files[0].base_name(), TARGET_NAME);
    }

    #[test]
    fn the_blob_can_be_taken_straight_out_of_a_pck() {
        let data = record(TARGET_NAME, b"payload");
        let got = extract(&data, Path::new("game.pck"), &Options::default()).unwrap();
        assert_eq!(got, b"payload");
    }

    #[test]
    fn a_container_without_the_blob_says_so() {
        let data = record("other.dat", b"x");
        assert!(matches!(
            extract(&data, Path::new("game.pck"), &Options::default()),
            Err(BadFormat::NoTarget(_))
        ));
    }
}
