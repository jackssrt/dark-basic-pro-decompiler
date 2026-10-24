//! Instruction templates emitted by DBProCompiler's `CASMWriter`.
//!
//! Generated from the reference table recovered from
//! `DBProCompiler/DBPCompiler/ASMWriter.cpp` (`GenerateASMCodes`).
//!
//! Each entry is the runtime byte sequence the writer produced (the operand
//! bytes are absent from the sequence and follow it) plus whether the template
//! declares an operand slot at all.

/// One writer template: mnemonic, opcode bytes, and whether an operand slot
/// follows them.
#[derive(Clone, Copy, Debug)]
pub struct Template {
    /// Mnemonic as the writer spells it, e.g. `"MOV EAX IMM4"`.
    pub name: &'static str,
    /// Opcode bytes present in the emitted stream (`-1` in the reference table
    /// means "no byte here").
    pub bytes: &'static [u8],
    /// `true` when the template declares an operand slot, `false` for the
    /// operands supplied directly by `WriteASMLine`.
    pub slot: bool,
}

/// Every template, in the order the reference table lists them.
pub static TEMPLATES: &[Template] = &[
    Template {
        name: "MOV EAX IMM1",
        bytes: &[0xB0],
        slot: true,
    },
    Template {
        name: "MOV EAX IMM2",
        bytes: &[0x66, 0xB8],
        slot: true,
    },
    Template {
        name: "MOV EAX IMM4",
        bytes: &[0xB8],
        slot: true,
    },
    Template {
        name: "MOV EBX IMM1",
        bytes: &[0xB3],
        slot: true,
    },
    Template {
        name: "MOV EBX IMM2",
        bytes: &[0x66, 0xBB],
        slot: true,
    },
    Template {
        name: "MOV EBX IMM4",
        bytes: &[0xBB],
        slot: true,
    },
    Template {
        name: "MOV EDX IMM4",
        bytes: &[0xBA],
        slot: true,
    },
    Template {
        name: "MOV EAX MEM1",
        bytes: &[0xA0],
        slot: true,
    },
    Template {
        name: "MOV EAX MEM2",
        bytes: &[0x66, 0xA1],
        slot: true,
    },
    Template {
        name: "MOV EAX MEM4",
        bytes: &[0xA1],
        slot: true,
    },
    Template {
        name: "MOV MEM EAX1",
        bytes: &[0xA2],
        slot: true,
    },
    Template {
        name: "MOV MEM EAX2",
        bytes: &[0x66, 0xA3],
        slot: true,
    },
    Template {
        name: "MOV MEM EAX4",
        bytes: &[0xA3],
        slot: true,
    },
    Template {
        name: "MOV [ECX+A] EAX1",
        bytes: &[0x88, 0x81],
        slot: true,
    },
    Template {
        name: "MOV [ECX+A] EAX2",
        bytes: &[0x66, 0x89, 0x81],
        slot: true,
    },
    Template {
        name: "MOV [ECX+A] EAX4",
        bytes: &[0x89, 0x81],
        slot: true,
    },
    Template {
        name: "MOV [EAX+A] ECX1",
        bytes: &[0x88, 0x88],
        slot: true,
    },
    Template {
        name: "MOV [EAX+A] ECX2",
        bytes: &[0x66, 0x89, 0x88],
        slot: true,
    },
    Template {
        name: "MOV [EAX+A] ECX4",
        bytes: &[0x89, 0x88],
        slot: true,
    },
    Template {
        name: "MOV [EAX] ECX1",
        bytes: &[0x88, 0x08],
        slot: false,
    },
    Template {
        name: "MOV [EAX] ECX2",
        bytes: &[0x66, 0x89, 0x08],
        slot: false,
    },
    Template {
        name: "MOV [EAX] ECX4",
        bytes: &[0x89, 0x08],
        slot: false,
    },
    Template {
        name: "MOV EAX [EAX]1",
        bytes: &[0x8A, 0x00],
        slot: false,
    },
    Template {
        name: "MOV EAX [EAX]2",
        bytes: &[0x66, 0x8B, 0x00],
        slot: false,
    },
    Template {
        name: "MOV EAX [EAX]4",
        bytes: &[0x8B, 0x00],
        slot: false,
    },
    Template {
        name: "MOV ECX1 [EAX+A]",
        bytes: &[0x8A, 0x88],
        slot: true,
    },
    Template {
        name: "MOV ECX2 [EAX+A]",
        bytes: &[0x66, 0x8B, 0x88],
        slot: true,
    },
    Template {
        name: "MOV ECX4 [EAX+A]",
        bytes: &[0x8B, 0x88],
        slot: true,
    },
    Template {
        name: "MOV EAX1 [ECX+A]",
        bytes: &[0x8A, 0x81],
        slot: true,
    },
    Template {
        name: "MOV EAX2 [ECX+A]",
        bytes: &[0x66, 0x8B, 0x81],
        slot: true,
    },
    Template {
        name: "MOV EAX4 [ECX+A]",
        bytes: &[0x8B, 0x81],
        slot: true,
    },
    Template {
        name: "MOV EDX1 [EAX+A]",
        bytes: &[0x8A, 0x90],
        slot: true,
    },
    Template {
        name: "MOV EDX2 [EAX+A]",
        bytes: &[0x66, 0x8B, 0x90],
        slot: true,
    },
    Template {
        name: "MOV EDX4 [EAX+A]",
        bytes: &[0x8B, 0x90],
        slot: true,
    },
    Template {
        name: "IMUL EDX4 [EAX+A]",
        bytes: &[0x0F, 0xAF, 0x90],
        slot: true,
    },
    Template {
        name: "MOV EBX1 [EAX+A]",
        bytes: &[0x8A, 0x98],
        slot: true,
    },
    Template {
        name: "MOV EBX2 [EAX+A]",
        bytes: &[0x66, 0x8B, 0x98],
        slot: true,
    },
    Template {
        name: "MOV EBX4 [EAX+A]",
        bytes: &[0x8B, 0x98],
        slot: true,
    },
    Template {
        name: "MOV EAX1 [EBP+A]",
        bytes: &[0x8A, 0x85],
        slot: true,
    },
    Template {
        name: "MOV EAX2 [EBP+A]",
        bytes: &[0x66, 0x8B, 0x85],
        slot: true,
    },
    Template {
        name: "MOV EAX4 [EBP+A]",
        bytes: &[0x8B, 0x85],
        slot: true,
    },
    Template {
        name: "MOV EBX1 [EBP+A]",
        bytes: &[0x8A, 0x9D],
        slot: true,
    },
    Template {
        name: "MOV EBX2 [EBP+A]",
        bytes: &[0x66, 0x8B, 0x9D],
        slot: true,
    },
    Template {
        name: "MOV EBX4 [EBP+A]",
        bytes: &[0x8B, 0x9D],
        slot: true,
    },
    Template {
        name: "MOV [EBP+A] EAX1",
        bytes: &[0x88, 0x85],
        slot: true,
    },
    Template {
        name: "MOV [EBP+A] EAX2",
        bytes: &[0x66, 0x89, 0x85],
        slot: true,
    },
    Template {
        name: "MOV [EBP+A] EAX4",
        bytes: &[0x89, 0x85],
        slot: true,
    },
    Template {
        name: "MOV EDX EAX",
        bytes: &[0x8B, 0xD0],
        slot: false,
    },
    Template {
        name: "MOV ECX IMM4",
        bytes: &[0xB9],
        slot: true,
    },
    Template {
        name: "MOV ECX EAX",
        bytes: &[0x8B, 0xC8],
        slot: false,
    },
    Template {
        name: "MOV ECX EDX",
        bytes: &[0x8B, 0xCA],
        slot: false,
    },
    Template {
        name: "MOV EDX ECX",
        bytes: &[0x8B, 0xD1],
        slot: false,
    },
    Template {
        name: "MOV EAX EBX",
        bytes: &[0x8B, 0xC3],
        slot: false,
    },
    Template {
        name: "MOV ECX EBX1",
        bytes: &[0x8A, 0xCB],
        slot: false,
    },
    Template {
        name: "MOV ECX EBX2",
        bytes: &[0x66, 0x8B, 0xCB],
        slot: false,
    },
    Template {
        name: "MOV ECX EBX4",
        bytes: &[0x8B, 0xCB],
        slot: false,
    },
    Template {
        name: "MOV EAX ECX1",
        bytes: &[0x8A, 0xC1],
        slot: true,
    },
    Template {
        name: "MOV EAX ECX2",
        bytes: &[0x66, 0x8B, 0xC1],
        slot: true,
    },
    Template {
        name: "MOV EAX ECX4",
        bytes: &[0x8B, 0xC1],
        slot: true,
    },
    Template {
        name: "MOV EAX EDX1",
        bytes: &[0x8A, 0xC2],
        slot: true,
    },
    Template {
        name: "MOV EAX EDX2",
        bytes: &[0x66, 0x8B, 0xC2],
        slot: true,
    },
    Template {
        name: "MOV EAX EDX4",
        bytes: &[0x8B, 0xC2],
        slot: true,
    },
    Template {
        name: "MOV EBX EAX1",
        bytes: &[0x8A, 0xD8],
        slot: true,
    },
    Template {
        name: "MOV EBX EAX2",
        bytes: &[0x66, 0x8B, 0xD8],
        slot: true,
    },
    Template {
        name: "MOV EBX EAX4",
        bytes: &[0x8B, 0xD8],
        slot: true,
    },
    Template {
        name: "ADD EAX EBX1",
        bytes: &[0x00, 0xD8],
        slot: false,
    },
    Template {
        name: "ADD EAX EBX2",
        bytes: &[0x66, 0x01, 0xD8],
        slot: false,
    },
    Template {
        name: "ADD EAX EBX4",
        bytes: &[0x01, 0xD8],
        slot: false,
    },
    Template {
        name: "ADD EAX ECX4",
        bytes: &[0x01, 0xC8],
        slot: false,
    },
    Template {
        name: "ADD ESP",
        bytes: &[0x81, 0xC4],
        slot: true,
    },
    Template {
        name: "SUB ESP",
        bytes: &[0x81, 0xEC],
        slot: true,
    },
    Template {
        name: "SUB ESP EAX",
        bytes: &[0x29, 0xC4],
        slot: false,
    },
    Template {
        name: "PUSH EBP",
        bytes: &[0x55],
        slot: true,
    },
    Template {
        name: "POP EBP",
        bytes: &[0x5D],
        slot: true,
    },
    Template {
        name: "MOV EBP ESP",
        bytes: &[0x89, 0xE5],
        slot: true,
    },
    Template {
        name: "MOV ESP EBP",
        bytes: &[0x89, 0xEC],
        slot: true,
    },
    Template {
        name: "MOV EAX ESP",
        bytes: &[0x89, 0xE0],
        slot: false,
    },
    Template {
        name: "MOV EAX EBP",
        bytes: &[0x89, 0xE8],
        slot: false,
    },
    Template {
        name: "MOV MEM IMM1",
        bytes: &[0xC6, 0x05],
        slot: true,
    },
    Template {
        name: "MOV MEM IMM2",
        bytes: &[0x66, 0xC7, 0x05],
        slot: true,
    },
    Template {
        name: "MOV MEM IMM4",
        bytes: &[0xC7, 0x05],
        slot: true,
    },
    Template {
        name: "MOV [EBP+A] IMM1",
        bytes: &[0xC6, 0x85],
        slot: true,
    },
    Template {
        name: "MOV [EBP+A] IMM2",
        bytes: &[0x66, 0xC7, 0x85],
        slot: true,
    },
    Template {
        name: "MOV [EBP+A] IMM4",
        bytes: &[0xC7, 0x85],
        slot: true,
    },
    Template {
        name: "REL MOV [AX] DX1",
        bytes: &[0x88, 0x10],
        slot: true,
    },
    Template {
        name: "REL MOV [AX] DX2",
        bytes: &[0x66, 0x89, 0x10],
        slot: true,
    },
    Template {
        name: "REL MOV [EAX] EDX",
        bytes: &[0x89, 0x10],
        slot: true,
    },
    Template {
        name: "REL MOV AX [DX]1",
        bytes: &[0x8A, 0x02],
        slot: true,
    },
    Template {
        name: "REL MOV AX [DX]2",
        bytes: &[0x66, 0x8B, 0x02],
        slot: true,
    },
    Template {
        name: "REL MOV EAX [EDX]",
        bytes: &[0x8B, 0x02],
        slot: true,
    },
    Template {
        name: "FSTP [MEM] ST08",
        bytes: &[0xDD, 0x1D],
        slot: true,
    },
    Template {
        name: "FLD ST08 [MEM]",
        bytes: &[0xDD, 0x05],
        slot: true,
    },
    Template {
        name: "FSTP [EBP+A] ST08",
        bytes: &[0xDD, 0x9D],
        slot: true,
    },
    Template {
        name: "FLD ST08 [EBP+A]",
        bytes: &[0xDD, 0x85],
        slot: true,
    },
    Template {
        name: "FSTP [EAX+A] ST08",
        bytes: &[0xDD, 0x98],
        slot: true,
    },
    Template {
        name: "FLD ST08 [EAX+A]",
        bytes: &[0xDD, 0x80],
        slot: true,
    },
    Template {
        name: "FSTP [ECX+A] ST08",
        bytes: &[0xDD, 0x99],
        slot: true,
    },
    Template {
        name: "FLD ST08 [ECX+A]",
        bytes: &[0xDD, 0x81],
        slot: true,
    },
    Template {
        name: "PUSH EAX",
        bytes: &[0x50],
        slot: true,
    },
    Template {
        name: "PUSH EDX",
        bytes: &[0x52],
        slot: true,
    },
    Template {
        name: "PUSH EBX",
        bytes: &[0x53],
        slot: true,
    },
    Template {
        name: "PUSH ESP",
        bytes: &[0x54],
        slot: true,
    },
    Template {
        name: "PUSH ECX",
        bytes: &[0x51],
        slot: true,
    },
    Template {
        name: "PUSH REL AX1",
        bytes: &[0xFF, 0x30],
        slot: true,
    },
    Template {
        name: "PUSH REL AX2",
        bytes: &[0xFF, 0x30],
        slot: true,
    },
    Template {
        name: "PUSH REL EAX",
        bytes: &[0xFF, 0x30],
        slot: true,
    },
    Template {
        name: "PUSH [EBP+A]",
        bytes: &[0xFF, 0xB5],
        slot: true,
    },
    Template {
        name: "PUSH [EAX]",
        bytes: &[0xFF, 0x30],
        slot: false,
    },
    Template {
        name: "CALL EAX",
        bytes: &[0xFF, 0xD0],
        slot: false,
    },
    Template {
        name: "CALL EBX",
        bytes: &[0xFF, 0xD3],
        slot: false,
    },
    Template {
        name: "CALL MEM",
        bytes: &[0xE8],
        slot: true,
    },
    Template {
        name: "CALL REL",
        bytes: &[0xE8],
        slot: true,
    },
    Template {
        name: "POP EAX",
        bytes: &[0x58],
        slot: false,
    },
    Template {
        name: "POP EBX",
        bytes: &[0x5B],
        slot: false,
    },
    Template {
        name: "RET",
        bytes: &[0xC3],
        slot: false,
    },
    Template {
        name: "POP EDX",
        bytes: &[0x5A],
        slot: false,
    },
    Template {
        name: "POP ECX",
        bytes: &[0x59],
        slot: false,
    },
    Template {
        name: "???",
        bytes: &[],
        slot: false,
    },
    Template {
        name: "CMP EAX1",
        bytes: &[0x3C],
        slot: true,
    },
    Template {
        name: "CMP EAX2",
        bytes: &[0x66, 0x3D],
        slot: true,
    },
    Template {
        name: "CMP EAX4",
        bytes: &[0x3D],
        slot: true,
    },
    Template {
        name: "CMP EBX1",
        bytes: &[0x80, 0xFB],
        slot: true,
    },
    Template {
        name: "CMP EBX2",
        bytes: &[0x66, 0x81, 0xFB],
        slot: true,
    },
    Template {
        name: "CMP EBX4",
        bytes: &[0x81, 0xFB],
        slot: true,
    },
    Template {
        name: "CMP EDX EBX4",
        bytes: &[0x3B, 0xDA],
        slot: false,
    },
    Template {
        name: "CMP EDX EBX1",
        bytes: &[0x3A, 0xDA],
        slot: false,
    },
    Template {
        name: "CMP EDX EBX2",
        bytes: &[0x66, 0x3B, 0xDA],
        slot: false,
    },
    Template {
        name: "CMP EAX EBX4",
        bytes: &[0x3B, 0xD8],
        slot: false,
    },
    Template {
        name: "SETE EAX",
        bytes: &[0x0F, 0x94, 0xC0],
        slot: false,
    },
    Template {
        name: "SETNE EAX",
        bytes: &[0x0F, 0x95, 0xC0],
        slot: false,
    },
    Template {
        name: "SETG EAX",
        bytes: &[0x0F, 0x9F, 0xC0],
        slot: false,
    },
    Template {
        name: "SETGE EAX",
        bytes: &[0x0F, 0x9D, 0xC0],
        slot: false,
    },
    Template {
        name: "SETL EAX",
        bytes: &[0x0F, 0x9C, 0xC0],
        slot: false,
    },
    Template {
        name: "SETLE EAX",
        bytes: &[0x0F, 0x9E, 0xC0],
        slot: false,
    },
    Template {
        name: "JMP",
        bytes: &[0xE9],
        slot: true,
    },
    Template {
        name: "JNE",
        bytes: &[0x0F, 0x85],
        slot: true,
    },
    Template {
        name: "JE",
        bytes: &[0x0F, 0x84],
        slot: true,
    },
    Template {
        name: "JMP REL",
        bytes: &[0xFF, 0x25],
        slot: true,
    },
    Template {
        name: "JMP EBX",
        bytes: &[0xFF, 0xE3],
        slot: true,
    },
    Template {
        name: "JGE",
        bytes: &[0x0F, 0x8D],
        slot: true,
    },
    Template {
        name: "JLE",
        bytes: &[0x0F, 0x8E],
        slot: true,
    },
    Template {
        name: "MOV MEM4 ESP",
        bytes: &[0x89, 0x25],
        slot: true,
    },
    Template {
        name: "MOV ESP MEM4",
        bytes: &[0x8B, 0x25],
        slot: true,
    },
    Template {
        name: "MOV EBX MEM4",
        bytes: &[0x8B, 0x1D],
        slot: true,
    },
    Template {
        name: "MOV MEM EBX4",
        bytes: &[0x89, 0x1D],
        slot: true,
    },
    Template {
        name: "MOV EAX SIB[EAX:EBX*4]",
        bytes: &[0x8B, 0x04, 0x98],
        slot: false,
    },
    Template {
        name: "MOV SIB[EAX:ECX*1],IMM1",
        bytes: &[0xC6, 0x04, 0x08],
        slot: true,
    },
    Template {
        name: "MOV SIB[EAX:ECX*4],IMM4",
        bytes: &[0xC7, 0x04, 0x88],
        slot: true,
    },
    Template {
        name: "PUSH REGISTERS",
        bytes: &[0x60],
        slot: false,
    },
    Template {
        name: "POP REGISTERS",
        bytes: &[0x61],
        slot: false,
    },
    Template {
        name: "LOOP ECX",
        bytes: &[0xE2],
        slot: true,
    },
    Template {
        name: "PUSH IMM4",
        bytes: &[0x68],
        slot: true,
    },
    Template {
        name: "INC MEM1",
        bytes: &[0xFE, 0x05],
        slot: true,
    },
    Template {
        name: "INC MEM2",
        bytes: &[0x66, 0xFF, 0x05],
        slot: true,
    },
    Template {
        name: "INC MEM4",
        bytes: &[0xFF, 0x05],
        slot: true,
    },
    Template {
        name: "DEC MEM1",
        bytes: &[0xFE, 0x0D],
        slot: true,
    },
    Template {
        name: "DEC MEM2",
        bytes: &[0x66, 0xFF, 0x0D],
        slot: true,
    },
    Template {
        name: "DEC MEM4",
        bytes: &[0xFF, 0x0D],
        slot: true,
    },
    Template {
        name: "ADD EAX IMM1",
        bytes: &[0x04],
        slot: true,
    },
    Template {
        name: "ADD EAX IMM2",
        bytes: &[0x66, 0x05],
        slot: true,
    },
    Template {
        name: "ADD EAX IMM4",
        bytes: &[0x05],
        slot: true,
    },
    Template {
        name: "SUB EAX IMM1",
        bytes: &[0x2C],
        slot: true,
    },
    Template {
        name: "SUB EAX IMM2",
        bytes: &[0x66, 0x2D],
        slot: true,
    },
    Template {
        name: "SUB EAX IMM4",
        bytes: &[0x2D],
        slot: true,
    },
    Template {
        name: "SUB EAX EBX1",
        bytes: &[0x28, 0xD8],
        slot: false,
    },
    Template {
        name: "SUB EAX EBX2",
        bytes: &[0x66, 0x29, 0xD8],
        slot: false,
    },
    Template {
        name: "SUB EAX EBX4",
        bytes: &[0x29, 0xD8],
        slot: false,
    },
    Template {
        name: "IMUL EAX EBX1",
        bytes: &[0xF6, 0xEB],
        slot: false,
    },
    Template {
        name: "IMUL EAX EBX2",
        bytes: &[0x66, 0xF7, 0xEB],
        slot: false,
    },
    Template {
        name: "IMUL EAX EBX4",
        bytes: &[0xF7, 0xEB],
        slot: false,
    },
    Template {
        name: "CDQ",
        bytes: &[0x99],
        slot: false,
    },
    Template {
        name: "IDIV EAX EBX1",
        bytes: &[0xF6, 0xFB],
        slot: false,
    },
    Template {
        name: "IDIV EAX EBX2",
        bytes: &[0x66, 0xF7, 0xFB],
        slot: false,
    },
    Template {
        name: "IDIV EAX EBX4",
        bytes: &[0xF7, 0xFB],
        slot: false,
    },
    Template {
        name: "AND EAX IMM1",
        bytes: &[0x24],
        slot: true,
    },
    Template {
        name: "AND EAX IMM2",
        bytes: &[0x66, 0x25],
        slot: true,
    },
    Template {
        name: "AND EAX IMM4",
        bytes: &[0x25],
        slot: true,
    },
    Template {
        name: "AND EAX EBX1",
        bytes: &[0x20, 0xD8],
        slot: false,
    },
    Template {
        name: "AND EAX EBX2",
        bytes: &[0x66, 0x21, 0xD8],
        slot: false,
    },
    Template {
        name: "AND EAX EBX4",
        bytes: &[0x21, 0xD8],
        slot: false,
    },
    Template {
        name: "OR EAX IMM1",
        bytes: &[0x0C],
        slot: true,
    },
    Template {
        name: "OR EAX IMM2",
        bytes: &[0x66, 0x0D],
        slot: true,
    },
    Template {
        name: "OR EAX IMM4",
        bytes: &[0x0D],
        slot: true,
    },
    Template {
        name: "OR EAX EBX1",
        bytes: &[0x08, 0xD8],
        slot: false,
    },
    Template {
        name: "OR EAX EBX2",
        bytes: &[0x66, 0x09, 0xD8],
        slot: false,
    },
    Template {
        name: "OR EAX EBX4",
        bytes: &[0x09, 0xD8],
        slot: false,
    },
    Template {
        name: "NOT EAX1",
        bytes: &[0xF6, 0xD0],
        slot: false,
    },
    Template {
        name: "NOT EAX2",
        bytes: &[0x66, 0xF7, 0xD0],
        slot: false,
    },
    Template {
        name: "NOT EAX4",
        bytes: &[0xF7, 0xD0],
        slot: false,
    },
    Template {
        name: "XOR EAX IMM1",
        bytes: &[0x34],
        slot: true,
    },
    Template {
        name: "XOR EAX IMM2",
        bytes: &[0x66, 0x35],
        slot: true,
    },
    Template {
        name: "XOR EAX IMM4",
        bytes: &[0x35],
        slot: true,
    },
    Template {
        name: "XOR EAX EBX1",
        bytes: &[0x30, 0xD8],
        slot: false,
    },
    Template {
        name: "XOR EAX EBX2",
        bytes: &[0x66, 0x31, 0xD8],
        slot: false,
    },
    Template {
        name: "XOR EAX EBX4",
        bytes: &[0x31, 0xD8],
        slot: false,
    },
    Template {
        name: "SHL EAX1 IMM1",
        bytes: &[0xC0, 0xE0],
        slot: true,
    },
    Template {
        name: "SHL EAX2 IMM2",
        bytes: &[0x66, 0xC1, 0xE0],
        slot: true,
    },
    Template {
        name: "SHL EAX4 IMM4",
        bytes: &[0xC1, 0xE0],
        slot: true,
    },
    Template {
        name: "SHL EAX1 CL",
        bytes: &[0xD2, 0xE0],
        slot: false,
    },
    Template {
        name: "SHL EAX2 CL",
        bytes: &[0x66, 0xD3, 0xE0],
        slot: false,
    },
    Template {
        name: "SHL EAX4 CL",
        bytes: &[0xD3, 0xE0],
        slot: false,
    },
    Template {
        name: "SHR EAX1 IMM1",
        bytes: &[0xC0, 0xE8],
        slot: true,
    },
    Template {
        name: "SHR EAX2 IMM2",
        bytes: &[0x66, 0xC1, 0xE8],
        slot: true,
    },
    Template {
        name: "SHR EAX4 IMM4",
        bytes: &[0xC1, 0xE8],
        slot: true,
    },
    Template {
        name: "SHR EAX1 CL",
        bytes: &[0xD2, 0xE8],
        slot: false,
    },
    Template {
        name: "SHR EAX2 CL",
        bytes: &[0x66, 0xD3, 0xE8],
        slot: false,
    },
    Template {
        name: "SHR EAX4 CL",
        bytes: &[0xD3, 0xE8],
        slot: false,
    },
    Template {
        name: "IMUL ECX EDX4",
        bytes: &[0x0F, 0xAF, 0x0A],
        slot: false,
    },
    Template {
        name: "ADD EBX EDX4",
        bytes: &[0x03, 0xDA],
        slot: false,
    },
];

