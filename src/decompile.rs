//! The Machine Code Block turned back into BASIC statements and control flow.
//!
//! Pass 1: linear decode of the instruction stream into statements.
//! Pass 2: resolve every branch target to a label.
//! Pass 3: recover control flow (REPEAT/UNTIL, DO/LOOP, WHILE/ENDWHILE,
//! IF/THEN, user FUNCTION/ENDFUNCTION) from the label graph.
//! Pass 4: emit DBP BASIC text.
//!
//! The emission rules reverse the ones in
//! `DBProCompiler/DBPCompiler/ASMWriter.cpp`.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::container::{Ins, Prog, Ref, ref_type};
use crate::names::keyword;

/// What a recovered statement is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A `MOV [@_LINE_], n` marker: the start of a BASIC statement.
    Line,
    /// A library command call.
    Call,
    /// A user FUNCTION call.
    Funcall,
    /// An assignment or array store.
    Assign,
    /// An IF test with a branch out.
    Cond,
    /// An unconditional branch.
    Jump,
    /// A user function prologue.
    Func,
    /// A note: RETURN, a frame teardown, a recognised runtime-error path.
    Misc,
    /// A run no recogniser claimed.
    Gap,
}

/// One recovered statement.
#[derive(Clone, Debug)]
pub struct Stmt {
    /// Byte offset of the first instruction it covers.
    pub addr: u32,
    /// Byte offset just past the last instruction it covers.
    pub end: u32,
    /// Kind of statement.
    pub kind: Kind,
    /// Rendered text, for the kinds that carry one.
    pub text: Option<String>,
    /// Original source line, for [`Kind::Line`].
    pub line: Option<u32>,
    /// Call arguments and return target, for calls.
    pub extra: Option<(Vec<String>, Option<String>)>,
    /// Branch target address, where the statement has one.
    pub target: Option<u32>,
    /// Branch condition, for [`Kind::Cond`].
    pub cc: Option<&'static str>,
    /// The tested expression, for [`Kind::Cond`].
    pub cond: Option<String>,
}

impl Stmt {
    fn new(addr: u32, kind: Kind) -> Self {
        Self {
            addr,
            end: addr + 1,
            kind,
            text: None,
            line: None,
            extra: None,
            target: None,
            cc: None,
            cond: None,
        }
    }
}

/// Per-run counters for the report.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub cmd: u32,
    pub assign: u32,
    pub cond: u32,
    pub for_: u32,
    pub function: u32,
    pub funcall: u32,
    pub array: u32,
}

/// Reserved variable-space slots used by the runtime (`CEXEBlock::Init`).
const RESERVED: &[(u32, &str)] = &[
    (0, "_ESP_"),
    (4, "_ERR_"),
    (8, "_ESC_"),
    (12, "_REK_"),
    (16, "_LINE_"),
    (20, "_TEMPA_"),
    (24, "_TEMPB_"),
];

/// `SETcc` templates and the comparison they carry.
const CMPOPS: &[(&str, &str)] = &[
    ("SETE EAX", "="),
    ("SETNE EAX", "<>"),
    ("SETG EAX", ">"),
    ("SETGE EAX", ">="),
    ("SETL EAX", "<"),
    ("SETLE EAX", "<="),
];

/// An expression result together with the instruction index that follows it.
type Expr = (String, usize);

/// The decompiler.
pub struct Dec<'a> {
    pub prog: &'a Prog,
    pub ins: &'a [Ins],
    n: usize,
    varnames: HashMap<u32, String>,
    /// Statements in address order.
    pub statements: Vec<Stmt>,
    /// Branch target address -> label name.
    labels: HashMap<u32, String>,
    /// Statement index -> the index whose address is its branch target.
    by_addr: HashMap<u32, usize>,
    /// Target that starts no statement -> the statement it snaps to.
    snap: HashMap<u32, Option<usize>>,
    /// Statement index -> the back edge that closes its loop.
    loophead: HashMap<usize, usize>,
    /// Labels already used by a recovered block.
    consumed_labels: HashSet<u32>,
    /// Undecoded regions.
    pub gaps: usize,
    /// Per-run counters.
    pub stats: Stats,
    /// Text of the value currently live in EAX.
    eax: Option<String>,
}

/// The index one past the statement at `k`.
struct EndOf<'a> {
    prog: &'a Prog,
    ins: &'a [Ins],
}

impl EndOf<'_> {
    fn at(&self, k: usize) -> u32 {
        match self.ins.get(k + 1) {
            Some(next) => next.addr,
            None => self.prog.mcbsize,
        }
    }
}

impl<'a> Dec<'a> {
    /// Wrap a parsed program.
    pub fn new(prog: &'a Prog) -> Self {
        Self {
            prog,
            ins: &prog.ins,
            n: prog.ins.len(),
            varnames: HashMap::new(),
            statements: Vec::new(),
            labels: HashMap::new(),
            by_addr: HashMap::new(),
            snap: HashMap::new(),
            loophead: HashMap::new(),
            consumed_labels: HashSet::new(),
            gaps: 0,
            stats: Stats::default(),
            eax: None,
        }
    }

    // ------------------------------------------------------------- utilities

    /// Name a variable-space slot.
    fn varname(&mut self, off: u32) -> String {
        if let Some((_, n)) = RESERVED.iter().find(|(o, _)| *o == off) {
            return (*n).to_string();
        }
        if let Some(v) = self.varnames.get(&off) {
            return v.clone();
        }
        let v = format!("var${off}");
        self.varnames.insert(off, v.clone());
        v
    }

    /// Name an EBP-relative slot. `CParserUserFunction::ActOnLocalVars` puts
    /// incoming parameters at +4+4*n and locals/temps at negative offsets.
    fn ebpname(d: i64) -> String {
        // The displacement is a DWORD: anything with the high bit set is a
        // negative frame offset.
        let d = if d > 0x7FFF_FFFF {
            d - 0x1_0000_0000
        } else {
            d
        };
        if d >= 8 {
            return format!("par{}", (d - 8) / 4 + 1);
        }
        match d {
            4 => "_RETDATA_".to_string(),
            0 => "_SAVEEBP_".to_string(),
            _ => format!("loc{d:+}"),
        }
    }

    fn funcname(addr: u32) -> String {
        format!("FUNC_{:X}", addr)
    }

    /// Interpret a 32-bit operand as a signed displacement.
    fn sd(v: i64) -> i64 {
        if v > 0x7FFF_FFFF {
            v - 0x1_0000_0000
        } else {
            v
        }
    }

    /// The immediate value of instruction `k`, when it has one.
    fn imm(&self, k: usize) -> Option<i64> {
        let x = self.ins.get(k)?;
        match x.reference {
            None => x.raw.map(|r| r as i64),
            Some(r) if r.kind == ref_type::IMMEDIATE => Some(r.index as i64),
            _ => None,
        }
    }

    fn is(&self, k: usize, name: &str) -> bool {
        self.ins.get(k).is_some_and(|x| x.name == name)
    }

    fn name_at(&self, k: usize) -> &str {
        self.ins.get(k).map_or("", |x| x.name)
    }

    fn ref_is(&self, k: usize, kind: u32, index: u32) -> bool {
        self.ins.get(k).and_then(|x| x.reference) == Some(Ref { kind, index })
    }

    /// A reference of the given kind at `k`, whatever its index.
    fn ref_kind(&self, k: usize, kind: u32) -> Option<u32> {
        self.ins
            .get(k)?
            .reference
            .filter(|r| r.kind == kind)
            .map(|r| r.index)
    }

