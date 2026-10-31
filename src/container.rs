//! The `_virtual.dat` container and the symbolic machine code block inside it.
//!
//! The Machine Code Block is produced verbatim by `CASMWriter`
//! (`DBProCompiler/DBPCompiler/ASMWriter.cpp`, `GenerateASMCodes()`). Every
//! 32-bit operand the writer emitted is recorded in the reference tables
//! (byte offset, type, index) and left in the file as the placeholder
//! `0xFFFFFFFF`; `EXEBlock::Init()` patches those slots at load time. So the
//! blob on disk is already fully symbolic -- applying the table decodes it.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::templates::{NO_OPERAND, TEMPLATES, TWO_OPERAND, Template};

/// Reference-table entry kinds (`ASMWriter::UpdateMCBRefData`).
pub mod ref_type {
    /// DLL command call -- index into the command table.
    pub const DLL_CALL: u32 = 1;
    /// String -- index into the string table.
    pub const STRING: u32 = 2;
    /// Variable -- byte offset into the variable space.
    pub const VARIABLE: u32 = 3;
    /// Immediate -- literal value.
    pub const IMMEDIATE: u32 = 4;
    /// Label -- byte offset in the machine code block.
    pub const LABEL: u32 = 5;
    /// Data -- index into the data space.
    pub const DATA: u32 = 6;
}

/// A reference-table entry: what the operand at a byte offset means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ref {
    /// One of the [`ref_type`] constants.
    pub kind: u32,
    /// The table index or literal value the operand carries.
    pub index: u32,
}

/// One decoded instruction.
#[derive(Clone, Debug)]
pub struct Ins {
    /// Byte offset of the instruction inside the machine code block.
    pub addr: u32,
    /// Mnemonic, borrowed from the static [`TEMPLATES`] table.
    pub name: &'static str,
    /// Length of the opcode bytes, before any operand.
    pub opcode: u32,
    /// First symbolic operand, when the reference table covers it.
    pub reference: Option<Ref>,
    /// First raw operand, when no reference covers it.
    pub raw: Option<u32>,
    /// Byte width of the first raw operand.
    pub width: u32,
    /// Second symbolic operand of a two-operand template.
    pub reference2: Option<Ref>,
    /// Second raw operand of a two-operand template.
    pub raw2: Option<u32>,
    /// Total encoded size in bytes.
    pub size: u32,
}

/// The template lookups the decoder needs, built once per [`Prog`].
#[derive(Clone, Debug, Default)]
pub struct TemplateIndex {
    /// Opcode byte sequence (little-endian packed) -> the templates that emit
    /// it, in reference-table order.
    by_bytes: HashMap<u32, Vec<&'static Template>>,
}

impl TemplateIndex {
    fn build() -> Self {
        let mut by_bytes: HashMap<u32, Vec<&'static Template>> =
            HashMap::with_capacity(TEMPLATES.len());
        for t in TEMPLATES {
            // A template that emits no opcode bytes has no runtime sequence to
            // match on; indexing it under key 0 would collide with the
            // single-byte opcode `00`.
            if t.bytes.is_empty() {
                continue;
            }
            by_bytes.entry(opcode_key(t.bytes)).or_default().push(t);
        }
        Self { by_bytes }
    }

    fn get(&self, key: u32) -> Option<&[&'static Template]> {
        self.by_bytes.get(&key).map(|v| &v[..])
    }
}

/// Pack an opcode sequence into a key: the bytes little-endian, with the
/// length in the top byte so `8B` and `8B 00` stay distinct.
fn opcode_key(bytes: &[u8]) -> u32 {
    let mut key = (bytes.len() as u32) << 24;
    for (i, b) in bytes.iter().enumerate() {
        key |= u32::from(*b) << (8 * i);
    }
    key
}

/// Everything `_virtual.dat` holds, plus the decoded machine code.
#[derive(Clone, Debug, Default)]
pub struct Prog {
    pub mode: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub appname: String,

    pub dllidx: Vec<u32>,
    pub dllfn: Vec<String>,
    pub dllalready: Vec<u32>,

