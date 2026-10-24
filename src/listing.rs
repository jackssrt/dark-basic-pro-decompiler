//! The fully symbolised instruction listing.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};

use crate::container::{Prog, Ref, demangle, ref_type};

/// Variable-space slots the runtime owns (`CEXEBlock::Init`).
const RESERVED: &[(u32, &str)] = &[
    (0, "_ESP_"),
    (4, "_ERR_"),
    (8, "_ESC_"),
    (12, "_REK_"),
    (16, "_LINE_"),
];

/// A resolved name for a variable-space slot: `@name(offset)`.
#[derive(Default)]
struct VarNamer {
    cache: HashMap<u32, String>,
    reserved: HashMap<u32, String>,
}

impl VarNamer {
    /// Slots are numbered in order of first appearance, so the number says
    /// nothing beyond "the n-th distinct slot". Reserved slots keep their own
    /// names and take no number.
    fn name(&mut self, off: u32) -> &str {
        if let Some((_, n)) = RESERVED.iter().find(|(o, _)| *o == off) {
            return self
                .reserved
                .entry(off)
                .or_insert_with(|| format!("@{n}({off})"));
        }
        let next = self.cache.len() + 1;
        self.cache
            .entry(off)
            .or_insert_with(|| format!("@v{next}({off})"))
    }
}

/// Append one operand as the listing spells it.
fn operand(
    prog: &Prog,
    reference: Option<Ref>,
    raw: Option<u32>,
    vars: &mut VarNamer,
    out: &mut String,
) {
    let Some(r) = reference else {
        if let Some(v) = raw {
            let _ = write!(out, "{v}");
        }
        return;
    };
    let i = r.index;
    match r.kind {
        ref_type::DLL_CALL => {
            let dll = prog
                .cmd_dll
                .get(i as usize)
                .and_then(|d| d.checked_sub(1))
                .and_then(|d| prog.dllfn.get(d as usize))
                .map_or("", String::as_str);
            let cmd = prog.cmd_name.get(i as usize).map_or("", String::as_str);
            let _ = write!(out, "cmd[{i}] {dll}:{}", demangle(cmd));
        }
        ref_type::STRING => {
            let _ = write!(
                out,
                "str[{i}] \"{}\"",
                prog.strings.get(i as usize).map_or("", String::as_str)
            );
        }
        ref_type::VARIABLE => out.push_str(vars.name(i)),
        ref_type::IMMEDIATE => {
            let _ = write!(out, "{i}");
        }
        ref_type::LABEL => {
            let _ = write!(out, "L_{i:06X}");
        }
        ref_type::DATA => {
            let _ = write!(out, "data[{i}]");
        }
        _ => out.push('?'),
    }
}

/// Write the listing of the instructions in `[lo, hi)`.
pub fn write_listing<W: Write>(
    prog: &Prog,
    lo: u32,
    hi: Option<u32>,
    out: &mut W,
) -> io::Result<()> {
    let hi = hi.unwrap_or(u32::MAX);
    let mut vars = VarNamer::default();
    let mut line = String::with_capacity(96);
    for ins in &prog.ins {
        if ins.addr < lo || ins.addr >= hi {
            continue;
        }
        line.clear();
        let _ = write!(line, "{:06X}  {:<22} ", ins.addr, ins.name);
        let mark = line.len();
        operand(prog, ins.reference, ins.raw, &mut vars, &mut line);
        if ins.reference2.is_some() || ins.raw2.is_some() {
            line.push_str(", ");
            operand(prog, ins.reference2, ins.raw2, &mut vars, &mut line);
        }
        while line.len() - mark < 30 {
            line.push(' ');
        }
        line.push(' ');
        // A truncated tail operand can size an instruction past the end of
        // the block; the listing shows the bytes that are there.
        let end = ((ins.addr + ins.size) as usize).min(prog.mcb.len());
        for (i, b) in prog.mcb[ins.addr as usize..end].iter().enumerate() {
            if i > 0 {
                line.push(' ');
            }
            let _ = write!(line, "{b:02X}");
        }
        line.push('\n');
        out.write_all(line.as_bytes())?;
    }
    Ok(())
}

/// The same listing as a string.
pub fn listing(prog: &Prog, lo: u32, hi: Option<u32>) -> String {
    let mut buf = Vec::with_capacity(prog.mcb.len() * 4);
    write_listing(prog, lo, hi, &mut buf).expect("writing into a Vec cannot fail");
    String::from_utf8(buf).expect("the listing only ever holds ASCII and latin-1 text")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_slots_are_numbered_by_first_appearance() {
        let mut n = VarNamer::default();
        assert_eq!(n.name(100), "@v1(100)");
        assert_eq!(n.name(200), "@v2(200)");
        assert_eq!(n.name(100), "@v1(100)");
        assert_eq!(n.name(8), "@_ESC_(8)");
    }
}