    fn endof(&self, k: usize) -> u32 {
        EndOf {
            prog: self.prog,
            ins: self.ins,
        }
        .at(k)
    }

    /// Where instruction `k` branches to.
    fn jump_target(&self, k: usize) -> Option<u32> {
        let x = self.ins.get(k)?;
        match x.reference {
            Some(r) if r.kind == ref_type::LABEL => Some(r.index),
            Some(r) if r.kind == ref_type::IMMEDIATE => {
                Some((x.addr + x.size).wrapping_add(r.index))
            }
            _ => None,
        }
    }

    // ----------------------------------------------------------- expressions

    /// `... -> EAX`.
    fn load(&mut self, k: usize) -> Option<Expr> {
        let x = self.ins.get(k)?;
        let nm = x.name;
        if matches!(nm, "MOV EAX IMM1" | "MOV EAX IMM2" | "MOV EAX IMM4") {
            let Some(r) = x.reference else {
                return Some((format!("{}", self.imm(k)?), k + 1));
            };
            return Some(match r.kind {
                ref_type::STRING => (
                    format!(
                        "\"{}\"",
                        self.prog.strings[r.index as usize].replace('"', "\"\"")
                    ),
                    k + 1,
                ),
                ref_type::VARIABLE => (self.varname(r.index), k + 1),
                ref_type::DATA => (format!("data${}", r.index), k + 1),
                ref_type::DLL_CALL => (format!("cmd${}", r.index), k + 1),
                _ => (format!("{}", r.index), k + 1),
            });
        }
        if matches!(nm, "MOV EAX MEM1" | "MOV EAX MEM2" | "MOV EAX MEM4")
            && let Some(i) = self.ref_kind(k, ref_type::VARIABLE)
        {
            return Some((self.varname(i), k + 1));
        }
        if matches!(
            nm,
            "MOV EAX1 [EBP+A]" | "MOV EAX2 [EBP+A]" | "MOV EAX4 [EBP+A]"
        ) && let Some(d) = self.imm(k)
        {
            return Some((Self::ebpname(d), k + 1));
        }
        if nm == "FLD ST08 [MEM]"
            && let Some(i) = self.ref_kind(k, ref_type::VARIABLE)
        {
            return Some((format!("{}#", self.varname(i)), k + 1));
        }
        if nm == "FLD ST08 [EBP+A]"
            && let Some(d) = self.imm(k)
        {
            return Some((format!("{}#", Self::ebpname(d)), k + 1));
        }
        // A 64-bit value is loaded as its low half then its high half.
        if nm == "MOV EDX IMM4"
            && x.reference.is_none()
            && let Some(d) = self.imm(k)
            && self.is(k + 1, "MOV EAX IMM4")
            && self.ins[k + 1].reference.is_none()
            && let Some(lo) = self.imm(k + 1)
        {
            return Some((format!("{{{lo},{d}}}"), k + 2));
        }
        if nm == "MOV ECX IMM4"
            && let Some(i) = self.ref_kind(k, ref_type::VARIABLE)
        {
            let base = self.varname(i);
            if matches!(
                self.name_at(k + 1),
                "MOV EAX1 [ECX+A]" | "MOV EAX2 [ECX+A]" | "MOV EAX4 [ECX+A]"
            ) && let Some(o) = self.imm(k + 1)
            {
                return Some((format!("{base}.{o}"), k + 2));
            }
        }
        if nm == "MOV EBX MEM4"
            && let Some(i) = self.ref_kind(k, ref_type::VARIABLE)
        {
            return Some((self.varname(i), k + 1));
        }
        if nm == "MOV EAX MEM4" && self.ref_kind(k, ref_type::VARIABLE).is_some() {
            return self.array_tail(k);
        }
        None
    }

    /// The `SIB`-indexed element load that ends an array read.
    fn array_tail(&mut self, k: usize) -> Option<Expr> {
        let arr = self.varname(self.ins[k].reference?.index);
        let mut j = k + 1;
        if self.is(j, "CMP EAX4") && self.imm(j) == Some(0) && self.is(j + 1, "JE") {
            j += 2;
        }
        let idx;
        if matches!(self.name_at(j), "MOV EBX IMM1" | "MOV EBX IMM4")
            && self.ins[j].reference.is_none()
        {
            idx = self.imm(j)?.to_string();
            j += 1;
        } else if self.name_at(j) == "MOV EBX MEM4"
            && let Some(i) = self.ref_kind(j, ref_type::VARIABLE)
        {
            idx = self.varname(i);
            j += 1;
        } else {
            return None;
        }
        if self.is(j, "MOV EAX SIB[EAX:EBX*4]") {
            return Some((format!("{arr}({idx})"), j + 1));
        }
        None
    }

    /// A full expression whose result ends up in EAX.
    fn expr(&mut self, k: usize) -> Option<Expr> {
        let v = self.load(k)?;
        self.expr_tail(v.1, v.0)
    }

    /// Fold the arithmetic that follows a loaded value.
    fn expr_tail(&mut self, mut k: usize, mut text: String) -> Option<Expr> {
        while k < self.n {
            let nm = self.name_at(k);
            let imm = || self.imm(k).map(|v| v.to_string());
            if matches!(nm, "ADD EAX IMM1" | "ADD EAX IMM2" | "ADD EAX IMM4") {
                text.push_str(&format!(" + {}", imm()?));
            } else if matches!(nm, "SUB EAX IMM1" | "SUB EAX IMM2" | "SUB EAX IMM4") {
                text.push_str(&format!(" - {}", imm()?));
            } else if matches!(nm, "AND EAX IMM1" | "AND EAX IMM2" | "AND EAX IMM4") {
                text.push_str(&format!(" AND {}", imm()?));
            } else if matches!(nm, "OR EAX IMM1" | "OR EAX IMM2" | "OR EAX IMM4") {
                text.push_str(&format!(" OR {}", imm()?));
            } else if matches!(nm, "XOR EAX IMM1" | "XOR EAX IMM2" | "XOR EAX IMM4") {
                text.push_str(&format!(" XOR {}", imm()?));
            } else if matches!(nm, "SHL EAX1 IMM1" | "SHL EAX4 IMM4") {
                text.push_str(&format!(" << {}", imm()?));
            } else if matches!(nm, "SHR EAX1 IMM1" | "SHR EAX4 IMM4") {
                text.push_str(&format!(" >> {}", imm()?));
            } else if matches!(nm, "NOT EAX1" | "NOT EAX2" | "NOT EAX4") {
                text = format!("NOT {text}");
            } else if nm == "MOV EAX EDX" {
                text.push('#'); // low/high dword pair completed
            } else {
                break;
            }
            k += 1;
        }
        Some((text, k))
    }

    // ------------------------------------------------------------ operators

    /// The 30-byte DIV/MOD safety block; returns the index after it.
    fn divblock(&self, k: usize) -> Option<usize> {
        // CMP EBX,0 / JNE +15 / MOV [@ERR],119 / JMP +3 / CDQ / IDIV / [MOV EAX,EDX]
        let mut j = k;
        if !self.is(j, "CMP EBX4") {
            return None;
        }
        j += 1;
        if !self.is(j, "JNE") {
            return None;
        }
        j += 1;
        if !(self.is(j, "MOV MEM IMM4") && self.ref_is(j, ref_type::VARIABLE, 4)) {
            return None;
        }
        j += 1;
        if !self.is(j, "JMP") {
            return None;
        }
        j += 1;
        if !self.is(j, "CDQ") {
            return None;
        }
        j += 1;
        if !self.name_at(j).starts_with("IDIV EAX EBX") {
            return None;
        }
        j += 1;
        if self.name_at(j).starts_with("MOV EAX EDX") {
            j += 1;
        }
        Some(j)
    }