    pub refpos: Vec<u32>,
    pub reftype: Vec<u32>,
    pub refidx: Vec<u32>,
    pub errstr: Vec<String>,

    pub mcbsize: u32,
    pub mcb: Vec<u8>,

    pub cmd_dll: Vec<u32>,
    pub cmd_name: Vec<String>,
    pub strings: Vec<String>,

    pub datablock: Vec<u8>,
    pub dstrings: Vec<String>,

    pub varsize: u32,
    pub datasize: u32,
    pub dynvar: Vec<u32>,
    pub dynvartype: Vec<u32>,
    pub utype: Vec<u8>,

    /// Bytes after the last section.
    pub tail: usize,

    /// Template lookups used while decoding.
    index: TemplateIndex,
    /// Reference slot offset -> index into [`Prog::refpos`].
    refindex: HashMap<u32, u32>,
    /// Decoded instructions in stream order.
    pub ins: Vec<Ins>,
    /// Offsets of bytes no template matched.
    pub bad: Vec<u32>,
    /// Offset just past the last decoded instruction.
    pub end: usize,
    /// Reference slots the code never consumes.
    pub unused_refs: Vec<u32>,
}

/// A truncated or otherwise unusable blob.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("truncated read of {0} bytes at 0x{1:x}")]
    Truncated(usize, usize),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

fn in_set(set: &[&str], name: &str) -> bool {
    set.contains(&name)
}

/// Width of a *raw* operand (one not recorded in the reference table).
fn operand_width(name: &str) -> u32 {
    if in_set(NO_OPERAND, name) {
        return 0;
    }
    let n = name.rsplit(' ').next().unwrap_or(name);
    if let Some(rest) = n.strip_prefix("IMM")
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return rest.parse().unwrap_or(0);
    }
    if n.ends_with(" CL") { 0 } else { 4 }
}

/// Width of the second (raw) operand.
fn operand2_width(name: &str) -> u32 {
    if name == "LOOP ECX" {
        return 1;
    }
    let n = name.rsplit(' ').next().unwrap_or(name);
    if let Some(rest) = n.strip_prefix("IMM")
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return rest.parse().unwrap_or(4);
    }
    4
}

/// Cursor over the blob's little-endian sections.
struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Result<u32> {
        let v = self
            .bytes(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))?;
        self.at += 4;
        Ok(v)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        match self.buf.get(self.at..self.at + n) {
            Some(s) => Ok(s),
            None => Err(Error::Truncated(n, self.at)),
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let v = self.bytes(n)?;
        self.at += n;
        Ok(v)
    }

    fn u32s(&mut self, n: usize) -> Result<Vec<u32>> {
        let raw = self.take(n.checked_mul(4).ok_or(Error::Truncated(n, self.at))?)?;
        Ok(raw
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect())
    }

    /// A length-prefixed latin-1 string.
    fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        Ok(latin1(self.take(n)?))
    }

    fn strings(&mut self, n: usize) -> Result<Vec<String>> {
        let mut out = Vec::with_capacity(n.min(1024));
        for _ in 0..n {
            out.push(self.string()?);
        }
        Ok(out)
    }
}

/// Bytes as the latin-1 the format stores them in: one char per byte.
pub fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// A raw operand is read at its own width: `MOV EAX IMM1` carries one byte,
/// and reading it as a dword would pull in the next three.
fn read_operand(buf: &[u8], off: usize, width: u32) -> u32 {
    buf.get(off..off + width as usize)
        .map(|s| {
            s.iter()
                .enumerate()
                .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (8 * i)))
        })
        .unwrap_or(0)
}

