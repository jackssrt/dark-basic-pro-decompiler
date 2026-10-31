//! The user-type block: the struct and user-function signatures the compiler
//! recorded in the blob.
//!
//! `CASMWriter::UpdateStructurePatternData` (`ASMWriter.cpp:2189`) walks the
//! struct table and, for every entry that has a declaration chain, writes one
//! NUL-terminated record:
//!
//! ```text
//! <TypeName>:<struct index>:<field type letters>:
//! ```
//!
//! `UpdateStructurePatternDataRec` (`ASMWriter.cpp:2160`) appends one letter per
//! field -- `L` integer, `F` float, `S` string, `B` boolean, `Y` byte, `W` word,
//! `D` dword, `O` double float, `R` double integer, `-` for anything else --
//! and expands a nested user type in place, so the letters are the flattened
//! field list in declaration order.  A user `FUNCTION` is registered in the
//! same table (`Statement.cpp:3886`), so its name and parameter types are in
//! here too.
//!
//! Field *names* are not in the format: the compiler uses them only while
//! compiling.  The layout is, because `CStructTable::CalculateSize`
//! (`StructTable.cpp:289`) puts each field at the running offset with the size
//! of its type, and every type is 4 bytes wide except the two 8-byte doubles.

use std::collections::BTreeSet;

use crate::container::latin1;

/// One field of a user type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// Byte offset inside the record.
    pub offset: u32,
    /// The DBP type letter the compiler wrote.
    pub letter: char,
}

impl Field {
    /// Bytes the field occupies (`CStructTable::CalculateSize`).
    pub fn size(self) -> u32 {
        match self.letter {
            'O' | 'R' => 8,
            _ => 4,
        }
    }

    /// The DBP type name for the field letter.
    pub fn type_name(self) -> &'static str {
        match self.letter {
            'L' => "integer",
            'F' => "float",
            'S' => "string",
            'B' => "boolean",
            'Y' => "byte",
            'W' => "word",
            'D' => "dword",
            'O' => "double float",
            'R' => "double integer",
            _ => "user type",
        }
    }
}

/// A struct or user function recorded in the blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UType {
    /// The identifier the programmer wrote.
    pub name: String,
    /// The struct table index the compiler assigned it.
    pub index: u32,
    /// Fields, in declaration order, at their computed offsets.
    pub fields: Vec<Field>,
}

impl UType {
    /// Bytes the whole record occupies.
    pub fn size(&self) -> u32 {
        self.fields.last().map_or(0, |f| f.offset + f.size())
    }

    /// The field at a byte offset, if one starts there.
    pub fn field_at(&self, offset: u32) -> Option<Field> {
        self.fields.iter().copied().find(|f| f.offset == offset)
    }

    /// `+0 integer, +4 float, ...` -- one line, for the report and the header.
    pub fn describe(&self) -> String {
        if self.fields.is_empty() {
            return "(no fields)".to_string();
        }
        let mut s = String::new();
        for (i, f) in self.fields.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!("+{} {}", f.offset, f.type_name()));
        }
        s
    }
}

/// Decode the block.  The compiler builds it as one C string (`strcpy` on the
/// first record, `strcat` after), so the records are `Name:index:letters:`
/// concatenated with no separator at all.  Split on the colons and read them
/// three at a time; a record whose index is not a number is skipped, as is a
/// truncated tail.
pub fn parse(blob: &[u8]) -> Vec<UType> {
    let text = latin1(blob);
    let text = text.split('\0').next().unwrap_or("");
    let parts: Vec<&str> = text.split(':').collect();
    let mut out = Vec::with_capacity(parts.len() / 3);
    for rec in parts.chunks_exact(3) {
        let (name, index, letters) = (rec[0], rec[1], rec[2]);
        let Ok(index) = index.parse::<u32>() else {
            continue;
        };
        if name.is_empty() {
            continue;
        }
        let mut offset = 0u32;
        let mut fields = Vec::with_capacity(letters.len());
        for c in letters.chars() {
            let f = Field {
                offset,
                letter: c,
            };
            offset = offset.saturating_add(f.size());
            fields.push(f);
        }
        out.push(UType {
            name: name.to_string(),
            index,
            fields,
        });
    }
    out
}

/// What the blob's records are: struct layouts, and the signatures of the
/// user functions the recovered code calls.
#[derive(Clone, Debug, Default)]
pub struct Signatures {
    /// Records that describe a struct.
    pub types: Vec<UType>,
    /// `(function entry address, signature)`, in address order.
    pub functions: Vec<(u32, UType)>,
}