    /// A hard-coded binary operator (`ASMWriter.cpp:4071-4217`).
    ///
    /// ```text
    /// <right>  PUSH EAX
    /// <left>   POP EBX          ; EBX = right, EAX = left
    /// <op>                     (EAX op EBX, i.e. left op right)
    /// ```
    ///
    /// `MathOp::WriteDBMBit` passes the LEFT operand as P1 and the RIGHT as P2,
    /// and `WriteASMTaskCore` emits P2 first, so the first load is the RIGHT
    /// operand.
    fn binop(&mut self, k: usize) -> Option<Expr> {
        let (right_text, after_right) = self.expr(k)?;
        if !self.is(after_right, "PUSH EAX") {
            return None;
        }
        let (left_text, after_left) = self.expr(after_right + 1)?;
        let mut j = after_left;
        if !self.is(j, "POP EBX") {
            return None;
        }
        j += 1;
        let nm = self.name_at(j);
        let op = match nm {
            "ADD EAX EBX1" | "ADD EAX EBX2" | "ADD EAX EBX4" => Some("+"),
            "SUB EAX EBX1" | "SUB EAX EBX2" | "SUB EAX EBX4" => Some("-"),
            "AND EAX EBX1" | "AND EAX EBX2" | "AND EAX EBX4" => Some("AND"),
            "OR EAX EBX1" | "OR EAX EBX2" | "OR EAX EBX4" => Some("OR"),
            "XOR EAX EBX1" | "XOR EAX EBX2" | "XOR EAX EBX4" => Some("XOR"),
            _ => None,
        };
        if let Some(op) = op {
            return Some((format!("{left_text} {op} {right_text}"), j + 1));
        }
        if matches!(nm, "IMUL EAX EBX1" | "IMUL EAX EBX2" | "IMUL EAX EBX4") {
            return Some((format!("{left_text} * {right_text}"), j + 1));
        }
        if matches!(nm, "MOV ECX EBX1" | "MOV ECX EBX2" | "MOV ECX EBX4") {
            j += 1;
            let nm2 = self.name_at(j);
            if nm2.starts_with("SHL EAX") {
                return Some((format!("{left_text} << {right_text}"), j + 1));
            }
            if nm2.starts_with("SHR EAX") {
                return Some((format!("{left_text} >> {right_text}"), j + 1));
            }
            return None;
        }
        if nm.starts_with("IDIV EAX EBX") {
            let j2 = self.divblock(j)?;
            return Some((format!("{left_text} / {right_text}"), j2));
        }
        None
    }

    /// A hard-coded comparison (`ASMWriter.cpp:4235-4297`).
    ///
    /// ```text
    /// <right>  PUSH EAX
    /// <left>   MOV EBX,EAX
    ///          POP EDX          ; EDX = right
    ///          MOV EAX,0        ; SETcc only writes AL
    ///          CMP EBX,EDX      ; 3B DA -> left - right
    ///          SETcc EAX
    /// ```
    fn compare(&mut self, k: usize) -> Option<Expr> {
        let (right_text, after_right) = self.expr(k)?;
        if !self.is(after_right, "PUSH EAX") {
            return None;
        }
        let (left_text, after_left) = self.expr(after_right + 1)?;
        let mut j = after_left;
        if !self.is(j, "MOV EBX EAX4") {
            return None;
        }
        j += 1;
        if !self.is(j, "POP EDX") {
            return None;
        }
        j += 1;
        if !(self.is(j, "MOV EAX IMM4") && self.imm(j) == Some(0)) {
            return None;
        }
        j += 1;
        if !self.name_at(j).starts_with("CMP EDX EBX") {
            return None;
        }
        j += 1;
        let op = CMPOPS
            .iter()
            .find(|(n, _)| *n == self.name_at(j))
            .map(|(_, o)| *o)?;
        Some((format!("{left_text} {op} {right_text}"), j + 1))
    }

    /// `PMODE_STACK` for a type-8 double (`ASMWriter.cpp:3241-3252`).
    fn push_double(&self, k: usize) -> Option<usize> {
        let ok = self.is(k, "FSTP [MEM] ST08")
            && self.ref_is(k, ref_type::VARIABLE, 20)
            && self.is(k + 1, "MOV EAX MEM4")
            && self.ref_is(k + 1, ref_type::VARIABLE, 24)
            && self.is(k + 2, "PUSH EAX")
            && self.is(k + 3, "MOV EAX MEM4")
            && self.ref_is(k + 3, ref_type::VARIABLE, 20)
            && self.is(k + 4, "PUSH EAX");
        ok.then_some(k + 5)
    }

    /// Parse a run of argument pushes.
    ///
    /// `eax` names the value a leading bare `PUSH EAX` passes; without it a
    /// bare push cannot be named and ends the run.
    fn pushes(&mut self, k: usize, eax: Option<String>) -> Option<(Vec<String>, usize)> {
        let mut j = k;
        let mut args: Vec<String> = Vec::new();
        while j < self.n {
            match self.expr(j) {
                Some(e) if self.is(e.1, "PUSH EAX") => {
                    args.push(e.0);
                    j = e.1 + 1;
                }
                // an 8-byte value already in ST0 pushes as two dwords
                Some(e) if e.0.ends_with('#') => match self.push_double(e.1) {
                    Some(pd) => {
                        args.push(e.0);
                        j = pd;
                    }
                    None => break,
                },
                Some(_) => break,
                None => {
                    if let Some(v) = &eax
                        && self.is(j, "PUSH EAX")
                    {
                        args.push(v.clone());
                        j += 1;
                        continue;
                    }
                    break;
                }
            }
        }
        (!args.is_empty()).then_some((args, j))
    }

    // ------------------------------------------------------------ statements

    fn push_stmt(&mut self, s: Stmt) {
        self.statements.push(s);
    }

    /// Decode starting at instruction `k`, returning the next index.
    fn step(&mut self, k: usize) -> usize {
        let ins = &self.ins[k];
        // The runtime-error hook marks the start of a BASIC statement.
        if ins.name == "MOV MEM IMM4"
            && self.ref_is(k, ref_type::VARIABLE, 16)
            && ins
                .reference2
                .is_some_and(|r| r.kind == ref_type::IMMEDIATE)
        {
            let line = ins.reference2.unwrap().index;
            let mut s = Stmt::new(ins.addr, Kind::Line);
            s.line = Some(line);
            let mut j = k + 1;
            if self.is(j, "MOV EAX MEM4")
                && self.ref_is(j, ref_type::VARIABLE, 4)
                && self.is(j + 1, "CMP EAX4")
                && self.imm(j + 1) == Some(0)
                && self.is(j + 2, "JNE")
            {
                j += 3;
            }
            s.end = self.endof(j);
            self.push_stmt(s);
            return j;
        }

        match self.try_all(k) {
            Some(j) => j,
            None => self.st_gap(k),
        }
    }

    /// Every recogniser except [`Dec::st_gap`], in priority order.
    fn try_all(&mut self, k: usize) -> Option<usize> {
        self.st_frame(k)
            .or_else(|| self.st_call(k))
            .or_else(|| self.st_array(k))
            .or_else(|| self.st_arrayload(k))
            .or_else(|| self.st_binop(k))
            .or_else(|| self.st_immediate_op(k))
            .or_else(|| self.st_assign(k))
            .or_else(|| self.st_condition(k))
            .or_else(|| self.st_funcall(k))
            .or_else(|| self.st_jump(k))
            .or_else(|| self.st_misc(k))
            .or_else(|| self.st_arrayerr(k))
    }