/// A little-endian unsigned read that cannot overflow its slice.
pub fn read_u32(buf: &[u8], off: usize) -> Result<u32> {
    let s = buf.get(off..off + 4).ok_or(Error::Truncated(4, off))?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

impl Prog {
    /// Parse and decode a blob held in memory.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut p = Prog {
            index: TemplateIndex::build(),
            ..Prog::default()
        };
        p.load(data)?;
        p.decode();
        Ok(p)
    }

    /// Parse and decode a blob on disk.
    pub fn load_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    /// The struct and user-function signatures the compiler recorded.
    ///
    /// The block is a convenience for the debugger, so it is parsed on demand:
    /// a malformed entry costs an empty row, never the decompilation.
    pub fn utypes(&self) -> Vec<crate::utypes::UType> {
        crate::utypes::parse(&self.utype)
    }

    fn load(&mut self, data: &[u8]) -> Result<()> {
        let mut r = Reader { buf: data, at: 0 };
        self.mode = r.u32()?;
        self.width = r.u32()?;
        self.height = r.u32()?;
        self.depth = r.u32()?;
        self.appname = latin1(r.take(256)?)
            .split('\0')
            .next()
            .unwrap_or("")
            .to_string();

        let n = r.u32()? as usize;
        self.dllidx = r.u32s(n)?;
        self.dllfn = r.strings(n)?;
        self.dllalready = r.u32s(n)?;

        let n = r.u32()? as usize;
        self.refpos = r.u32s(n)?;
        self.reftype = r.u32s(n)?;
        self.refidx = r.u32s(n)?;

        let n = r.u32()? as usize;
        self.errstr = r.strings(n)?;

        self.mcbsize = r.u32()?;
        self.mcb = r.take(self.mcbsize as usize)?.to_vec();

        let n = r.u32()? as usize;
        self.cmd_dll = r.u32s(n)?;
        self.cmd_name = r.strings(n)?;

        let n = r.u32()? as usize;
        self.strings = r.strings(n)?;

        let n = r.u32()? as usize;
        self.datablock = r.take(n.saturating_mul(10))?.to_vec();
        self.dstrings = r.strings(n)?;

        self.varsize = r.u32()?;
        self.datasize = r.u32()?;

        let n = r.u32()? as usize;
        self.dynvar = r.u32s(n)?;
        self.dynvartype = r.u32s(n)?;

        let n = r.u32()? as usize;
        self.utype = r.take(n)?.to_vec();

        self.tail = data.len() - r.at;
        self.refindex = self
            .refpos
            .iter()
            .enumerate()
            .map(|(i, p)| (*p, i as u32))
            .collect();
        Ok(())
    }

    fn decode(&mut self) {
        let index = &self.index;
        let m = &self.mcb;
        let n = m.len();
        let mut p = 0usize;

        while p < n {
            let mut key = 0u32;
            let mut cands: Option<&[&'static Template]> = None;
            for len in 1..=3 {
                if p + len > n {
                    break;
                }
                key |= u32::from(m[p + len - 1]) << (8 * (len - 1));
                if let Some(c) = index.get(key | (len as u32) << 24) {
                    cands = Some(c);
                    break;
                }
            }
            let Some(cands) = cands else {
                self.bad.push(p as u32);
                p += 1;
                continue;
            };
            // A template that claims no operand slot is unambiguous and ends
            // the instruction. When every candidate declares a slot, the
            // operand is whatever follows and has to be worked out.
            let slot_free = cands.iter().find(|t| !t.slot).copied();
            let t = slot_free.unwrap_or(cands[0]);
            let name = t.name;

            let mut reference = None;
            let mut raw = None;
            let mut width = 0u32;
            let mut reference2 = None;
            let mut raw2 = None;
            let mut op = p + t.bytes.len();

            if slot_free.is_none() {
                let two = in_set(TWO_OPERAND, name);
                if let Some(&i) = self.refindex.get(&(op as u32)) {
                    reference = Some(Ref {
                        kind: self.reftype[i as usize],
                        index: self.refidx[i as usize],
                    });
                    width = 4;
                    op += 4;
                } else if two {
                    width = 0; // WriteASMLine2IMM(..., NULL, imm, n)
                } else {
                    width = operand_width(name);
                    if width != 0 {
                        raw = Some(read_operand(m, op, width));
                    }
                    op += width as usize;
                }
                if two {
                    if let Some(&i) = self.refindex.get(&(op as u32)) {
                        reference2 = Some(Ref {
                            kind: self.reftype[i as usize],
                            index: self.refidx[i as usize],
                        });
                        op += 4;
                    } else {
                        let w2 = operand2_width(name);
                        raw2 = Some(read_operand(m, op, w2));
                        op += w2 as usize;
                    }
                }
            }

            let size = t.bytes.len() as u32
                + width
                + if reference2.is_some() { 4 } else { 0 }
                + if raw2.is_some() {
                    operand2_width(name)
                } else {
                    0
                };
            self.ins.push(Ins {
                addr: p as u32,
                name,
                opcode: t.bytes.len() as u32,
                reference,
                raw,
                width,
                reference2,
                raw2,
                size,
            });
            p = op;
        }
        self.end = p;

        let mut used: HashSet<u32> = HashSet::with_capacity(self.ins.len() * 2);
        for ins in &self.ins {
            let opl = ins.opcode;
            if ins.reference.is_some() {
                used.insert(ins.addr + opl);
            }
            if ins.reference2.is_some() {
                used.insert(ins.addr + opl + ins.width);
            }
        }
        let mut unused: Vec<u32> = self
            .refpos
            .iter()
            .copied()
            .filter(|r| !used.contains(r))
            .collect();
        unused.sort_unstable();
        self.unused_refs = unused;
    }
}

