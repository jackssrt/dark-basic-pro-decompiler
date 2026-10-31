//! Reconstruct a Ghidra-importable native image of the machine code block.
//!
//! The on-disk MCB is fully symbolic: every 32-bit ref-backed operand is the
//! placeholder `0xFFFFFFFF`, and the reference table says exactly what the
//! runtime substitutes at load (`CEXEBlock::Init`). Applying that table yields
//! the very bytes the runtime executes, so the patched MCB is ground truth:
//! no disassembly heuristics are involved.
//!
//! The emitted image is a minimal 32-bit little-endian ELF:
//!
//!   .text  `TEXT_BASE`: patched MCB + one trailing `RET`, then one stub per
//!         command index
//!   .data  right after: zeroed variable space, string table, data block
//!
//! Symbols name the whole block (`CORE_Program`), every user-function entry,
//! every command stub, and the data items, so Ghidra's function discovery,
//! decompiler and cross-reference tools have real names to work with.

use std::collections::BTreeMap;

use crate::container::{Prog, latin1, ref_type};
use crate::names::keyword;

/// The `.text` base of the emitted image.
pub const TEXT_BASE: u32 = 0x0804_8000;

/// Variable-space slots the runtime owns (`CEXEBlock::Init`).
const RESERVED: &[(u32, &str)] = &[
    (0, "_ESP_"),
    (4, "_ERR_"),
    (8, "_ESC_"),
    (12, "_REK_"),
    (16, "_LINE_"),
    (20, "_TEMPA_"),
    (24, "_TEMPB_"),
];

/// Where one build put everything.
#[derive(Debug, Clone)]
pub struct Layout {
    /// Address just past the MCB's terminating `RET`.
    pub mcb_end: u32,
    /// Address of the `.data` section (start of the variable space).
    pub data_base: u32,
    /// Address of the data block.
    pub datablock_base: u32,
    /// Address of string `i` in the table.
    pub strings: Vec<u32>,
    /// Size of `.text` in bytes.
    pub text_len: usize,
    /// Size of `.data` in bytes.
    pub data_len: usize,
}

impl Layout {
    /// Address of the stub for command index `i`.
    pub fn stub(&self, i: u32) -> u32 {
        self.mcb_end + i
    }

    /// The value the runtime puts into a ref slot of kind `k`, index `i`.
    pub fn ref_value(&self, k: u32, i: u32) -> u32 {
        match k {
            ref_type::DLL_CALL => self.stub(i),
            ref_type::STRING => *self.strings.get(i as usize).unwrap_or(&0),
            ref_type::VARIABLE => self.data_base + i,
            ref_type::IMMEDIATE => i,
            ref_type::LABEL => TEXT_BASE + i,
            ref_type::DATA => self.datablock_base + 10 * i,
            _ => 0xFFFF_FFFF,
        }
    }
}

/// One `.symtab` entry.
#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub value: u32,
    pub size: u32,
    pub is_func: bool,
}

/// The image, its layout and its symbols.
pub struct Image {
    pub bytes: Vec<u8>,
    pub layout: Layout,
    pub symbols: Vec<Symbol>,
}

/// User-type entries from the pattern blob: struct-table index -> type name.
///
/// The blob is a run of `name:idx:letters:` entries; the letters name each
/// field (`L` integer, `F` float, `S` string, `B` boolean, `Y` byte,
/// `W` word, `D` dword, `O` double float, `R` double integer, empty for a
/// nested type) and need no further reading here.
pub fn parse_utype(utype: &[u8]) -> BTreeMap<u32, String> {
    let s = latin1(utype);
    let parts: Vec<&str> = s.split(':').collect();
    let mut out = BTreeMap::new();
    for chunk in parts.chunks(3) {
        if chunk.len() == 3 {
            if let Ok(idx) = chunk[1].parse::<u32>() {
                out.insert(idx, chunk[0].to_string());
            }
        }
    }
    out
}