    /// Record `[lo, hi)` as an undecoded region.
    fn add_gap(&mut self, lo: usize, hi: usize) {
        if hi <= lo {
            return;
        }
        let mut s = Stmt::new(self.ins[lo].addr, Kind::Gap);
        s.end = if hi < self.n {
            self.ins[hi].addr
        } else {
            self.prog.mcbsize
        };
        self.push_stmt(s);
    }

    /// Re-run the recognisers inside each gap and split it where one fires.
    ///
    /// A gap only records that no recogniser claimed the run *from where the
    /// walker was standing*. A compiler preamble -- saving a value, stashing
    /// it, loading a table -- routinely sits in front of a shape that is
    /// perfectly recognisable and stops the first pass from ever reaching it.
    /// Walking the gap instruction by instruction and keeping the undecodable
    /// bytes as their own (smaller) gaps recovers those statements without
    /// dropping a single instruction from the transcription.
    fn refine_gaps(&mut self) {
        let idx: HashMap<u32, usize> = self
            .ins
            .iter()
            .enumerate()
            .map(|(i, x)| (x.addr, i))
            .collect();
        let outer = std::mem::take(&mut self.statements);
        let mut out = Vec::with_capacity(outer.len());
        for s in outer {
            if s.kind != Kind::Gap {
                out.push(s);
                continue;
            }
            let Some(lo) = idx.get(&s.addr).copied() else {
                out.push(s);
                continue;
            };
            let hi = idx.get(&s.end).copied().unwrap_or(self.n);
            if hi <= lo {
                out.push(s);
                continue;
            }
            let mut run_start: Option<usize> = None;
            let mut k = lo;
            while k < hi {
                match self.try_all(k) {
                    Some(j) if k < j && j <= hi => {
                        if let Some(r) = run_start.take() {
                            self.add_gap(r, k);
                        }
                        k = j;
                    }
                    _ => {
                        run_start = Some(run_start.unwrap_or(k));
                        k += 1;
                    }
                }
            }
            if let Some(r) = run_start {
                self.add_gap(r, hi);
            }
            out.append(&mut self.statements);
        }
        self.statements = out;
        self.gaps = self
            .statements
            .iter()
            .filter(|s| s.kind == Kind::Gap)
            .count();
    }

    /// The keyword for the command `index` calls.
    fn command_name(&self, index: u32) -> String {
        let sym = self
            .prog
            .cmd_name
            .get(index as usize)
            .map_or("", String::as_str);
        let stem = sym.trim_start_matches('?').split('@').next().unwrap_or("");
        keyword(stem)
    }

    /// The `MOV EBX, <cmd>` / `CALL EBX` pair, when `j` sits on it.
    fn call_at(&self, j: usize) -> Option<u32> {
        let idx = self.ref_kind(j, ref_type::DLL_CALL)?;
        (self.is(j, "MOV EBX IMM4") && self.is(j + 1, "CALL EBX")).then_some(idx)
    }

    /// A library command call used as a statement.
    fn st_call(&mut self, k: usize) -> Option<usize> {
        let eax = self.eax.clone();
        let (mut args, mut j) = match self.pushes(k, eax) {
            Some(pr) => pr,
            None => (Vec::new(), k), // zero-argument command
        };
        let index = self.call_at(j)?;

        let name = self.command_name(index);
        self.stats.cmd += 1;
        j += 2;

        // Statement-level calls store the result before the argument pops
        // (`ParseInstruction`); expression-level calls (`MathOp::WriteDBMBit`)
        // pop first and store afterwards. Accept either order.
        let mut ret = None;
        if let Some((tgt, next)) = self.store(j) {
            ret = Some(tgt);
            j = next;
        }
        while self.is(j, "POP EBX") || self.is(j, "POP EAX") {
            j += 1;
        }
        if ret.is_none()
            && let Some((tgt, next)) = self.store(j)
        {
            ret = Some(tgt);
            j = next;
        }
        while self.is(j, "POP EBX") || self.is(j, "POP EAX") {
            j += 1;
        }

        args.reverse();
        let call = format!("{name}({})", args.join(", "));
        let mut s = Stmt::new(self.ins[k].addr, Kind::Call);
        s.text = Some(name);
        s.extra = Some((args, ret.clone()));
        s.end = self.endof(j);
        self.push_stmt(s);
        self.eax = ret.is_none().then_some(call);
        Some(j)
    }

    /// A library command call used as a value rather than as a statement.
    fn cmd_value(&mut self, k: usize) -> Option<Expr> {
        let eax = self.eax.clone();
        let (mut args, j) = match self.pushes(k, eax) {
            Some(pr) => pr,
            None => (Vec::new(), k),
        };
        let index = self.call_at(j)?;
        let name = self.command_name(index);
        args.reverse();
        Some((format!("{name}({})", args.join(", ")), j + 2))
    }

    /// The program preamble (`BUILD_STARTPROGRAM`) and epilogue.
    fn st_frame(&mut self, k: usize) -> Option<usize> {
        let nm = self.name_at(k);
        if nm == "PUSH REGISTERS" && self.is(k + 1, "MOV MEM4 ESP") {
            let mut j = k + 2;
            if self.call_at(j).is_some() {
                j += 2;
            }
            let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
            s.text = Some("' [program start: PUSHAD, ESP saved]".to_string());
            s.end = self.endof(j);
            self.push_stmt(s);
            return Some(j);
        }
        if nm == "POP REGISTERS" || nm == "MOV ESP MEM4" {
            let text = if nm == "POP REGISTERS" {
                "' [program end: POPAD]"
            } else {
                "' [restore ESP]"
            };
            let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
            s.text = Some(text.to_string());
            s.end = self.endof(k);
            self.push_stmt(s);
            return Some(k + 1);
        }
        None
    }

    /// Recognise `EAX/ST0 -> destination`.
    fn store(&mut self, k: usize) -> Option<(String, usize)> {
        let x = self.ins.get(k)?;
        let nm = x.name;
        if matches!(nm, "MOV MEM EAX1" | "MOV MEM EAX2" | "MOV MEM EAX4")
            && let Some(i) = self.ref_kind(k, ref_type::VARIABLE)
        {
            return Some((self.varname(i), k + 1));
        }
        if matches!(
            nm,
            "MOV [EBP+A] EAX1"
                | "MOV [EBP+A] EAX2"
                | "MOV [EBP+A] EAX4"
                | "MOV [ECX+A] EAX1"
                | "MOV [ECX+A] EAX2"
                | "MOV [ECX+A] EAX4"
        ) && let Some(d) = self.imm(k)
        {
            // The mnemonic names the register itself: EBP-relative slots are
            // the function frame, ECX-relative ones the caller's array base.
            return Some((
                if nm.contains("EBP") {
                    Self::ebpname(d)
                } else {
                    format!("[ECX{:+}]", Self::sd(d))
                },
                k + 1,
            ));
        }
        if matches!(
            nm,
            "MOV MEM ST08"
                | "FSTP [MEM] ST08"
                | "FSTP [EBP+A] ST08"
                | "FSTP [EAX+A] ST08"
                | "FSTP [ECX+A] ST08"
        ) && (self.ref_kind(k, ref_type::VARIABLE).is_some() || self.imm(k).is_some())
        {
            if let Some(i) = self.ref_kind(k, ref_type::VARIABLE) {
                return Some((self.varname(i), k + 1));
            }
            let d = self.imm(k)?;
            return Some((
                if nm.contains("ECX") {
                    format!("[ECX{:+}]", Self::sd(d))
                } else {
                    Self::ebpname(d)
                },
                k + 1,
            ));
        }
        None
    }