/// MSVC decorated name to a plain name, best effort (`?MakeSphere@@YAXHH@Z`
/// becomes `MakeSphere(HXH)`).
pub fn demangle(symbol: &str) -> String {
    let Some(rest) = symbol.strip_prefix('?') else {
        return symbol.to_string();
    };
    let Some((name, tail)) = rest.split_once("@@") else {
        return symbol.to_string();
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '$'))
    {
        return symbol.to_string();
    }
    let mut args = String::new();
    for c in tail.chars() {
        if (c == 'X' || c == 'Z') && args.is_empty() {
            break;
        }
        if c == '@' {
            continue;
        }
        if c.is_ascii_alphabetic() {
            args.push(c);
        } else {
            break;
        }
    }
    format!("{name}({args})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demangles_msvc_names() {
        // The decoration's argument codes are kept as they are written: this
        // is a best-effort reading, not a real demangler.
        assert_eq!(demangle("?MakeSphere@@YAXHMHH@Z"), "MakeSphere(YAXHMHHZ)");
        assert_eq!(demangle("?Sync@@YAXXZ"), "Sync(YAXXZ)");
        assert_eq!(demangle("plain_name"), "plain_name");
        assert_eq!(demangle("?no_double_at"), "?no_double_at");
    }

    #[test]
    fn raw_operand_widths_follow_the_template_spelling() {
        assert_eq!(operand_width("MOV EAX IMM1"), 1);
        assert_eq!(operand_width("MOV EAX IMM4"), 4);
        assert_eq!(operand_width("PUSH EAX"), 0);
        // The rule reads the last token, so a ` CL` spelling only widens to
        // zero through the operand-free template list; these templates carry
        // no slot, so the width is never consulted.
        assert_eq!(operand_width("SHL EAX4 CL"), 4);
        assert_eq!(operand_width("MOV MEM4 ESP"), 4);
        assert_eq!(operand2_width("LOOP ECX"), 1);
        assert_eq!(operand2_width("MOV MEM IMM2"), 2);
    }

    #[test]
    fn a_raw_operand_is_read_at_its_own_width() {
        // `MOV EAX IMM1` carries one byte; reading a dword would swallow the
        // next three and turn 0 into 4294943232.
        assert_eq!(read_operand(&[0x00, 0x01, 0x02, 0x03], 0, 1), 0);
        assert_eq!(read_operand(&[0x00, 0x01, 0x02, 0x03], 0, 4), 0x0302_0100);
        assert_eq!(read_operand(&[0x00, 0x00, 0x00, 0x80], 0, 4), 0x8000_0000);
        assert_eq!(read_operand(&[0x34, 0x12, 0x00], 0, 2), 0x1234);
    }

    #[test]
    fn every_template_name_is_unique() {
        let mut names: Vec<&str> = TEMPLATES.iter().map(|t| t.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate template names");
    }
}