/// Split the records into struct layouts and user-function signatures.
///
/// The block carries no marker saying which is which. What it does carry is
/// declaration order, and the recovered code gives the same order for its
/// functions (address order is source order), plus each function's parameter
/// count from its call sites. A signature starts with the synthetic
/// `returnvalue` slot the compiler puts in front of every parameter chain, so a
/// record only fits a function when it has at least one letter per parameter
/// after that slot. Matching runs from the end, so a game that declares types
/// after its functions still lines up.
pub fn align(records: Vec<UType>, functions: &[(u32, usize)]) -> Signatures {
    let mut out = Signatures::default();
    let mut used = vec![false; records.len()];
    let mut budget: Vec<Option<UType>> = vec![None; functions.len()];
    let mut j = records.len();
    for i in (0..functions.len()).rev() {
        let want = functions[i].1;
        let mut found = None;
        while j > 0 {
            j -= 1;
            let r = &records[j];
            // one letter for the return value, then one per parameter
            if r.fields.len() > want && r.fields[0].letter == 'L' {
                found = Some(j);
                break;
            }
        }
        if let Some(j) = found {
            used[j] = true;
            let mut t = records[j].clone();
            // the leading letter is the return slot, not a parameter
            t.fields.remove(0);
            for f in &mut t.fields {
                f.offset -= 4;
            }
            budget[i] = Some(t);
        }
    }
    for (i, sig) in budget.into_iter().enumerate() {
        if let Some(sig) = sig {
            out.functions.push((functions[i].0, sig));
        }
    }
    out.types = records
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !used[*i])
        .map(|(_, t)| t)
        .collect();
    out
}

/// Which user type a whole array's element accesses fit.
///
/// The blob does not say what an array's element type is, but the code does:
/// every field it reads or writes lands on a field offset of the type. When
/// exactly one type has all of them, that is the element type.
pub fn fit<'a>(
    types: &'a [UType],
    fields: &BTreeSet<u32>,
) -> Option<&'a UType> {
    if fields.len() < 2 {
        return None;
    }
    let mut hit: Option<&UType> = None;
    for t in types {
        if fields.iter().all(|f| t.field_at(*f).is_some()) {
            if hit.is_some() {
                return None; // ambiguous
            }
            hit = Some(t);
        }
    }
    hit
}

/// The lines that describe the recovered user types, for the header of a
/// recovered file.
pub fn describe(sig: &Signatures) -> Vec<String> {
    let mut out = Vec::new();
    for t in &sig.types {
        out.push(format!("'   {:<16} {} bytes; {}", t.name, t.size(), t.describe()));
    }
    for (addr, f) in &sig.functions {
        let mut names: Vec<&str> = f.fields.iter().map(|x| x.type_name()).collect();
        if names.is_empty() {
            names.push("no parameters");
        }
        out.push(format!(
            "'   {:<16} function at {addr:05X}: {}",
            f.name,
            names.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn aligns_signatures_to_functions_from_the_end() {
        let records = vec![
            UType {
                name: "Door".into(),
                index: 1,
                fields: vec![Field { offset: 0, letter: 'L' }],
            },
            UType {
                name: "open".into(),
                index: 2,
                fields: (0..3).map(|i| Field { offset: i * 4, letter: 'L' }).collect(),
            },
            UType {
                name: "close".into(),
                index: 3,
                fields: (0..2).map(|i| Field { offset: i * 4, letter: 'L' }).collect(),
            },
        ];
        let sig = align(records, &[(0x100, 2), (0x200, 1)]);
        assert_eq!(sig.types.len(), 1);
        assert_eq!(sig.types[0].name, "Door");
        assert_eq!(sig.functions.len(), 2);
        assert_eq!(sig.functions[0].0, 0x100);
        assert_eq!(sig.functions[0].1.name, "open");
        // the return slot is not a parameter
        assert_eq!(sig.functions[0].1.size(), 8);
        assert_eq!(sig.functions[1].1.name, "close");
    }

    #[test]
    fn parses_named_structs_at_their_offsets() {
        let blob = b"Vector:1:LFFF:Colour:2:BBBB:thing:3:SO:\0";
        let t = parse(blob);
        assert_eq!(t.len(), 3);
        assert_eq!(t[0].name, "Vector");
        assert_eq!(t[0].index, 1);
        assert_eq!(t[0].size(), 16);
        assert_eq!(t[0].field_at(4), Some(Field { offset: 4, letter: 'F' }));
        // the double float is the only 8-byte field
        assert_eq!(t[2].field_at(4).unwrap().size(), 8);
        assert_eq!(t[2].size(), 12);
    }

    #[test]
    fn parses_a_nested_type_that_contributed_no_letters() {
        // a field list made only of nested user types leaves the letters empty
        let t = parse(b"Outer:4::Vector:1:LFFF:");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].name, "Outer");
        assert!(t[0].fields.is_empty());
        assert_eq!(t[1].name, "Vector");
    }

    #[test]
    fn skips_entries_that_do_not_parse() {
        let t = parse(b"junkVector:x:LF:Colour:2:LL:");
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "Colour");
    }
}