    /// A binary operator or comparison whose result is stored.
    fn st_binop(&mut self, k: usize) -> Option<usize> {
        let r = self.binop(k).or_else(|| self.compare(k))?;
        let (text, j) = r;
        let (tgt, after) = self.store(j)?;
        self.stats.assign += 1;
        let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
        s.text = Some(format!("{tgt} = {text}"));
        s.end = self.endof(after);
        self.push_stmt(s);
        self.eax = Some(text);
        Some(after)
    }

    /// Arithmetic with an immediate right operand (`ASMWriter.cpp:3930-4000`).
    fn st_immediate_op(&mut self, k: usize) -> Option<usize> {
        let (left, j) = self.expr(k)?;
        if !(matches!(self.name_at(j), "MOV EBX IMM1" | "MOV EBX IMM4") && self.imm(j).is_some()) {
            return None;
        }
        let rhs = self.imm(j)?.to_string();
        let j = j + 1;
        let nm = self.name_at(j).to_string();
        if matches!(
            nm.as_str(),
            "IMUL EAX EBX1" | "IMUL EAX EBX2" | "IMUL EAX EBX4"
        ) {
            return self.emit_assign(self.ins[k].addr, &left, "*", &rhs, j + 1);
        }
        if nm == "CMP EBX4" && self.imm(j) == Some(0) {
            let q = self.divblock(j)?;
            let op = if self.name_at(q - 1).starts_with("MOV EAX EDX") {
                "MOD"
            } else {
                "/"
            };
            return self.emit_assign(self.ins[k].addr, &left, op, &rhs, q);
        }
        None
    }

    fn emit_assign(
        &mut self,
        addr: u32,
        left: &str,
        op: &str,
        right: &str,
        j: usize,
    ) -> Option<usize> {
        let (tgt, after) = self.store(j)?;
        let mut s = Stmt::new(addr, Kind::Assign);
        s.text = Some(format!("{tgt} = {left} {op} {right}"));
        s.end = self.endof(after);
        self.push_stmt(s);
        self.stats.assign += 1;
        Some(after)
    }

    /// A simple assignment.
    fn st_assign(&mut self, k: usize) -> Option<usize> {
        let (text, j) = self.expr(k)?;
        let (tgt, after) = self.store(j)?;
        self.stats.assign += 1;
        let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
        s.text = Some(format!("{tgt} = {text}"));
        s.end = self.endof(after);
        self.push_stmt(s);
        self.eax = Some(text);
        Some(after)
    }

    /// `MOV EBX, <index>` at `k`.
    fn array_index(&mut self, k: usize) -> Option<Expr> {
        let nm = self.name_at(k).to_string();
        if matches!(nm.as_str(), "MOV EBX IMM1" | "MOV EBX IMM4") && self.ins[k].reference.is_none()
        {
            return Some((self.imm(k)?.to_string(), k + 1));
        }
        if nm == "MOV EBX MEM4"
            && let Some(i) = self.ref_kind(k, ref_type::VARIABLE)
        {
            return Some((self.varname(i), k + 1));
        }
        if matches!(
            nm.as_str(),
            "MOV EBX1 [EBP+A]" | "MOV EBX2 [EBP+A]" | "MOV EBX4 [EBP+A]"
        ) && let Some(d) = self.imm(k)
        {
            return Some((Self::ebpname(d), k + 1));
        }
        None
    }

    /// `arr(index) = value`, with the compiler's array bounds check
    /// (`WriteASMEAXtoX` / `WriteASMEAXtoARR`).
    fn st_array(&mut self, k: usize) -> Option<usize> {
        let (arr, idx, val, mut j) = if let Some(e) = self.expr(k)
            && self.is(e.1, "PUSH EAX")
        {
            let j = e.1 + 1;
            // A strict prefix: the array pointer load, the existence check
            // and the subscript pop. Without this the loop-condition pattern
            // is misread.
            if !(self.is(j, "MOV EAX MEM4") && self.ref_kind(j, ref_type::VARIABLE).is_some()) {
                return None;
            }
            let arr = self.varname(self.ins[j].reference?.index);
            if !(self.is(j + 1, "CMP EAX4")
                && self.imm(j + 1) == Some(0)
                && self.is(j + 2, "JE")
                && self.is(j + 3, "POP EBX"))
            {
                return None;
            }
            (arr, e.0, None, j + 4)
        } else {
            // The stored value may be a library command's return, which the
            // expression parser cannot see; it is stashed with `MOV ECX,EAX`
            // and written into the element by the `MOV [EAX+A] ECX` below.
            let (v, mut j) = self.cmd_value(k)?;
            if !self.is(j, "MOV ECX EAX") {
                return None;
            }
            j += 1;
            if !(self.is(j, "MOV EAX MEM4") && self.ref_kind(j, ref_type::VARIABLE).is_some()) {
                return None;
            }
            let arr = self.varname(self.ins[j].reference?.index);
            if !(self.is(j + 1, "CMP EAX4") && self.imm(j + 1) == Some(0) && self.is(j + 2, "JE")) {
                return None;
            }
            j += 3;
            let (idx, next) = self.array_index(j)?;
            (arr, idx, Some(v), next)
        };
        let mut arr = Some(arr);
        let idx = Some(idx);
        let mut val = val;

        let limit = self.n.min(j + 36);
        while j < limit {
            let x = &self.ins[j];
            let nm = x.name;
            let var_ref = self.ref_kind(j, ref_type::VARIABLE);
            if matches!(nm, "MOV EAX MEM4" | "MOV ECX MEM4" | "MOV EBX MEM4")
                && nm == "MOV EAX MEM4"
                && let Some(i) = var_ref
            {
                if arr.is_none() {
                    arr = Some(self.varname(i));
                } else if val.is_none() {
                    val = Some(self.varname(i));
                }
            } else if nm == "MOV EAX SIB[EAX:EBX*4]" {
                let (Some(arr), Some(idx), Some(val)) = (arr.clone(), idx.clone(), val.clone())
                else {
                    return None;
                };
                if !self.name_at(j + 1).starts_with("MOV [EAX+A] ECX") {
                    return None;
                }
                let off = self.imm(j + 1).unwrap_or(0);
                let suffix = if off == 0 {
                    String::new()
                } else {
                    format!(".{}", Self::sd(off))
                };
                let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
                s.text = Some(format!("{arr}({idx}){suffix} = {val}"));
                let mut q = j + 2;
                if self.is(q, "JMP") {
                    q += 1;
                }
                s.end = self.endof(q);
                self.push_stmt(s);
                self.stats.assign += 1;
                self.stats.array += 1;
                return Some(q);
            } else if nm == "MOV ECX EAX" {
                // the value is already loaded into EAX
            } else if matches!(nm, "MOV EAX IMM1" | "MOV EAX IMM2" | "MOV EAX IMM4")
                && matches!(
                    x.reference.map(|r| r.kind),
                    None | Some(ref_type::IMMEDIATE)
                )
            {
                val = self.imm(j).map(|v| v.to_string());
            } else if matches!(
                nm,
                "MOV EAX1 [EBP+A]" | "MOV EAX2 [EBP+A]" | "MOV EAX4 [EBP+A]"
            ) && let Some(d) = self.imm(j)
            {
                val = Some(Self::ebpname(d));
            }
            j += 1;
        }
        None
    }

    /// `arr(index).field` -- the read mirror of the store in [`Dec::st_array`].
    ///
    /// A statement commonly saves a value before the read, so the walker is
    /// rarely sitting on the array load itself; scan a short window for the
    /// start of the shape and take the first that parses.
    fn st_arrayload(&mut self, k: usize) -> Option<usize> {
        (k..self.n.min(k + 8)).find_map(|s| self.arrayload_at(s, k))
    }