/// Templates that declare an operand slot but are always emitted without one
/// (`WriteASMLine(ASM_PUSHEAX, "")` and friends). Matching on the reference
/// table alone would otherwise mis-eat the following instruction's operand.
pub static NO_OPERAND: &[&str] = &[
    "???",
    "MOV EAX ECX1",
    "MOV EAX ECX2",
    "MOV EAX ECX4",
    "MOV EAX EDX1",
    "MOV EAX EDX2",
    "MOV EAX EDX4",
    "MOV EBP ESP",
    "MOV EBX EAX1",
    "MOV EBX EAX2",
    "MOV EBX EAX4",
    "MOV ESP EBP",
    "POP EAX",
    "POP EBP",
    "POP EBX",
    "POP ECX",
    "POP EDX",
    "PUSH EAX",
    "PUSH EBP",
    "PUSH EBX",
    "PUSH ECX",
    "PUSH EDX",
    "PUSH ESP",
    "RET",
];

/// Templates written with `WriteASMLine2()` / `WriteASMLine2IMM()`, which
/// carry a second operand.
pub static TWO_OPERAND: &[&str] = &[
    "LOOP ECX",
    "MOV MEM IMM1",
    "MOV MEM IMM2",
    "MOV MEM IMM4",
    "MOV SIB[EAX:ECX*1],IMM1",
    "MOV SIB[EAX:ECX*4],IMM4",
    "MOV [EBP+A] IMM1",
    "MOV [EBP+A] IMM2",
    "MOV [EBP+A] IMM4",
];