/// Rebuild the executable image from the parsed container.
///
/// `funcs` is the MCB offset of every user-function entry, in address order.
pub fn build(prog: &Prog, funcs: &[u32]) -> Image {
    let mcb_len = prog.mcb.len() as u32;
    let ncmd = prog.cmd_name.len() as u32;

    // --- addresses ---------------------------------------------------------
    let mcb_end = TEXT_BASE + mcb_len + 1; // past the MCB and its RET
    let var_base = mcb_end + ncmd; // .data starts with the variable space
    let mut str_addr: Vec<u32> = Vec::with_capacity(prog.strings.len());
    let mut off = prog.varsize;
    for s in &prog.strings {
        str_addr.push(off);
        off += s.len() as u32 + 1;
    }
    let datablock_base = var_base + off;

    let layout = Layout {
        mcb_end,
        data_base: var_base,
        datablock_base,
        strings: str_addr,
        text_len: (mcb_len + 1 + ncmd) as usize,
        data_len: off as usize + prog.datablock.len(),
    };

    // --- .text: patched MCB + RET + one stub per command -------------------
    let mut text = prog.mcb.clone();
    for (i, pos) in prog.refpos.iter().enumerate() {
        let pos = *pos as usize;
        let k = prog.reftype[i];
        let idx = prog.refidx[i];
        let v = if k == ref_type::LABEL {
            // Jump operands are rel32 displacements relative to the next
            // instruction (end of the 4-byte operand). The base cancels, so
            // the displacement is target offset - (operand + 4).
            idx.wrapping_sub(pos as u32).wrapping_sub(4)
        } else {
            layout.ref_value(k, idx)
        };
        text[pos..pos + 4].copy_from_slice(&v.to_le_bytes());
    }
    text.push(0xC3); // the MCB ends in POPAD with no return; terminate it
    for _ in 0..ncmd {
        text.push(0xC3); // stub: placeholder body for one command
    }

    // --- .data: variable space, strings, data block -----------------------
    let mut data = vec![0u8; prog.varsize as usize];
    for s in &prog.strings {
        data.extend_from_slice(s.as_bytes());
        data.push(0);
    }
    data.extend_from_slice(&prog.datablock);

    // --- symbols -----------------------------------------------------------
    let mut symbols: Vec<Symbol> = Vec::new();
    symbols.push(Symbol {
        name: "CORE_Program".to_string(),
        value: TEXT_BASE,
        size: mcb_len + 1,
        is_func: true,
    });
    for &a in funcs {
        symbols.push(Symbol {
            name: format!("FUNC_{a:X}"),
            value: TEXT_BASE + a,
            size: 0,
            is_func: true,
        });
    }
    for (i, name) in prog.cmd_name.iter().enumerate() {
        let stem = name.trim_start_matches('?').split('@').next().unwrap_or("");
        let ident: String = keyword(stem)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let ident = ident.trim_matches('_').to_string();
        let nm = if ident.is_empty() {
            format!("cmd{i:04X}")
        } else {
            format!("cmd{i:04X}_{ident}")
        };
        symbols.push(Symbol {
            name: nm,
            value: layout.stub(i as u32),
            size: 1,
            is_func: true,
        });
    }
    for (o, n) in RESERVED {
        symbols.push(Symbol {
            name: n.to_string(),
            value: var_base + o,
            size: 4,
            is_func: false,
        });
    }
    let types = parse_utype(&prog.utype);
    for (i, &o) in prog.dynvar.iter().enumerate() {
        let t = *prog.dynvartype.get(i).unwrap_or(&u32::MAX);
        let name = match types.get(&t) {
            Some(n) => format!("dyn{i}_{n}"),
            None => format!("dyn{i}"),
        };
        symbols.push(Symbol {
            name,
            value: var_base + o,
            size: 4,
            is_func: false,
        });
    }
    for (i, a) in layout.strings.iter().enumerate() {
        symbols.push(Symbol {
            name: format!("str{i}"),
            value: *a,
            size: prog.strings[i].len() as u32 + 1,
            is_func: false,
        });
    }
    for i in 0..prog.dstrings.len() {
        symbols.push(Symbol {
            name: format!("data{i}"),
            value: datablock_base + 10 * i as u32,
            size: 10,
            is_func: false,
        });
    }

    Image {
        bytes: emit_elf(&text, &data, &symbols),
        layout,
        symbols,
    }
}