    /// Parse the array-of-user-types read starting exactly at `k`.
    ///
    /// ```text
    /// MOV EAX, @arr   CMP EAX,0      JE <error>
    /// MOV EBX, <index>
    /// PUSH EDX   MOV EDX,[EAX-16]  CMP EDX,EBX  POP EDX  JGE <error>
    /// CMP EBX,-1  JLE <error>
    /// MOV EAX,[EAX+EBX*4]          -- the element
    /// MOV ECX,[EAX+off]   MOV EAX,ECX
    /// ```
    fn arrayload_at(&mut self, k: usize, start: usize) -> Option<usize> {
        if !(self.is(k, "MOV EAX MEM4") && self.ref_kind(k, ref_type::VARIABLE).is_some()) {
            return None;
        }
        let arr = self.varname(self.ins[k].reference?.index);
        let mut j = k + 1;
        if !(self.is(j, "CMP EAX4") && self.imm(j) == Some(0) && self.is(j + 1, "JE")) {
            return None;
        }
        j += 2;
        let (idx, next) = self.array_index(j)?;
        j = next;
        // The bounds check the compiler wraps around every array subscript:
        // [EAX-16] holds the element count and -1 is the lowest legal index.
        let bounds = self.is(j, "PUSH EDX")
            && self.is(j + 1, "MOV EDX4 [EAX+A]")
            && self.imm(j + 1).is_some_and(|d| Self::sd(d) == -16)
            && self.is(j + 2, "CMP EDX EBX4")
            && self.is(j + 3, "POP EDX")
            && self.is(j + 4, "JGE")
            && self.is(j + 5, "CMP EBX4")
            && self.imm(j + 5).is_some_and(|d| Self::sd(d) == -1)
            && self.is(j + 6, "JLE");
        if bounds {
            j += 7;
        }
        if !self.is(j, "MOV EAX SIB[EAX:EBX*4]") {
            return None;
        }
        j += 1;
        let off;
        if self.is(j, "MOV ECX4 [EAX+A]") {
            off = self.imm(j)?;
            j += 1;
            if !self.is(j, "MOV EAX ECX4") {
                return None;
            }
            j += 1;
        } else if matches!(
            self.name_at(j),
            "MOV EAX1 [EAX+A]" | "MOV EAX2 [EAX+A]" | "MOV EAX4 [EAX+A]"
        ) {
            off = self.imm(j)?;
            j += 1;
        } else {
            return None;
        }
        let suffix = if off == 0 {
            String::new()
        } else {
            format!(".{}", Self::sd(off))
        };
        let tgt = format!("{arr}({idx}){suffix}");
        let mut s = Stmt::new(self.ins[start].addr, Kind::Misc);
        s.text = Some(format!("' {tgt}"));
        s.end = self.endof(j - 1);
        self.push_stmt(s);
        self.eax = Some(tgt);
        self.stats.array += 1;
        Some(j)
    }

    /// The unreachable array-bounds error path (runtime error 118).
    fn st_arrayerr(&mut self, k: usize) -> Option<usize> {
        if !(self.is(k, "MOV MEM IMM4")
            && self.ref_is(k, ref_type::VARIABLE, 4)
            && self.ins[k]
                .reference2
                .is_some_and(|r| r.kind == ref_type::IMMEDIATE))
        {
            return None;
        }
        // Do NOT swallow the `_LINE_` marker that follows: step() turns it
        // into a source-line statement, and eating it would lose which line
        // comes next.
        let mut j = k + 1;
        if self.is(j, "MOV EAX MEM4")
            && self.ref_is(j, ref_type::VARIABLE, 4)
            && self.is(j + 1, "CMP EAX4")
            && self.imm(j + 1) == Some(0)
            && self.is(j + 2, "JNE")
        {
            j += 3;
        }
        let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
        s.text = Some("' [array index out of range -> runtime error 118]".to_string());
        s.end = self.endof(j);
        self.push_stmt(s);
        Some(j)
    }

    /// `<load> [NOT EAX] CMP EAX,0 JE|JNE <target>`.
    fn st_condition(&mut self, k: usize) -> Option<usize> {
        let (cond, mut j) = self.expr(k)?;
        j = self.expr_tail(j, cond.clone())?.1;
        let mut neg = false;
        if matches!(self.name_at(j), "NOT EAX1" | "NOT EAX2" | "NOT EAX4") {
            neg = true;
            j += 1;
        }
        if !(self.is(j, "CMP EAX4") && self.imm(j) == Some(0)) {
            return None;
        }
        j += 1;
        if !(self.is(j, "JE") || self.is(j, "JNE")) {
            return None;
        }
        let cc = self.name_at(j);
        let tgt = self.jump_target(j);
        let mut s = Stmt::new(self.ins[k].addr, Kind::Cond);
        s.cond = Some(cond.clone());
        // JE jumps when the expression is false; JNE jumps when it is true.
        s.cc = Some(if (cc == "JNE") != neg {
            "true"
        } else {
            "false"
        });
        s.target = tgt;
        s.end = self.endof(j);
        self.push_stmt(s);
        self.eax = Some(cond);
        self.stats.cond += 1;
        Some(j + 1)
    }

    /// A user FUNCTION call.
    fn st_funcall(&mut self, k: usize) -> Option<usize> {
        let (mut args, mut j) = self.pushes(k, None)?;
        if !self.is(j, "CALL MEM") {
            return None;
        }
        let t = self.jump_target(j);
        j += 1;
        if self.name_at(j) == "ADD ESP" {
            j += 1;
        }
        while self.is(j, "POP EBX") {
            j += 1;
        }
        let mut ret = None;
        if let Some((tgt, next)) = self.store(j) {
            ret = Some(tgt);
            j = next;
        }
        args.reverse();
        let mut s = Stmt::new(self.ins[k].addr, Kind::Funcall);
        s.text = Some(format!("CALL {}", Self::funcname(t.unwrap_or(0))));
        s.extra = Some((args, ret));
        s.end = self.endof(j);
        self.push_stmt(s);
        self.stats.funcall += 1;
        Some(j)
    }

    /// A bare branch.
    fn st_jump(&mut self, k: usize) -> Option<usize> {
        let nm = self.name_at(k);
        if !matches!(nm, "JMP" | "JNE" | "JE" | "JGE" | "JLE") {
            return None;
        }
        let cc = match nm {
            "JMP" => "JMP",
            "JNE" => "JNE",
            "JE" => "JE",
            "JGE" => "JGE",
            _ => "JLE",
        };
        let t = self.jump_target(k);
        let mut s = Stmt::new(self.ins[k].addr, Kind::Jump);
        s.target = t;
        s.cc = Some(cc);
        s.end = self.endof(k);
        self.push_stmt(s);
        Some(k + 1)
    }

