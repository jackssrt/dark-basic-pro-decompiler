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

/// The element load an array access starts with, and where its error block is.
struct Access {
    /// The array's name.
    arr: String,
    /// The index the `MOV EBX` loads.
    idx: String,
    /// Instruction index of the `MOV EAX,[EAX+EBX*4]` element load.
    elem: usize,
    /// Instruction index the bounds failure jumps to.
    err: usize,
}

/// The decompiler.
pub struct Dec<'a> {
    pub prog: &'a Prog,
    pub ins: &'a [Ins],
    n: usize,
    varnames: HashMap<u32, String>,
    /// Statements in address order.
    pub statements: Vec<Stmt>,
    /// User function entry address -> the name the blob records.
    fnames: HashMap<u32, String>,
    /// Branch target address -> label name.
    labels: HashMap<u32, String>,
    /// Statement index -> the index whose address is its branch target.
    by_addr: HashMap<u32, usize>,
    /// Instruction address -> instruction index.
    addr_index: HashMap<u32, usize>,
    /// Target that starts no statement -> the statement it snaps to.
    snap: HashMap<u32, Option<usize>>,
    /// Statement index -> the back edge that closes its loop.
    loophead: HashMap<usize, usize>,
    /// Labels already used by a recovered block.
    consumed_labels: HashSet<u32>,
    /// Undecoded regions.
    pub gaps: usize,
    /// Address of the program epilogue (the final break-out check), when seen.
    pub epilogue: Option<u32>,
    /// Array name -> the element field offsets the code reads or writes.
    pub array_fields: HashMap<String, BTreeSet<u32>>,
    /// Per-run counters.
    pub stats: Stats,
    /// Text of the value currently live in EAX.
    eax: Option<String>,
    /// Labels a structured block swallowed but another branch still names.
    forced_labels: HashSet<String>,
    /// Labels already printed in this pass.
    printed: HashSet<u32>,
    /// Position whose subscript is being parsed, if any.  Stops the expression
    /// parser from re-entering the array-access parser at that same position.
    subscript_at: Option<usize>,
    /// The multi-dimensional offset calculation sitting immediately before
    /// the statement being decoded: `(statement index, start address,
    /// temporary, subscripts in source order)`.  A statement that reads the
    /// temporary absorbs the calculation, because the compiler split one
    /// source subscript calculation from the access that consumes it.
    active_offset: Option<(usize, u32, String, Vec<String>)>,
    /// The calculation a statement just emitted, for the next statement to
    /// absorb if it reads the temporary.
    pending_offset: Option<(usize, u32, String, Vec<String>)>,
    /// The offset temporary the statement just recognised read.
    used_offset: Option<String>,
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
            fnames: HashMap::new(),
            labels: HashMap::new(),
            by_addr: HashMap::new(),
            addr_index: prog
                .ins
                .iter()
                .enumerate()
                .map(|(i, x)| (x.addr, i))
                .collect(),
            snap: HashMap::new(),
            loophead: HashMap::new(),
            consumed_labels: HashSet::new(),
            gaps: 0,
            epilogue: None,
            array_fields: HashMap::new(),
            stats: Stats::default(),
            eax: None,
            subscript_at: None,
            active_offset: None,
            pending_offset: None,
            used_offset: None,
            forced_labels: HashSet::new(),
            printed: HashSet::new(),
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

    /// The name the blob gives the function at `addr`, or its address.
    fn funcname(&self, addr: u32) -> String {
        self.fnames
            .get(&addr)
            .cloned()
            .unwrap_or_else(|| format!("FUNC_{:X}", addr))
    }

    /// The user-function names the blob records, keyed by entry address.
    pub fn name_functions(&mut self, names: HashMap<u32, String>) {
        self.fnames = names;
    }

    /// Every user function's entry address and its parameter count, in
    /// address (= source) order.
    ///
    /// The parameter count comes from the calls: the compiler pushes one
    /// argument per parameter, so a call site states it exactly.
    pub fn functions(&self) -> Vec<(u32, usize)> {
        let mut counts: HashMap<u32, usize> = HashMap::new();
        for s in &self.statements {
            if s.kind != Kind::Funcall {
                continue;
            }
            let Some((args, _)) = &s.extra else { continue };
            let Some(addr) = s.target else { continue };
            counts
                .entry(addr)
                .and_modify(|n| *n = (*n).max(args.len()))
                .or_insert(args.len());
        }
        self.statements
            .iter()
            .filter(|s| s.kind == Kind::Func)
            .map(|s| (s.addr, counts.get(&s.addr).copied().unwrap_or(0)))
            .collect()
    }

    /// Interpret a 32-bit operand as a signed displacement.
    fn sd(v: i64) -> i64 {
        if v > 0x7FFF_FFFF {
            v - 0x1_0000_0000
        } else {
            v
        }
    }

    /// The immediate of instruction `k`'s second operand, when it has one.
    fn second_imm(&self, k: usize) -> Option<u32> {
        let x = self.ins.get(k)?;
        match x.reference2 {
            Some(r) if r.kind == ref_type::IMMEDIATE => Some(r.index),
            Some(_) => None,
            None => x.raw2,
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

    /// The index of the instruction at `addr`, when one starts there.
    fn index_at(&self, addr: u32) -> Option<usize> {
        self.addr_index.get(&addr).copied()
    }

    /// Where a compiler-internal branch goes.
    ///
    /// The compiler's own jumps -- the array checks, the DIV guard, the leap
    /// markers (`WriteASMLeapMarkerJump`) -- carry their distance as an
    /// immediate reference (kind `IMMEDIATE`, the finished rel32). A source
    /// `IF`/`GOTO` carries a `LABEL` reference instead, which the loader turns
    /// into the same kind of displacement. The reference *kind* is what
    /// separates scaffolding from a branch the programmer wrote.
    fn internal_jump(&self, k: usize) -> Option<u32> {
        let x = self.ins.get(k)?;
        match x.reference {
            Some(r) if r.kind == ref_type::IMMEDIATE => {
                Some((x.addr + x.size).wrapping_add(r.index))
            }
            None => {
                let rel = x.raw? as i32;
                Some((x.addr + x.size).wrapping_add(rel as u32))
            }
            _ => None,
        }
    }

    /// The instruction a compiler-internal branch lands on.
    fn internal_target(&self, k: usize) -> Option<usize> {
        self.index_at(self.internal_jump(k)?)
    }

    /// A compiler null-check: `<load array>; CMP EAX,0; JE <skip>`.
    ///
    /// This is how the array code guards every element access when safe arrays
    /// are on (`GetArrayCheckFlag`): the guarded run only runs when the array
    /// pointer exists. Returns the array's name and the instruction the guard
    /// jumps to.
    fn null_guard(&mut self, k: usize) -> Option<(String, usize)> {
        let arr = match self.name_at(k) {
            "MOV EAX MEM4" => self.varname(self.ref_kind(k, ref_type::VARIABLE)?),
            "MOV EAX4 [EBP+A]" => Self::ebpname(self.imm(k)?),
            _ => return None,
        };
        if !(self.is(k + 1, "CMP EAX4") && self.imm(k + 1) == Some(0) && self.is(k + 2, "JE")) {
            return None;
        }
        Some((arr, self.internal_target(k + 2)?))
    }

    // ----------------------------------------------------------- expressions

    /// `... -> EAX`.
    fn load(&mut self, k: usize) -> Option<Expr> {
        let x = self.ins.get(k)?;
        let nm = x.name;
        // An array element is a value like any other: the whole access (with
        // its subscript calculation in front) or just the access itself, when
        // the subscript's temporary was stored by an earlier statement.
        if self.subscript_at != Some(k)
            && let Some(e) = self.array_expr(k).or_else(|| self.access_read(k, None))
        {
            return Some(e);
        }
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
            && matches!(
                x.reference.map(|r| r.kind),
                None | Some(ref_type::IMMEDIATE)
            )
            && let Some(hi) = self.imm(k)
            && self.is(k + 1, "MOV EAX IMM4")
            && matches!(
                self.ins[k + 1].reference.map(|r| r.kind),
                None | Some(ref_type::IMMEDIATE)
            )
            && let Some(lo) = self.imm(k + 1)
        {
            // a 64-bit (double integer) literal: the compiler splits it into
            // its low dword in EAX and its high dword in EDX
            return Some((format!("{{{lo},{hi}}}"), k + 2));
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
        // A 64-bit result arriving in EDX:EAX from a DLL call.  The compiler
        // spills the high dword next to the low one and gets both back for the
        // pair push (`PUSH EDX` / `PUSH EAX`):
        //
        //   MOV EAX,EDX   MOV [EBP+d+4],EAX   MOV EAX,[EBP+d+4]
        //   MOV EDX,EAX   MOV EAX,[EBP+d]      PUSH EDX  PUSH EAX
        //
        // The value is the 8-byte slot at `d`.
        if nm == "MOV EAX EDX4"
            && self.is(k + 1, "MOV [EBP+A] EAX4")
            && self.is(k + 2, "MOV EAX4 [EBP+A]")
            && self.is(k + 3, "MOV EDX EAX")
            && self.is(k + 4, "MOV EAX4 [EBP+A]")
            && self.is(k + 5, "PUSH EDX")
            && self.is(k + 6, "PUSH EAX")
            && let (Some(hi), Some(lo)) = (self.imm(k + 1), self.imm(k + 4))
            && self.imm(k + 2) == Some(hi)
            && Self::sd(hi) == Self::sd(lo) + 4
        {
            return Some((format!("{}#", Self::ebpname(lo)), k + 5));
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
        if nm.starts_with("IDIV EAX EBX") || (nm == "CMP EBX4" && self.imm(j) == Some(0)) {
            let j2 = self.divblock(j)?;
            let op = if self.name_at(j2 - 1).starts_with("MOV EAX EDX") {
                "MOD"
            } else {
                "/"
            };
            return Some((format!("{left_text} {op} {right_text}"), j2));
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
                // a 64-bit literal is pushed as its high dword, then its low
                Some(e)
                    if e.0.starts_with('{')
                        && self.is(e.1, "PUSH EDX")
                        && self.is(e.1 + 1, "PUSH EAX") =>
                {
                    args.push(e.0);
                    j = e.1 + 2;
                }
                // an 8-byte value already in ST0 pushes as two dwords
                Some(e) if e.0.ends_with('#') => {
                    if self.is(e.1, "PUSH EDX") && self.is(e.1 + 1, "PUSH EAX") {
                        // a 64-bit result spilled to memory: high, then low
                        args.push(e.0);
                        j = e.1 + 2;
                        continue;
                    }
                    match self.push_double(e.1) {
                        Some(pd) => {
                            args.push(e.0);
                            j = pd;
                        }
                        None => break,
                    }
                }
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
            s.end = self.endof(j - 1);
            self.push_stmt(s);
            self.active_offset = None;
            self.pending_offset = None;
            return j;
        }

        // The calculation the previous statement emitted is available to this
        // one, which may read its temporary; once this statement is decoded,
        // the calculation is either absorbed or dropped.
        self.active_offset = self.pending_offset.take();
        self.used_offset = None;
        let base = self.statements.len();
        let j = match self.try_all(k) {
            Some(j) => {
                self.fuse_offset(base);
                j
            }
            None => {
                self.active_offset = None;
                self.st_gap(k)
            }
        };
        self.active_offset = None;
        j
    }

    /// Absorb the offset statement the recognised statement just consumed.
    ///
    /// A multi-dimensional access is emitted by the compiler in two pieces:
    /// the subscript calculation stores a flat offset in a temporary, and the
    /// access reads that temporary.  When the two land in consecutive
    /// statements, the first is scaffolding, not a source statement, so it is
    /// folded into the access that reads it.
    fn fuse_offset(&mut self, base: usize) -> bool {
        let Some((idx, addr, tmp, _)) = self.active_offset.take() else {
            return false;
        };
        if self.used_offset.as_deref() != Some(tmp.as_str()) {
            return false;
        }
        if idx + 1 != base
            || idx >= self.statements.len()
            || self.statements.len() != base + 1
            || self.statements[idx].addr != addr
        {
            return false;
        }
        self.statements.remove(idx);
        if let Some(s) = self.statements.get_mut(idx) {
            s.addr = addr;
        }
        true
    }

    /// Every recogniser except [`Dec::st_gap`], in priority order.
    fn try_all(&mut self, k: usize) -> Option<usize> {
        self.used_offset = None;
        self.st_frame(k)
            .or_else(|| self.st_sync(k))
            .or_else(|| self.st_array_read(k))
            .or_else(|| self.st_array_write(k))
            .or_else(|| self.st_arridx(k))
            .or_else(|| self.st_call(k))
            .or_else(|| self.st_binop(k))
            .or_else(|| self.st_immediate_op(k))
            .or_else(|| self.st_assign(k))
            .or_else(|| self.st_condition(k))
            .or_else(|| self.st_funcall(k))
            .or_else(|| self.st_jump(k))
            .or_else(|| self.st_double_imm(k))
            .or_else(|| self.st_case(k))
            .or_else(|| self.st_incdec(k))
            .or_else(|| self.st_break_check(k))
            .or_else(|| self.st_misc(k))
            .or_else(|| self.st_return_value(k))
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
                self.active_offset = self.pending_offset.take();
                self.used_offset = None;
                let base = self.statements.len();
                match self.try_all(k) {
                    Some(j) if k < j && j <= hi => {
                        let fused = self.fuse_offset(base);
                        let at = if fused { base - 1 } else { base };
                        let fresh: Vec<Stmt> = self.statements.split_off(at);
                        if let Some(r) = run_start.take() {
                            self.add_gap(r, k);
                        }
                        self.statements.extend(fresh);
                        k = j;
                    }
                    _ => {
                        self.active_offset = None;
                        self.pending_offset = None;
                        run_start = Some(run_start.unwrap_or(k));
                        k += 1;
                    }
                }
            }
            self.active_offset = None;
            self.pending_offset = None;
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
        // A call whose result is written straight into an array element is one
        // BASIC statement: `arr(i).f = NAME(...)`.
        if ret.is_none()
            && let Some((lhs, next)) = self.elem_store(j)
        {
            let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
            s.text = Some(format!("{lhs} = {call}"));
            s.end = self.endof(next - 1);
            self.push_stmt(s);
            self.stats.assign += 1;
            self.stats.array += 1;
            self.eax = None;
            return Some(next);
        }
        let mut s = Stmt::new(self.ins[k].addr, Kind::Call);
        s.text = Some(name);
        s.extra = Some((args, ret.clone()));
        s.end = self.endof(j - 1);
        self.push_stmt(s);
        self.eax = ret.is_none().then_some(call);
        Some(j)
    }

    /// `MOV ECX,EAX` + an element store: where a call's result lands.  The
    /// call's own argument pops follow the store.
    fn elem_store(&mut self, k: usize) -> Option<(String, usize)> {
        if !self.is(k, "MOV ECX EAX") {
            return None;
        }
        let (lhs, mut j) = self.access_write(k + 1, None)?;
        while self.is(j, "POP EBX") || self.is(j, "POP EAX") {
            j += 1;
        }
        Some((lhs, j))
    }

    /// `SYNC()` -- `BUILD_SYNC`: process messages and leave the program when
    /// the runtime reports it was asked to quit.
    ///
    /// ```text
    /// MOV EBX, cmd ProcessMessages
    /// CALL EBX
    /// CMP EAX,1
    /// JE $labelend
    /// ```
    fn st_sync(&mut self, k: usize) -> Option<usize> {
        let index = self.call_at(k)?;
        if self.command_name(index) != "PROCESS MESSAGES" {
            return None;
        }
        let mut j = k + 2;
        if !(self.is(j, "CMP EAX4") && self.imm(j) == Some(1) && self.is(j + 1, "JE")) {
            return None;
        }
        j += 2;
        let mut s = Stmt::new(self.ins[k].addr, Kind::Call);
        s.text = Some("SYNC".to_string());
        s.extra = Some((Vec::new(), None));
        s.end = self.endof(j - 1);
        self.push_stmt(s);
        self.stats.cmd += 1;
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
            s.end = self.endof(j - 1);
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
        // `MOV ECX,<variable>` loads the slot's *address* (ref type 3 patches
        // `m_pVariableSpace+offset`), so a following `MOV [ECX+A],EAX` writes
        // the variable at `offset + A` -- that is how a store to a user-type
        // field or to an unaligned slot is spelled.
        if nm == "MOV ECX IMM4"
            && let Some(base) = self.ref_kind(k, ref_type::VARIABLE)
            && matches!(
                self.name_at(k + 1),
                "MOV [ECX+A] EAX1" | "MOV [ECX+A] EAX2" | "MOV [ECX+A] EAX4"
            )
            && let Some(d) = self.imm(k + 1)
        {
            let name = self.varname(base);
            let off = Self::sd(d);
            let tgt = if off == 0 {
                name
            } else {
                format!("{name}.{off}")
            };
            return Some((tgt, k + 2));
        }
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
        s.end = self.endof(after - 1);
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
        s.end = self.endof(after - 1);
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
        s.end = self.endof(after - 1);
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

    // -------------------------------------------------------- array accesses
    //
    // With safe arrays on (`GetArrayCheckFlag`) the compiler brackets every
    // element access with its own safety code: a null check on the array
    // pointer, a bounds check against the element count at `[EAX-16]`, and an
    // unreachable runtime-error block (118).  Every one of those jumps is a raw
    // relative displacement (`WriteASMLeapMarkerJump`), which is what makes it
    // distinguishable from a source-level `IF`, whose target is a reference-
    // table label.

    /// The element load every access starts with.
    ///
    /// `CalculateArrayOffsetInEBX` + `MOV EAX,[EAX+EBX*4]`:
    ///
    /// ```text
    /// MOV EAX,<array>   CMP EAX,0   JE <error>
    /// MOV EBX,<index>
    /// PUSH EDX  MOV EDX,[EAX-16]  CMP EDX,EBX  POP EDX  JGE <error>
    /// CMP EBX,-1   JLE <error>
    /// MOV EAX,[EAX+EBX*4]
    /// ```
    fn access_head(&mut self, k: usize) -> Option<Access> {
        let (arr, err) = self.null_guard(k)?;
        let (idx, mut j) = self.array_index(k + 3)?;
        if self.is(j, "PUSH EDX")
            && self.is(j + 1, "MOV EDX4 [EAX+A]")
            && self.imm(j + 1).is_some_and(|d| Self::sd(d) == -16)
            && self.is(j + 2, "CMP EDX EBX4")
            && self.is(j + 3, "POP EDX")
            && self.is(j + 4, "JGE")
            && self.is(j + 5, "CMP EBX4")
            && self.imm(j + 5).is_some_and(|d| Self::sd(d) == -1)
            && self.is(j + 6, "JLE")
            && self.internal_target(j + 4) == Some(err)
            && self.internal_target(j + 6) == Some(err)
        {
            j += 7;
        }
        if !self.is(j, "MOV EAX SIB[EAX:EBX*4]") {
            return None;
        }
        Some(Access {
            arr,
            idx,
            elem: j,
            err,
        })
    }

    /// Is `k` the unreachable `MOV [@_ERR_],118` of an array bounds failure?
    fn err_block_at(&self, k: usize) -> bool {
        self.is(k, "MOV MEM IMM4")
            && self.ref_is(k, ref_type::VARIABLE, 4)
            && self.second_imm(k) == Some(118)
    }

    /// `arr(idx).field` read: everything from the element load to the jump that
    /// steps over the error block.  `idx` overrides the loaded index when the
    /// caller already has the subscript text.
    fn access_read(&mut self, k: usize, idx: Option<&str>) -> Option<Expr> {
        let a = self.access_head(k)?;
        let mut j = a.elem + 1;
        let off;
        if matches!(self.name_at(j), "MOV ECX4 [EAX+A]" | "MOV ECX1 [EAX+A]") {
            off = self.imm(j)?;
            if !matches!(self.name_at(j + 1), "MOV EAX ECX4" | "MOV EAX ECX1") {
                return None;
            }
            j += 2;
        } else if matches!(
            self.name_at(j),
            "MOV EAX1 [EAX+A]" | "MOV EAX2 [EAX+A]" | "MOV EAX4 [EAX+A]"
        ) {
            off = self.imm(j)?;
            j += 1;
        } else if matches!(self.name_at(j), "ADD EAX IMM1" | "ADD EAX IMM2" | "ADD EAX IMM4") {
            // a user-type array element is a pointer, offset by the field
            off = self.imm(j)?;
            j += 1;
        } else {
            return None;
        }
        if !self.is(j, "JMP") {
            return None;
        }
        let end = self.internal_target(j)?;
        if !self.err_block_at(a.err) || end <= a.err {
            return None;
        }
        let idx = self.subscript_text(idx, &a.idx);
        self.array_fields
            .entry(a.arr.clone())
            .or_default()
            .insert(off as u32);
        Some((
            format!("{}({idx}){}", a.arr, Self::field_suffix(off)),
            end,
        ))
    }

    /// `arr(idx).field = <value>`: the same head, with the value already in
    /// ECX.
    fn access_write(&mut self, k: usize, idx: Option<&str>) -> Option<(String, usize)> {
        let a = self.access_head(k)?;
        let j = a.elem + 1;
        if !matches!(
            self.name_at(j),
            "MOV [EAX+A] ECX1" | "MOV [EAX+A] ECX2" | "MOV [EAX+A] ECX4"
        ) {
            return None;
        }
        let off = self.imm(j)?;
        if !self.is(j + 1, "JMP") {
            return None;
        }
        let end = self.internal_target(j + 1)?;
        if !self.err_block_at(a.err) || end <= a.err {
            return None;
        }
        let idx = self.subscript_text(idx, &a.idx);
        self.array_fields
            .entry(a.arr.clone())
            .or_default()
            .insert(off as u32);
        Some((
            format!("{}({idx}){}", a.arr, Self::field_suffix(off)),
            end,
        ))
    }

    /// The subscript text for an access.
    ///
    /// Normally it is the index the access loads itself.  A multi-dimensional
    /// access instead loads a flat *offset* temporary that an earlier
    /// statement computed from the subscripts; that statement recorded them,
    /// and they are what the source said.
    fn subscript_text(&mut self, given: Option<&str>, loaded: &str) -> String {
        if let Some(g) = given {
            return g.to_string();
        }
        match &self.active_offset {
            Some((_, _, tmp, subs)) if tmp == loaded => {
                self.used_offset = Some(loaded.to_string());
                subs.join(", ")
            }
            _ => loaded.to_string(),
        }
    }

    /// `.field`, or nothing when the field offset is zero.
    fn field_suffix(off: i64) -> String {
        if off == 0 {
            String::new()
        } else {
            format!(".{}", Self::sd(off))
        }
    }

    /// The subscript the element access reads, and where it ends.
    ///
    /// `ASMTASK_PUSH` pushes one subscript per dimension; an array used without
    /// a subscript pushes its *internal list index* instead
    /// (`ASMTASK_PUSHINTERNALARRAYINDEX`), which is the value `ARRAY INDEX(arr)`
    /// returns and what the `ARRAY INDEX TO TOP` / `NEXT ARRAY INDEX` loop
    /// machinery walks.
    fn index_push(&mut self, k: usize) -> Option<(String, usize)> {
        if let Some((arr, t)) = self.null_guard(k)
            && self.is(k + 3, "MOV EDX4 [EAX+A]")
            && self.imm(k + 3).is_some_and(|d| Self::sd(d) == -4)
            && self.is(k + 4, "PUSH EDX")
            && t == k + 5
        {
            return Some((format!("ARRAY INDEX({arr})"), k + 5));
        }
        // An element read used as the subscript: `a(b(i).f)`.
        if let Some((text, j)) = self.access_read(k, None)
            && self.is(j, "PUSH EAX")
        {
            return Some((text, j + 1));
        }
        // The subscript itself, pushing its value.  A subscript whose own
        // statement already ended with the value in EAX pushes it bare.
        if self.is(k, "PUSH EAX")
            && let Some(text) = self.eax.clone()
        {
            return Some((text, k + 1));
        }
        let saved = self.subscript_at.replace(k);
        let parsed = self.expr(k);
        self.subscript_at = saved;
        let (text, j) = parsed?;
        if !self.is(j, "PUSH EAX") {
            return None;
        }
        Some((text, j + 1))
    }

    /// `ASMTASK_CALCARRAYOFFSET`: pop the subscript into EBX, move it to EAX and
    /// store it in the compiler's offset temporary.  Returns the temporary and
    /// the index after the block.
    ///
    /// A multi-dimensional access adds one
    /// `POP EDX / IMUL EDX,[EAX-56+4*i] / ADD EBX,EDX` per extra dimension
    /// before the store; `dims` is the number of subscripts the access pushed.
    fn calc_offset(&mut self, k: usize, dims: usize) -> Option<(String, usize)> {
        if dims == 0 {
            return None;
        }
        let (_arr, t) = self.null_guard(k)?;
        let mut j = k + 3;
        if !self.is(j, "POP EBX") {
            return None;
        }
        j += 1;
        for _ in 1..dims {
            if !(self.is(j, "POP EDX")
                && self.is(j + 1, "IMUL EDX4 [EAX+A]")
                && self.imm(j + 1).is_some()
                && self.is(j + 2, "ADD EBX EDX4"))
            {
                return None;
            }
            j += 3;
        }
        if !self.is(j, "MOV EAX EBX") {
            return None;
        }
        j += 1;
        let (tmp, next) = self.store(j)?;
        (t == next).then_some((tmp, next))
    }

    /// Every subscript an array access pushes, in source order.
    ///
    /// `ASMTASK_PUSH` emits one push per dimension; the offset calculation
    /// that follows consumes them.  The loop stops at the calculation, which
    /// starts with the array's null guard followed by `POP EBX`.
    fn subscripts(&mut self, k: usize) -> Option<(Vec<String>, usize)> {
        let mut subs = Vec::new();
        let mut j = k;
        while subs.len() < 8 {
            if self.null_guard(j).is_some() && self.is(j + 3, "POP EBX") {
                break;
            }
            match self.index_push(j) {
                Some((text, next)) if next > j => {
                    subs.push(text);
                    j = next;
                }
                _ => break,
            }
        }
        if subs.is_empty() {
            return None;
        }
        // The compiler pushes the subscripts in reverse order (last subscript
        // first), so the stack order runs backwards through the source.
        subs.reverse();
        Some((subs, j))
    }

    /// A whole element access, subscript calculation included.
    fn array_expr(&mut self, k: usize) -> Option<Expr> {
        let (subs, j) = self.subscripts(k)?;
        let (_tmp, j) = self.calc_offset(j, subs.len())?;
        let idx = subs.join(", ");
        self.access_read(j, Some(&idx))
    }

    /// `x = arr(idx).field` when the whole access sits in one statement.
    fn st_array_read(&mut self, k: usize) -> Option<usize> {
        let (text, j) = self.array_expr(k)?;
        let (tgt, after) = self.store(j)?;
        self.stats.assign += 1;
        self.stats.array += 1;
        let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
        s.text = Some(format!("{tgt} = {text}"));
        s.end = self.endof(after - 1);
        self.push_stmt(s);
        self.eax = Some(text);
        Some(after)
    }

    /// `arr(idx).field = <value>`.
    fn st_array_write(&mut self, k: usize) -> Option<usize> {
        let (val, j) = match self.expr(k) {
            Some(e) if self.is(e.1, "MOV ECX EAX") => e,
            _ => {
                let (v, j) = self.cmd_value(k)?;
                (v, j)
            }
        };
        if !self.is(j, "MOV ECX EAX") {
            return None;
        }
        let (lhs, mut next) = self.access_write(j + 1, None)?;
        while self.is(next, "POP EBX") || self.is(next, "POP EAX") {
            next += 1;
        }
        let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
        s.text = Some(format!("{lhs} = {val}"));
        s.end = self.endof(next - 1);
        self.push_stmt(s);
        self.stats.assign += 1;
        self.stats.array += 1;
        self.eax = None;
        Some(next)
    }

    /// The subscript's offset calculation on its own (`ASMTASK_CALCARRAYOFFSET`).
    ///
    /// The compiler separates it from the access it belongs to whenever another
    /// statement's code is emitted in between, so it is transcribed as what it
    /// is: `temp = <subscript>`.  The readability pass then inlines it back
    /// into the access that reads it.
    fn st_arridx(&mut self, k: usize) -> Option<usize> {
        let (subs, j) = self.subscripts(k)?;
        let dims = subs.len();
        let (tmp, next) = self.calc_offset(j, dims)?;
        let text = if dims == 1 {
            format!("{tmp} = {}", subs[0])
        } else {
            // The compiler folds the subscripts into a flat offset using the
            // array's own dimension sizes, which are runtime values.  Record
            // what it does know: the subscripts, and the temporary they land
            // in.  A statement that reads the temporary absorbs it.
            format!("{tmp} = ARRAY OFFSET({})", subs.join(", "))
        };
        let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
        s.text = Some(text);
        s.end = self.endof(next - 1);
        self.push_stmt(s);
        self.stats.assign += 1;
        self.eax = None;
        if dims > 1 {
            let idx = self.statements.len() - 1;
            self.pending_offset = Some((idx, self.ins[k].addr, tmp, subs));
        }
        Some(next)
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
        s.end = self.endof(j - 1);
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
        // Only a reference-table label is a source-level branch.  A raw
        // relative jump is the compiler's own scaffolding -- the null and
        // bounds checks of an array access -- and belongs to that statement.
        self.ref_kind(j, ref_type::LABEL)?;
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
        let (mut args, mut j) = match self.pushes(k, None) {
            Some(pr) => pr,
            None => (Vec::new(), k), // a call with no parameters
        };
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
        let addr = t.unwrap_or(0);
        if ret.is_none()
            && let Some((lhs, next)) = self.elem_store(j)
        {
            let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
            s.text = Some(format!("{lhs} = CALL {}({})", self.funcname(addr), args.join(", ")));
            s.end = self.endof(next - 1);
            self.push_stmt(s);
            self.stats.assign += 1;
            self.stats.array += 1;
            Some(next)
        } else {
            let mut s = Stmt::new(self.ins[k].addr, Kind::Funcall);
            s.target = Some(addr);
            s.extra = Some((args, ret));
            s.end = self.endof(j - 1);
            self.push_stmt(s);
            self.stats.funcall += 1;
            Some(j)
        }
    }

    /// One arm of a `SELECT` dispatch: `CMP EAX,<value>` + `JE <label>`.
    ///
    /// The value tested was left in EAX by the preceding statement
    /// (`ASMTASK_ASSIGNTOEAX`); each `CASE` becomes one comparison against it.
    fn st_case(&mut self, k: usize) -> Option<usize> {
        // The first comparison of a chain loads the tested value; the rest use
        // the value the load left in EAX.
        let (tested, at) = if self.is(k, "CMP EAX4") {
            (self.eax.clone()?, k)
        } else {
            self.expr(k)?
        };
        if !self.is(at, "CMP EAX4") {
            return None;
        }
        let value = self.imm(at)?;
        let cc = self.name_at(at + 1);
        if !matches!(cc, "JE" | "JNE") {
            return None;
        }
        let label = self.ref_kind(at + 1, ref_type::LABEL)?;
        let lbl = self.label(Some(label));
        let op = if cc == "JE" { "=" } else { "<>" };
        let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
        s.text = Some(format!("IF {tested} {op} {value} THEN GOTO {lbl}"));
        // the case body label must also be emitted where it lands
        s.target = Some(label);
        s.end = self.endof(at + 1);
        self.push_stmt(s);
        self.eax = Some(tested);
        Some(at + 2)
    }

    /// A 64-bit floating literal: the compiler stores its two dwords into the
    /// register spill slots `_TEMPA_` / `_TEMPB_`.
    fn st_double_imm(&mut self, k: usize) -> Option<usize> {
        let pair = self.is(k, "MOV MEM IMM4")
            && self.ref_is(k, ref_type::VARIABLE, 20)
            && self.is(k + 1, "MOV MEM IMM4")
            && self.ref_is(k + 1, ref_type::VARIABLE, 24);
        if !pair {
            return None;
        }
        let lo = self.second_imm(k)? as u64;
        let hi = self.second_imm(k + 1)? as u64;
        let value = f64::from_bits((hi << 32) | lo);
        let mut s = Stmt::new(self.ins[k].addr, Kind::Assign);
        s.text = Some(format!("_TEMPA_# = {value}"));
        s.end = self.endof(k + 1);
        self.push_stmt(s);
        self.stats.assign += 1;
        Some(k + 2)
    }

    /// The runtime's break-out check the program epilogue starts with.
    fn st_break_check(&mut self, k: usize) -> Option<usize> {
        if !(self.is(k, "MOV EAX MEM4")
            && self.ref_is(k, ref_type::VARIABLE, 12)
            && self.is(k + 1, "CMP EAX4")
            && self.imm(k + 1) == Some(0)
            && self.is(k + 2, "JNE"))
        {
            return None;
        }
        self.internal_target(k + 2)?;
        self.epilogue.get_or_insert(self.ins[k].addr);
        let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
        s.text = Some("' [break-out check: quit unless the runtime asked to stop]".to_string());
        s.end = self.endof(k + 2);
        self.push_stmt(s);
        Some(k + 3)
    }

    /// A bare branch.
    fn st_jump(&mut self, k: usize) -> Option<usize> {
        let nm = self.name_at(k);
        if !matches!(nm, "JMP" | "JNE" | "JE" | "JGE" | "JLE") {
            return None;
        }
        // A raw relative jump is compiler scaffolding (the array error-block
        // leap, the DIV guard); only a label is a source `GOTO`.
        self.ref_kind(k, ref_type::LABEL)?;
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

    /// `INC v` / `DEC v` (`BUILD_INC` / `BUILD_DEC` of a single variable).
    fn st_incdec(&mut self, k: usize) -> Option<usize> {
        let nm = self.name_at(k);
        let op = match nm {
            "INC MEM1" | "INC MEM2" | "INC MEM4" => "INC",
            "DEC MEM1" | "DEC MEM2" | "DEC MEM4" => "DEC",
            _ => return None,
        };
        let var = self.varname(self.ref_kind(k, ref_type::VARIABLE)?);
        let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
        s.text = Some(format!("{op} {var}"));
        s.end = self.endof(k);
        self.push_stmt(s);
        Some(k + 1)
    }

    /// `RETURN <value>`: the value the epilogue leaves in EAX.
    fn st_return_value(&mut self, k: usize) -> Option<usize> {
        let (text, j) = self.expr(k)?;
        if !(self.is(j, "MOV ESP EBP") && self.is(j + 1, "POP EBP") && self.is(j + 2, "RET")) {
            return None;
        }
        let mut s = Stmt::new(self.ins[k].addr, Kind::Misc);
        s.text = Some(format!("RETURN {text}"));
        s.end = self.endof(j + 2);
        self.push_stmt(s);
        Some(j + 3)
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
            // the name is resolved when the program is rendered, so it can
            // come from the blob's own signature table
            let text = match size {
                Some(n) => format!("(stack frame {n} bytes)"),
                None => "(stack frame unknown)".to_string(),
            };
            let mut s = Stmt::new(self.ins[k].addr, Kind::Func);
            s.text = Some(text);
            s.target = Some(self.ins[k].addr);
            s.end = self.endof(j - 1);
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

        // A label a structured block ends at is not printed, because the block
        // itself shows where it lands.  Another branch may still name that
        // address -- an unstructured `GOTO`, or a `CASE` whose body starts
        // there -- so render once, see which names went missing, and render
        // again with those labels pinned.
        self.forced_labels.clear();
        let out = self.render_once();
        let missing = missing_labels(&out);
        if missing.is_empty() {
            return out;
        }
        self.forced_labels = missing;
        self.render_once()
    }

    fn render_once(&mut self) -> Vec<String> {
        self.consumed_labels.clear();
        self.printed.clear();
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
                        self.flush_labels(start, bj + 1, depth, out);
                        out.push(format!(
                            "{}UNTIL {} = 0",
                            Self::ind(depth),
                            c.cond.as_deref().unwrap_or("")
                        ));
                    }
                    None => {
                        out.push(format!("{}DO", Self::ind(depth)));
                        self.block(i, start, depth + 1, out);
                        self.flush_labels(start, bj + 1, depth, out);
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
                out.push(format!(
                    "{}FUNCTION {}  {}",
                    Self::ind(depth),
                    self.funcname(s.addr),
                    s.text.as_deref().unwrap_or("")
                ));
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
                && (!self.consumed_labels.contains(&s.addr)
                    || self.forced_labels.contains(label))
            {
                out.push(format!("{}{}:", Self::ind(depth), label));
                self.printed.insert(s.addr);
            }
            self.emit(&s, out, depth);
            i += 1;
        }
    }

    /// Print the labels of statements a loop consumed, when a branch names
    /// them: a jump into the back edge has to land somewhere.
    fn flush_labels(&mut self, lo: usize, hi: usize, depth: usize, out: &mut Vec<String>) {
        for i in lo..hi.min(self.statements.len()) {
            let addr = self.statements[i].addr;
            let Some(label) = self.labels.get(&addr) else {
                continue;
            };
            if self.forced_labels.contains(label) && self.printed.insert(addr) {
                out.push(format!("{}{}:", Self::ind(depth), label));
            }
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
        self.flush_labels(c, bj + 1, depth, out);
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
                let name = if s.kind == Kind::Funcall {
                    format!("CALL {}", self.funcname(s.target.unwrap_or(0)))
                } else {
                    s.text.clone().unwrap_or_default()
                };
                let call = format!("{name}({args})");
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
                // `cc` says when the branch is taken: `JE` (false) jumps on
                // zero, `JNE` (true) jumps on anything else.
                if s.cc == Some("false") {
                    out.push(format!("IF {cond} = 0 THEN GOTO {lbl}"));
                } else {
                    out.push(format!("IF {cond} <> 0 THEN GOTO {lbl}"));
                }
            }
            Kind::Jump => {
                if s.target == Some(0xFFFF_FFFF) {
                    out.push(format!("{ind}END"));
                } else {
                    out.push(format!("{ind}GOTO {}", self.label(s.target)));
                }
            }
            Kind::Func => out.push(format!(
                "{ind}FUNCTION {}  {}",
                self.funcname(s.target.unwrap_or(s.addr)),
                s.text.as_deref().unwrap_or("")
            )),
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

/// Labels the rendered text names but never defines.
fn missing_labels(out: &[String]) -> HashSet<String> {
    let mut defined: HashSet<&str> = HashSet::new();
    let mut used: HashSet<&str> = HashSet::new();
    for line in out {
        let s = line.trim();
        if let Some(name) = s.strip_suffix(':')
            && name.len() == 7
            && name.starts_with('L')
            && name[1..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            defined.insert(name);
        }
        if let Some(at) = s.rfind("GOTO ")
            && (at == 0 || s[..at].ends_with(' '))
        {
            used.insert(&s[at + 5..]);
        }
    }
    used.into_iter()
        .filter(|l| !defined.contains(*l))
        .map(str::to_string)
        .collect()
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