/// The minimal ELF32 that maps `text` and `data` at `TEXT_BASE`.
fn emit_elf(text: &[u8], data: &[u8], symbols: &[Symbol]) -> Vec<u8> {
    let text_len = text.len() as u32;
    let data_len = data.len() as u32;
    let total = text_len + data_len;
    let img_off = 84u32; // ehdr (52) + one phdr (32)
    let shnum = 6u16;
    let sh_off = img_off + total;

    // section names; e_shstrndx points at section 5
    let shstrtab = b"\0.text\0.data\0.symtab\0.strtab\0.shstrtab\0";
    let n_text = 1u32;
    let n_data = 7;
    let n_symtab = 13;
    let n_strtab = 21;
    let n_shstrtab = 29;

    // symbol-name string table, with each symbol's name index
    let mut symstr = vec![0u8];
    let mut name_idx: Vec<u32> = Vec::with_capacity(symbols.len());
    for s in symbols {
        name_idx.push(symstr.len() as u32);
        symstr.extend_from_slice(s.name.as_bytes());
        symstr.push(0);
    }
    let symstr_off = sh_off + shnum as u32 * 40;
    let nsym = (symbols.len() + 1) as u32;
    let symtab_len = nsym * 16;
    let symtab_off = symstr_off + symstr.len() as u32;
    let shstr_off = symtab_off + symtab_len;

    let mut out = Vec::new();

    // ELF header
    out.extend_from_slice(b"\x7fELF\x01\x01\x01\x00");
    out.extend_from_slice(&[0u8; 8]); // rest of e_ident, then e_type low byte pad
    out.extend_from_slice(&2u16.to_le_bytes()); // e_type    ET_EXEC
    out.extend_from_slice(&3u16.to_le_bytes()); // e_machine EM_386
    out.extend_from_slice(&1u32.to_le_bytes()); // e_version EV_CURRENT
    out.extend_from_slice(&TEXT_BASE.to_le_bytes());
    out.extend_from_slice(&52u32.to_le_bytes()); // e_phoff
    out.extend_from_slice(&sh_off.to_le_bytes()); // e_shoff
    out.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    out.extend_from_slice(&52u16.to_le_bytes()); // e_ehsize
    out.extend_from_slice(&32u16.to_le_bytes()); // e_phentsize
    out.extend_from_slice(&1u16.to_le_bytes()); // e_phnum
    out.extend_from_slice(&40u16.to_le_bytes()); // e_shentsize
    out.extend_from_slice(&shnum.to_le_bytes());
    out.extend_from_slice(&(shnum - 1).to_le_bytes()); // e_shstrndx

    // one PT_LOAD over .text and .data
    out.extend_from_slice(&1u32.to_le_bytes()); // p_type  PT_LOAD
    out.extend_from_slice(&img_off.to_le_bytes());
    out.extend_from_slice(&TEXT_BASE.to_le_bytes()); // p_vaddr
    out.extend_from_slice(&TEXT_BASE.to_le_bytes()); // p_paddr
    out.extend_from_slice(&total.to_le_bytes()); // p_filesz
    out.extend_from_slice(&total.to_le_bytes()); // p_memsz
    out.extend_from_slice(&7u32.to_le_bytes()); // p_flags R|W|X
    out.extend_from_slice(&0x1000u32.to_le_bytes());

    // image bytes
    out.extend_from_slice(text);
    out.extend_from_slice(data);

    // section headers
    fn sec(
        name: u32,
        st: u32,
        flags: u32,
        addr: u32,
        off: u32,
        size: u32,
        link: u32,
        info: u32,
        align: u32,
        entsize: u32,
    ) -> [u8; 40] {
        let mut h = [0u8; 40];
        for (i, v) in [name, st, flags, addr, off, size, link, info, align, entsize]
            .iter()
            .enumerate()
        {
            h[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        h
    }

    out.extend_from_slice(&[0u8; 40]);
    out.extend_from_slice(&sec(n_text, 1, 6, TEXT_BASE, img_off, text_len, 0, 0, 16, 0));
    out.extend_from_slice(&sec(n_data, 1, 3, TEXT_BASE + text_len, img_off + text_len, data_len, 0, 0, 16, 0));
    out.extend_from_slice(&sec(n_symtab, 2, 0, 0, symtab_off, symtab_len, 4, 0, 4, 16));
    out.extend_from_slice(&sec(n_strtab, 3, 0, 0, symstr_off, symstr.len() as u32, 0, 0, 1, 0));
    out.extend_from_slice(&sec(n_shstrtab, 3, 0, 0, shstr_off, shstrtab.len() as u32, 0, 0, 1, 0));

    // symbol-name table, symbol table, section-name table
    out.extend_from_slice(&symstr);
    out.extend_from_slice(&[0u8; 16]); // null symbol
    for (s, idx) in symbols.iter().zip(&name_idx) {
        out.extend_from_slice(&idx.to_le_bytes());
        out.extend_from_slice(&s.value.to_le_bytes());
        out.extend_from_slice(&s.size.to_le_bytes());
        let info: u8 = 0x10 | if s.is_func { 2 } else { 1 }; // GLOBAL | FUNC/OBJECT
        out.push(info);
        out.push(0); // other
        let shndx: u16 = if s.value < TEXT_BASE + text_len { 1 } else { 2 };
        out.extend_from_slice(&shndx.to_le_bytes());
    }
    out.extend_from_slice(shstrtab);

    out
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::Prog;

    #[test]
    fn patches_every_ref_slot() {
        let blob = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/sample_virtual.dat");
        let prog = Prog::load_file(&blob).expect("sample blob");
        let funcs: Vec<u32> = Vec::new();
        let img = build(&prog, &funcs);
        assert!(!img.bytes.is_empty());
        let img_off = 84usize; // ehdr + phdr
        // every ref slot now holds a real value, not the placeholder
        for (i, pos) in prog.refpos.iter().enumerate() {
            let pos = img_off + *pos as usize;
            let v = u32::from_le_bytes(img.bytes[pos..pos + 4].try_into().unwrap());
            assert_ne!(v, 0xFFFF_FFFF, "ref slot {i} still a placeholder");
        }
        // the MCB now ends with a RET, then one stub byte per command
        assert_eq!(img.bytes[img_off + img.layout.text_len - prog.cmd_name.len() - 1], 0xC3);
    }

    #[test]
    fn elf_header_is_well_formed() {
        let blob = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/sample_virtual.dat");
        let prog = Prog::load_file(&blob)
            .expect("sample blob");
        let img = build(&prog, &[]);
        let b = &img.bytes;
        assert_eq!(&b[0..4], b"\x7fELF");
        assert_eq!(b[4], 1); // 32-bit
        assert_eq!(b[5], 1); // little-endian
        let e_entry = u32::from_le_bytes(b[24..28].try_into().unwrap());
        assert_eq!(e_entry, TEXT_BASE);
        let phnum = u16::from_le_bytes(b[44..46].try_into().unwrap());
        assert_eq!(phnum, 1);
        let shnum = u16::from_le_bytes(b[48..50].try_into().unwrap());
        assert_eq!(shnum, 6);
        let shstrndx = u16::from_le_bytes(b[50..52].try_into().unwrap());
        assert_eq!(shstrndx, 5);
        // section names resolve through the section headers
        let sh_off = 84usize + img.layout.text_len + img.layout.data_len;
        let shstr_off = u32::from_le_bytes(
            img.bytes[sh_off + 5 * 40 + 16..sh_off + 5 * 40 + 20]
                .try_into()
                .unwrap(),
        );
        let name4 = u32::from_le_bytes(
            img.bytes[sh_off + 4 * 40..sh_off + 4 * 40 + 4]
                .try_into()
                .unwrap(),
        );
        let name5 = u32::from_le_bytes(
            img.bytes[sh_off + 5 * 40..sh_off + 5 * 40 + 4]
                .try_into()
                .unwrap(),
        );
        let tab = &img.bytes[shstr_off as usize..];
        assert_eq!(shstr_name(tab, name4), ".strtab");
        assert_eq!(shstr_name(tab, name5), ".shstrtab");
    }

    fn shstr_name(tab: &[u8], off: u32) -> &str {
        let s = &tab[off as usize..];
        let end = s.iter().position(|&b| b == 0).unwrap();
        std::str::from_utf8(&s[..end]).unwrap()
    }
    #[test]
    fn utype_entries_parse() {
        let t = parse_utype(b"a:1:LF:b:2:");
        assert_eq!(t.get(&1).map(String::as_str), Some("a"));
        assert_eq!(t.get(&2).map(String::as_str), Some("b"));
    }
}