    /// RETURN, a function prologue, and frame teardown.
    fn st_misc(&mut self, k: usize) -> Option<usize> {
        let nm = self.name_at(k).to_string();
        if nm == "RET" {
            let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
            s.text = Some("RETURN".to_string());
            s.end = self.endof(k);
            self.push_stmt(s);
            return Some(k + 1);
        }
        if nm == "PUSH EBP" {
            let mut j = k;
            let mut size = None;
            if self.is(j + 1, "MOV EBP ESP") && self.is(j + 2, "SUB ESP") {
                size = self.imm(j + 2);
                j += 3;
                if self.is(j, "MOV EAX ESP") {
                    j += 1;
                    if self.is(j, "MOV ECX IMM4") {
                        j += 1;
                    }
                    if self.is(j, "MOV SIB[EAX:ECX*4],IMM4") {
                        j += 1;
                    }
                    if self.is(j, "LOOP ECX") {
                        j += 1;
                    }
                }
            }
            let text = match size {
                Some(n) => format!(
                    "FUNCTION {}  (stack frame {} bytes)",
                    Self::funcname(self.ins[k].addr),
                    n
                ),
                None => format!(
                    "FUNCTION {}  (stack frame None bytes)",
                    Self::funcname(self.ins[k].addr)
                ),
            };
            let mut s = Stmt::new(self.ins[k].addr, Kind::Func);
            s.text = Some(text);
            s.target = Some(self.ins[k].addr);
            s.end = self.endof(j);
            self.push_stmt(s);
            self.stats.function += 1;
            return Some(j);
        }
        if nm == "POP EBP" || nm == "MOV ESP EBP" {
            let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
            s.end = self.endof(k);
            self.push_stmt(s);
            return Some(k + 1);
        }
        None
    }

    /// Everything from `k` to the next `_LINE_` marker.
    fn st_gap(&mut self, k: usize) -> usize {
        let mut j = k + 1;
        while j < self.n && !self.ref_is(j, ref_type::VARIABLE, 16) {
            if self.name_at(j) == "MOV MEM IMM4" && self.ref_is(j, ref_type::VARIABLE, 16) {
                break;
            }
            j += 1;
        }
        let mut s = Stmt::new(self.ins[k].addr, Kind::Gap);
        s.end = if j < self.n {
            self.ins[j].addr
        } else {
            self.prog.mcbsize
        };
        self.push_stmt(s);
        self.gaps += 1;
        j
    }

    // ------------------------------------------------------------------- run

    /// Walk the whole machine code block.
    pub fn run(&mut self) -> &[Stmt] {
        let mut k = 0;
        while k < self.n {
            k = self.step(k);
        }
        self.refine_gaps();
        self.build_labels();
        &self.statements
    }

    fn build_labels(&mut self) {
        let targets: BTreeSet<u32> = self
            .statements
            .iter()
            .filter_map(|s| s.target)
            .filter(|t| *t != 0xFFFF_FFFF)
            .collect();
        for t in targets {
            self.labels.insert(t, format!("L{t:05X}"));
        }
    }

    /// The label for `addr`, or the default name for it.
    fn label(&self, addr: Option<u32>) -> String {
        let a = addr.unwrap_or(0);
        self.labels
            .get(&a)
            .cloned()
            .unwrap_or_else(|| format!("L{a:05X}"))
    }

    // -------------------------------------------------------------- rendering

    fn target_index(&self, s: &Stmt) -> Option<usize> {
        let t = s.target?;
        if let Some(i) = self.by_addr.get(&t) {
            return Some(*i);
        }
        // A target that starts no statement is snapped forward to the next
        // one; if there is no next one the branch leaves the block entirely.
        self.snap.get(&t).copied().flatten()
    }

    /// Render every statement, recovering IF/REPEAT/DO/FOR structure.
    pub fn render(&mut self) -> Vec<String> {
        self.by_addr = self
            .statements
            .iter()
            .enumerate()
            .map(|(i, s)| (s.addr, i))
            .collect();

        // A compiler label always starts a statement, but a recovered
        // statement may begin later if a recogniser consumed a prefix. Snap
        // such targets to the next statement start so the control flow can
        // still be shaped.
        let mut addrs: Vec<u32> = self.by_addr.keys().copied().collect();
        addrs.sort_unstable();
        for s in &self.statements {
            let Some(t) = s.target else { continue };
            if self.by_addr.contains_key(&t) {
                continue;
            }
            let j = addrs.partition_point(|a| *a < t);
            self.snap
                .insert(t, addrs.get(j).and_then(|a| self.by_addr.get(a)).copied());
        }

        // Loop heads: for every backward JMP, map its target to the nearest
        // back edge.
        self.loophead.clear();
        for j in 0..self.statements.len() {
            let t = &self.statements[j];
            if t.kind == Kind::Jump
                && t.cc == Some("JMP")
                && let Some(ti) = self.target_index(t)
                && ti <= j
            {
                let slot = self.loophead.entry(ti).or_insert(usize::MAX);
                *slot = (*slot).min(j);
            }
        }

        self.consumed_labels.clear();
        let mut out = Vec::with_capacity(self.statements.len() + 16);
        self.block(0, self.statements.len(), 0, &mut out);
        out
    }

    fn ind(depth: usize) -> String {
        "  ".repeat(depth)
    }

    /// Render statements `[lo, hi)`, recovering loop and IF structure.
    fn block(&mut self, lo: usize, hi: usize, depth: usize, out: &mut Vec<String>) {
        let mut i = lo;
        while i < hi {
            // ---- loop: detect at its head, not at the back edge
            if let Some(bj) = self.loophead.get(&i).copied()
                && bj < hi
            {
                if self.for_loop(i, bj, depth, out) {
                    i = bj + 1;
                    continue;
                }
                let mut cond = None;
                let mut start = bj;
                if bj - 1 > i
                    && self.statements[bj - 1].kind == Kind::Cond
                    && self.statements[bj - 1].target == self.statements[bj].target
                {
                    cond = Some(self.statements[bj - 1].clone());
                    start = bj - 1;
                }
                if let Some(t) = self.statements[bj].target {
                    self.consumed_labels.insert(t);
                }
                match cond {
                    Some(c) => {
                        out.push(format!("{}REPEAT", Self::ind(depth)));
                        self.block(i, start, depth + 1, out);
                        out.push(format!(
                            "{}UNTIL {} = 0",
                            Self::ind(depth),
                            c.cond.as_deref().unwrap_or("")
                        ));
                    }
                    None => {
                        out.push(format!("{}DO", Self::ind(depth)));
                        self.block(i, start, depth + 1, out);
                        out.push(format!("{}LOOP", Self::ind(depth)));
                    }
                }
                i = bj + 1;
                continue;
            }

            // ---- forward conditional = IF block
            let s = self.statements[i].clone();
            let tgt = self.target_index(&s);
            if s.kind == Kind::Cond
                && let Some(tgt) = tgt
                && i < tgt
                && tgt <= hi
                && self.plain_block(i + 1, tgt)
            {
                if let Some(t) = s.target {
                    self.consumed_labels.insert(t);
                }
                let test = s.cond.clone().unwrap_or_default();
                let test = if s.cc == Some("false") {
                    test
                } else {
                    format!("NOT {test}")
                };
                out.push(format!("{}IF {test} <> 0 THEN", Self::ind(depth)));
                self.block(i + 1, tgt, depth + 1, out);
                out.push(format!("{}ENDIF", Self::ind(depth)));
                i = tgt;
                continue;
            }

            // ---- user function body
            if s.kind == Kind::Func {
                self.consumed_labels.insert(s.addr);
                if let Some(t) = &s.text {
                    out.push(format!("{}{}", Self::ind(depth), t));
                }
                let mut j = i + 1;
                while j < hi {
                    let f = &self.statements[j];
                    if f.kind == Kind::Func && self.labels.contains_key(&f.addr) {
                        break;
                    }
                    j += 1;
                }
                self.block(i + 1, j, depth, out);
                out.push(format!("{}ENDFUNCTION", Self::ind(depth)));
                i = j;
                continue;
            }

            if let Some(label) = self.labels.get(&s.addr)
                && !self.consumed_labels.contains(&s.addr)
            {
                out.push(format!("{}{}:", Self::ind(depth), label));
            }
            self.emit(&s, out, depth);
            i += 1;
        }
    }

    /// True when `[lo, hi)` is a single IF body: no branch leaves it except a
    /// trailing `JMP <endif>`, which is the end of a single-line IF...THEN.
    fn plain_block(&self, lo: usize, hi: usize) -> bool {
        for t in &self.statements[lo..hi] {
            if t.target.is_none() {
                continue;
            }
            let Some(ti) = self.target_index(t) else {
                return false;
            };
            if lo <= ti && ti < hi {
                continue; // the branch stays inside the body
            }
            if t.kind == Kind::Jump && t.cc == Some("JMP") && ti >= hi {
                continue; // it jumps past the ENDIF
            }
            return false;
        }
        true
    }

    /// Recognise FOR/NEXT.
    ///
    /// ```text
    /// v = a - s                       (initialiser, before the loop)
    /// L:  v = v + s                    (the loop label points here)
    ///     <tmp> = v <= b
    ///     IF <tmp> <> 0 THEN GOTO exit
    ///     ...body...
    ///     JMP L
    /// ```
    fn for_loop(&mut self, tgt: usize, bj: usize, depth: usize, out: &mut Vec<String>) -> bool {
        let c = (tgt..bj).find(|j| self.statements[*j].kind == Kind::Cond);
        let Some(c) = c else { return false };
        if c < tgt + 1 {
            return false;
        }
        let cond = self.statements[c].clone();
        let test = self.statements[c - 1].clone();
        if test.kind != Kind::Assign || cond.target.is_none() {
            return false;
        }
        let (var, op, limit) = match split_comparison(test.text.as_deref().unwrap_or("")) {
            Some(v) => v,
            None => return false,
        };

        let mut inc: Option<String> = None;
        for st in self.statements[tgt..c.saturating_sub(1)].iter() {
            if st.kind == Kind::Assign
                && let Some(text) = &st.text
                && let Some(rest) = text.strip_prefix(&format!("{var} = {var} + "))
            {
                inc = Some(rest.trim().to_string());
            }
        }
        let Some(inc) = inc else { return false };
        if inc.is_empty() || !inc.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }

        let mut init = None;
        let mut j = tgt as isize - 1;
        while j >= 0 && self.statements[j as usize].kind == Kind::Line {
            j -= 1;
        }
        if j >= 0 {
            let st = &self.statements[j as usize];
            if st.kind == Kind::Assign
                && let Some((lhs, rhs)) = st.text.as_deref().and_then(split_seed)
                && lhs == var
                && let Some(head) = rhs.strip_suffix(&format!(" - {inc}"))
            {
                init = Some(head.to_string());
            }
        }
        let Some(init) = init else { return false };

        if let Some(t) = cond.target {
            self.consumed_labels.insert(t);
        }
        let step = if inc == "1" {
            String::new()
        } else {
            format!(" STEP {inc}")
        };
        if op == "<=" {
            out.push(format!(
                "{}FOR {var} = {init} TO {limit}{step}",
                Self::ind(depth)
            ));
        } else {
            out.push(format!(
                "{}FOR {var} = {init} DOWNTO {limit}{step}",
                Self::ind(depth)
            ));
        }
        self.block(c + 1, bj, depth + 1, out);
        out.push(format!("{}NEXT {var}", Self::ind(depth)));
        self.stats.for_ += 1;
        true
    }

    /// Render one statement.
    fn emit(&self, s: &Stmt, out: &mut Vec<String>, depth: usize) {
        let ind = Self::ind(depth);
        match s.kind {
            Kind::Line => out.push(format!(
                "{ind}' ==== source line {} ====",
                s.line.unwrap_or(0)
            )),
            Kind::Call | Kind::Funcall => {
                let args = s
                    .extra
                    .as_ref()
                    .map(|(a, _)| a.join(", "))
                    .unwrap_or_default();
                let call = format!("{}({args})", s.text.as_deref().unwrap_or(""));
                match s.extra.as_ref().and_then(|(_, r)| r.clone()) {
                    Some(ret) => out.push(format!("{ind}{ret} = {call}")),
                    None => out.push(format!("{ind}{call}")),
                }
            }
            Kind::Assign => {
                if let Some(t) = &s.text {
                    out.push(format!("{ind}{t}"));
                }
            }
            Kind::Cond => {
                let lbl = self.label(s.target);
                let cond = s.cond.as_deref().unwrap_or("");
                if s.cc == Some("false") {
                    out.push(format!("IF {cond} <> 0 THEN GOTO {lbl}"));
                } else {
                    out.push(format!("IF {cond} = 0 THEN GOTO {lbl}"));
                }
            }
            Kind::Jump => {
                if s.target == Some(0xFFFF_FFFF) {
                    out.push(format!("{ind}END"));
                } else {
                    out.push(format!("{ind}GOTO {}", self.label(s.target)));
                }
            }
            Kind::Func => {
                if let Some(t) = &s.text {
                    out.push(format!("{ind}{t}"));
                }
            }
            Kind::Misc => {
                if let Some(t) = &s.text {
                    out.push(format!("{ind}{t}"));
                }
            }
            Kind::Gap => {
                out.push(format!(
                    "' <<< {} bytes not recognised at {:06X} >>>",
                    s.end - s.addr,
                    s.addr
                ));
                for x in self.ins {
                    if s.addr <= x.addr && x.addr < s.end {
                        let o = match x.reference {
                            Some(r) => match r.kind {
                                ref_type::DLL_CALL => format!("cmd{}", r.index),
                                ref_type::STRING => format!("\"{}\"", r.index),
                                ref_type::VARIABLE => format!("@{}", r.index),
                                ref_type::IMMEDIATE => format!("{}", r.index),
                                ref_type::LABEL => format!("L{:05X}", r.index),
                                ref_type::DATA => format!("data{}", r.index),
                                _ => "?".to_string(),
                            },
                            None => x.raw.map_or(String::new(), |v| v.to_string()),
                        };
                        out.push(format!("'   {:06X}  {:<24} {}", x.addr, x.name, o));
                    }
                }
            }
        }
    }
}

/// Split `x <= y` / `x >= y` at the operator, keeping both sides verbatim.
/// Split `lhs = rhs` on the LAST ` = `, which is where a greedy left group
/// lands: `^(.+) = (.+) - <inc>$`.
fn split_seed(text: &str) -> Option<(&str, &str)> {
    let i = text.rfind(" = ")?;
    Some((text[..i].trim_end(), text[i + 3..].trim_start()))
}

fn split_comparison(text: &str) -> Option<(String, String, String)> {
    // `^\S+ = (.+) (<=|>=) (.+)$`: the leading token is the compiler's
    // temporary, so the loop variable is the comparison's LEFT operand -- the
    // greedy middle group takes the last operator that leaves text on both
    // sides.
    let head = text.split_whitespace().next()?;
    let after = text[head.len()..].strip_prefix(" = ")?;
    let b = after.as_bytes();
    for i in (1..b.len()).rev() {
        let op = after.get(i..i + 2).unwrap_or("");
        let is_cmp = op == "<=" || op == ">=";
        if !is_cmp || b.get(i.wrapping_sub(1)) != Some(&b' ') || b.get(i + 2) != Some(&b' ') {
            continue;
        }
        let lhs = after[..i].trim_end();
        let rhs = after[i + 2..].trim_start();
        if !lhs.is_empty() && !rhs.is_empty() {
            return Some((
                lhs.to_string(),
                after[i..i + 2].to_string(),
                rhs.to_string(),
            ));
        }
    }
    None
}
