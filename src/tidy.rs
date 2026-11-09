//! The readability pass over the recovered BASIC.
//!
//! The same program, written so a person can read it:
//!
//! * `' ==== source line N ====` markers and the compiler's bounds-check notes
//!   are dropped (the line numbers survive in the header legend),
//! * the compiler's scratch variables are copy-propagated through
//!   straight-line code and the now-dead stores deleted,
//! * expressions are folded: unsigned literals become signed, IEEE-754
//!   immediates become decimal floats, DBP operator calls (`EQUAL`, `ADD`,
//!   `CAST INTEGER TO DOUBLE` ...) become operators, constant sub-expressions
//!   are evaluated, and `<> 0` is dropped from tests that are already boolean,
//! * lowering artefacts are undone: the `v = a - s` seed in front of FOR,
//!   `GOTO` to the program epilogue becomes `END`, and
//!   `IF c THEN ... GOTO L ENDIF ... L:` becomes a real IF/ELSE,
//! * undecoded regions stay, compressed to one comment each,
//! * variables are renamed after their role (loop counters, timers, flags,
//!   modes, arrays, paths, user types); the header legend maps every new name
//!   back to the variable-space byte offset it came from.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::patterns::{self, Shape};
use std::fmt::Write as _;

/// The header `readable.dba` opens with, named for the program it came from.
pub fn header(appname: &str) -> String {
    let name = if appname.is_empty() {
        "a compiled DBP game"
    } else {
        appname
    };
    let rule = "' ======================================================================\n";
    let mut h = String::from(rule);
    h.push_str(&format!("' {name} -- DarkBASIC PRO, recovered as readable source\n"));
    h.push_str(
        "'\n\
         ' Produced from recovered.dba by dbp-decompile.  Nothing was executed: this is a\n\
         ' static reading of the compiled machine code in _virtual.dat.\n\
         '\n\
         ' Reading notes\n\
         '   * statement order is the original one, and variables are named for the\n\
         '     role their use implies -- loop counter, elapsed-time reference, file\n\
         '     path, array, user-type record.  The real identifiers are not in the\n\
         '     format (it stores byte offsets, not names), so every name below was\n\
         '     inferred; recovered.dba lists each one against its offset.\n\
         '   * calls with side effects -- RND, TIMER, GET ..., every DLL command --\n\
         '     stay exactly where the compiler put them, so their number and order\n\
         '     match recovered.dba.  Only side-effect-free values are inlined.\n\
         '   * library operator calls are written back as operators:\n\
         '       EQUAL / GREATER / LESS      ->  = > <\n\
         '       ADD / SUB / MUL / DIV       ->  + - * /\n\
         '       CAST ... TO DOUBLE / FLOAT  ->  the value itself\n\
         '   * literals are re-typed the way DBP would have written them: a DWORD with\n\
         '     the high bit set is negative (4294965296 -> -2000), and an IEEE-754 bit\n\
         '     pattern that decodes to a short decimal becomes that decimal\n\
         '     (1073741824 -> 2.0, 1050253722 -> 0.3).\n\
         '   * the compiler's loop lowering is undone, the IF ... GOTO L / L: shape\n\
         '     becomes a real IF/ELSE, and a jump to the program epilogue becomes END.\n\
         '   * regions this pass could not decode are kept as one-line comments\n\
         '     carrying the raw instructions, so nothing is hidden.\n\
         ' ======================================================================\n",
    );
    h
}

/// One entry of the header legend: `(new name, old offset, why)`.
pub type Legend = Vec<(String, String, String)>;

// ============================================================== expressions

/// A numeric literal, keeping DBP's int/float distinction.
#[derive(Clone, Copy, Debug)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    fn as_f64(self) -> f64 {
        match self {
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }
}

impl PartialEq for Num {
    /// Python compares `1 == 1.0`, so mixed literals are equal here too.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a == b,
            _ => self.as_f64() == other.as_f64(),
        }
    }
}

/// A parsed expression.
#[derive(Clone, Debug, PartialEq)]
enum Expr {
    Num(Num),
    Str(String),
    Id(String),
    /// `base.field`, where the base is a plain identifier.
    Fld(Box<Expr>, String),
    /// `base[i, j, ...]`.
    Arr(Box<Expr>, Vec<Expr>),
    /// `NAME(args)`.
    Call(String, Vec<Expr>),
    /// `lhs op rhs`, `op` being a symbolic operator or `AND` / `OR`.
    Bin(String, Box<Expr>, Box<Expr>),
    /// `NOT x` or `- x`.
    Un(&'static str, Box<Expr>),
}

const KEYWORDS: &[&str] = &["AND", "OR", "NOT", "TO", "STEP"];
const CMP: &[&str] = &["=", "<>", "<", ">", "<=", ">="];

fn prec_of(op: &str) -> u8 {
    match op {
        "OR" => 30,
        "AND" => 40,
        "NOT" => 45,
        "=" | "<>" | "<" | ">" | "<=" | ">=" => 50,
        "+" | "-" => 60,
        "*" | "/" => 70,
        _ => 100,
    }
}

/// One token: the pattern it matched and its text.
#[derive(Clone, Debug, PartialEq)]
struct Tok {
    kind: &'static str,
    val: String,
}

fn tok(kind: &'static str, val: &str) -> Tok {
    Tok {
        kind,
        val: val.to_string(),
    }
}

/// Matches the identifier shapes the decompiler emits.
fn name_at(s: &str) -> Option<usize> {
    // `[ECX+4]` / `[ECX-4]`
    if let Some(rest) = s.strip_prefix("[ECX") {
        let digits = rest.strip_prefix('+').or_else(|| rest.strip_prefix('-'))?;
        let n: String = digits.chars().take_while(char::is_ascii_digit).collect();
        if !n.is_empty() && rest[n.len() + 1..].starts_with(']') {
            // "[ECX", the sign, the digits and the bracket.
            return Some(4 + 1 + n.len() + 1);
        }
    }
    // `loc` carries a signed frame displacement: `loc-8`, `loc+4`.
    if let Some(rest) = s.strip_prefix("loc") {
        if let Some(rest) = rest.strip_prefix('+').or_else(|| rest.strip_prefix('-')) {
            let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !n.is_empty() {
                return Some(3 + 1 + n.len());
            }
        }
    }
    for p in ["par", "data$", "cmd$"] {
        if let Some(rest) = s.strip_prefix(p) {
            let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if !n.is_empty() {
                let base = p.len() + n.len();
                // `var$` styles also allow a `.field` suffix
                if let Some(after) = s.get(base..)
                    && let Some(off) = after.strip_prefix('.')
                {
                    let d: String = off.chars().take_while(char::is_ascii_digit).collect();
                    if !d.is_empty() {
                        return Some(base + 1 + d.len());
                    }
                }
                return Some(base);
            }
        }
    }
    if let Some(rest) = s.strip_prefix("var$") {
        let n: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !n.is_empty() {
            let mut at = 4 + n.len();
            if let Some(after) = s.get(at..)
                && let Some(off) = after.strip_prefix('.')
            {
                let d: String = off.chars().take_while(char::is_ascii_digit).collect();
                if !d.is_empty() {
                    at += 1 + d.len();
                }
            }
            return Some(at);
        }
    }
    if s.starts_with('_') {
        let end = s
            .char_indices()
            .skip(1)
            .take_while(|(_, c)| *c == '_' || c.is_ascii_uppercase())
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0);
        if end >= 2 && s[end..].starts_with('_') {
            return Some(end + 1);
        }
    }
    None
}

/// Split `s` into tokens, whitespace excluded.
fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            continue;
        }
        // string
        if b[i] == b'"' {
            let mut j = i + 1;
            loop {
                if j >= b.len() {
                    return Err(format!("lex error at {i} in {s:?}"));
                }
                if b[j] == b'"' {
                    if b.get(j + 1) == Some(&b'"') {
                        j += 2;
                        continue;
                    }
                    j += 1;
                    break;
                }
                j += 1;
            }
            out.push(tok("str", &s[i..j]));
            i = j;
            continue;
        }
        // number
        if b[i].is_ascii_digit() {
            let mut j = i;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let mut seen_dot = false;
            if j + 1 < b.len() && b[j] == b'.' && b[j + 1].is_ascii_digit() {
                seen_dot = true;
                j += 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
            }
            let text = &s[i..j];
            let text = if seen_dot {
                format!("{text}.0")
            } else {
                text.to_string()
            };
            out.push(tok("num", &text));
            i = j;
            continue;
        }
        if let Some(len) = name_at(&s[i..]) {
            out.push(tok("name", &s[i..i + len]));
            i += len;
            continue;
        }
        // operator
        let two = s
            .as_bytes()
            .get(i + 1)
            .is_some_and(|c| matches!(c, b'=' | b'>'));
        if two && matches!(&s[i..i + 2], "<=" | ">=" | "<>" | "==" | "=>") {
            out.push(tok("op", &s[i..i + 2]));
            i += 2;
            continue;
        }
        if matches!(
            b[i],
            b'<' | b'>' | b'-' | b'+' | b'*' | b'/' | b'(' | b')' | b'=' | b',' | b'#' | b'.'
        ) {
            out.push(tok("op", &s[i..i + 1]));
            i += 1;
            continue;
        }
        // identifier
        if b[i] == b'_' || b[i].is_ascii_alphabetic() {
            let mut j = i + 1;
            while j < b.len() && (b[j] == b'_' || b[j].is_ascii_alphanumeric() || b[j] == b'$') {
                j += 1;
            }
            let text = &s[i..j];
            let kind = if KEYWORDS.contains(&text) { "kw" } else { "id" };
            out.push(tok(kind, text));
            i = j;
            continue;
        }
        return Err(format!("lex error at {i} in {s:?}"));
    }
    Ok(out)
}

/// A recursive-descent parser over the token stream.
struct Parser<'t> {
    t: &'t [Tok],
    i: usize,
}

impl<'t> Parser<'t> {
    fn peek(&self, k: usize) -> Option<&'t Tok> {
        self.t.get(self.i + k)
    }

    fn kind(&self) -> &'static str {
        self.peek(0).map_or("eof", |t| t.kind)
    }

    fn val(&self) -> &str {
        self.peek(0).map_or("", |t| t.val.as_str())
    }

    fn next(&mut self) -> Option<&'t Tok> {
        let t = self.peek(0);
        if t.is_some() {
            self.i += 1;
        }
        t
    }

    fn eat(&mut self, kind: &str, val: Option<&str>) -> bool {
        match self.peek(0) {
            Some(t) if t.kind == kind && val.is_none_or(|v| t.val == v) => {
                self.i += 1;
                true
            }
            _ => false,
        }
    }

    fn expect(&mut self, kind: &str, val: Option<&str>) -> Result<(), String> {
        if self.eat(kind, val) {
            Ok(())
        } else {
            Err(format!("expected {kind} {val:?}, got {:?}", self.peek(0)))
        }
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.or_()
    }

    fn or_(&mut self) -> Result<Expr, String> {
        let mut n = self.and_()?;
        while self.kind() == "kw" && self.val() == "OR" {
            self.next();
            n = Expr::Bin("OR".to_string(), Box::new(n), Box::new(self.and_()?));
        }
        Ok(n)
    }

    fn and_(&mut self) -> Result<Expr, String> {
        let mut n = self.not_()?;
        while self.kind() == "kw" && self.val() == "AND" {
            self.next();
            n = Expr::Bin("AND".to_string(), Box::new(n), Box::new(self.not_()?));
        }
        Ok(n)
    }

    fn not_(&mut self) -> Result<Expr, String> {
        if self.kind() == "kw" && self.val() == "NOT" {
            self.next();
            return Ok(Expr::Un("NOT", Box::new(self.not_()?)));
        }
        self.cmp()
    }

    /// Comparisons chain left to right: the decompiler writes a condition's
    /// zero test as the right-hand side (`IF x > 0 THEN` comes from `x > 0`
    /// plus the `<> 0` stripped later), so `a > b = 0` is `(a > b) = 0`.
    fn cmp(&mut self) -> Result<Expr, String> {
        let mut n = self.add()?;
        loop {
            if self.kind() != "op" || !CMP.contains(&self.val()) {
                return Ok(n);
            }
            let op = self.val().to_string();
            self.next();
            n = Expr::Bin(op, Box::new(n), Box::new(self.add()?));
        }
    }

    fn add(&mut self) -> Result<Expr, String> {
        let mut n = self.mul()?;
        loop {
            if self.kind() == "op" && matches!(self.val(), "+" | "-") {
                let op = self.val().to_string();
                self.next();
                n = Expr::Bin(op, Box::new(n), Box::new(self.mul()?));
            } else {
                return Ok(n);
            }
        }
    }

    fn mul(&mut self) -> Result<Expr, String> {
        let mut n = self.unary()?;
        loop {
            if self.kind() == "op" && matches!(self.val(), "*" | "/") {
                let op = self.val().to_string();
                self.next();
                n = Expr::Bin(op, Box::new(n), Box::new(self.unary()?));
            } else {
                return Ok(n);
            }
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        if self.kind() == "op" && self.val() == "-" {
            self.next();
            return Ok(Expr::Un("-", Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut n = self.primary()?;
        loop {
            if self.kind() == "op" && self.val() == "#" {
                self.next(); // the double marker, dropped
            } else if self.kind() == "op"
                && self.val() == "."
                && matches!(self.peek(1).map(|t| t.kind), Some("id") | Some("num"))
            {
                self.next();
                let field = self.next().map(|t| t.val.clone()).unwrap_or_default();
                n = Expr::Fld(Box::new(n), field);
            } else if self.kind() == "op" && self.val() == "(" {
                self.next();
                let mut subs = vec![self.expr()?];
                while self.eat("op", Some(",")) {
                    subs.push(self.expr()?);
                }
                self.expect("op", Some(")"))?;
                n = Expr::Arr(Box::new(n), subs);
            } else {
                return Ok(n);
            }
        }
    }

    fn args(&mut self) -> Result<Vec<Expr>, String> {
        self.expect("op", Some("("))?;
        let mut a = Vec::new();
        if !self.eat("op", Some(")")) {
            loop {
                a.push(self.expr()?);
                if self.eat("op", Some(",")) {
                    continue;
                }
                self.expect("op", Some(")"))?;
                break;
            }
        }
        Ok(a)
    }

    fn primary(&mut self) -> Result<Expr, String> {
        match self.kind() {
            "num" => {
                let v = self.next().map(|t| t.val.clone()).unwrap_or_default();
                let n = if v.contains('.') {
                    Num::Float(v.parse().unwrap_or(0.0))
                } else {
                    Num::Int(norm_int(v.parse().unwrap_or(0)))
                };
                Ok(Expr::Num(n))
            }
            "str" => Ok(Expr::Str(
                self.next().map(|t| t.val.clone()).unwrap_or_default(),
            )),
            "op" if self.val() == "(" => {
                self.next();
                let n = self.expr()?;
                self.expect("op", Some(")"))?;
                Ok(n)
            }
            "id" => self.name_or_call(),
            "name" => Ok(Expr::Id(
                self.next().map(|t| t.val.clone()).unwrap_or_default(),
            )),
            _ => Err(format!("unexpected {:?}", self.peek(0))),
        }
    }

    fn name_or_call(&mut self) -> Result<Expr, String> {
        let mut words = Vec::new();
        loop {
            if !matches!(self.kind(), "id" | "kw") {
                break;
            }
            words.push(self.next().map(|t| t.val.clone()).unwrap_or_default());
            if self.kind() == "op" && self.val() == "(" {
                let args = self.args()?;
                return Ok(Expr::Call(words.join(" "), args));
            }
        }
        Ok(Expr::Id(if words.len() == 1 {
            words.remove(0)
        } else {
            words.join(" ")
        }))
    }
}

fn parse_expr(s: &str) -> Result<Expr, String> {
    let toks = lex(s)?;
    let mut p = Parser { t: &toks, i: 0 };
    let n = p.expr()?;
    if p.kind() != "eof" {
        return Err(format!("trailing {:?} in {s:?}", p.peek(0)));
    }
    Ok(n)
}

// =============================================================== literals

/// Does this 32-bit pattern encode a float a programmer would have typed?
fn decode_float(v: i64) -> Option<f64> {
    let bits = v as u32;
    let f = f32::from_bits(bits) as f64;
    if !f.is_finite() || f == 0.0 {
        return None;
    }
    if !(1e-4 < f.abs() && f.abs() < 1e8) {
        return None;
    }
    for digits in 2..8 {
        let r = round_to(f, digits);
        if (r as f32).to_bits() == bits {
            return Some(r);
        }
    }
    None
}

/// Round half away from zero, as Python's `round` does for these magnitudes.
fn round_to(f: f64, digits: u32) -> f64 {
    let m = 10f64.powi(digits as i32);
    let scaled = f * m;
    (scaled + 0.5).floor() / m
}

/// Python's `repr` for a float, with its 17-digit fallback trimmed.
fn repr_float(f: f64) -> String {
    if f.abs() >= 1e16 || (f != 0.0 && f.abs() < 1e-4) {
        return format!("{f:.10}");
    }
    let s = format!("{f}");
    if s.contains(['e', 'E']) {
        return format!("{f:.10}");
    }
    if s.contains('.') { s } else { format!("{s}.0") }
}

fn fmt_float(f: f64) -> String {
    let mut s = repr_float(f);
    if !s.contains('.') {
        s.push_str(".0");
    }
    s
}

/// Render one numeric literal the way DBP would have written it.
fn lit(n: Num) -> String {
    match n {
        Num::Int(i) if i < 0 => i.to_string(),
        Num::Int(i) => match decode_float(i) {
            Some(f) => fmt_float(f),
            None => i.to_string(),
        },
        Num::Float(f) => fmt_float(f),
    }
}

/// A DWORD that is not a float immediate is a signed integer.
fn norm_int(v: i64) -> i64 {
    if v >= 1 << 31 && decode_float(v).is_none() {
        v - (1 << 32)
    } else {
        v
    }
}

// ============================================================ simplifying

const PURE: &[&str] = &[
    "ABS", "SQR", "INT", "ROUND", "SIN", "COS", "TAN", "ATN", "EXP", "LEN", "LEFT", "RIGHT", "MID",
    "CHR", "ASC", "UPPER", "LOWER", "INSTR", "SPACE", "STR", "VAL", "REPLACE", "POW",
    // `ARRAY INDEX(arr)` reads the array's list cursor; only the commands that
    // move it (`NEXT ARRAY INDEX`, `ARRAY INDEX TO TOP`) have a side effect.
    "ARRAY INDEX",
];

fn cmp_fn(name: &str) -> Option<&'static str> {
    match name.to_uppercase().as_str() {
        "EQUAL" => Some("="),
        "GREATER" => Some(">"),
        "LESS" => Some("<"),
        "NOT EQUAL" => Some("<>"),
        _ => None,
    }
}

fn arith_fn(name: &str) -> Option<&'static str> {
    match name.to_uppercase().as_str() {
        "SUB" => Some("-"),
        "MUL" => Some("*"),
        "DIV" => Some("/"),
        _ => None,
    }
}

fn is_pure(e: &Expr) -> bool {
    match e {
        Expr::Call(n, args) => {
            PURE.contains(&n.to_uppercase().as_str()) && args.iter().all(is_pure)
        }
        Expr::Arr(base, subs) => is_pure(base) && subs.iter().all(is_pure),
        Expr::Fld(base, _) => is_pure(base),
        Expr::Bin(_, l, r) => is_pure(l) && is_pure(r),
        Expr::Un(_, x) => is_pure(x),
        _ => true,
    }
}

fn is_bool(e: &Expr) -> bool {
    match e {
        Expr::Bin(op, l, _) => {
            CMP.contains(&op.as_str()) || matches!(op.as_str(), "AND" | "OR") || is_bool(l)
        }
        Expr::Un(op, x) => *op == "NOT" || is_bool(x),
        Expr::Call(n, _) => cmp_fn(n).is_some(),
        _ => false,
    }
}

fn as_num(e: &Expr) -> Option<Num> {
    match e {
        Expr::Num(n) => Some(*n),
        _ => None,
    }
}

/// Every variable the expression mentions.
fn vars_of(e: Option<&Expr>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect_vars(e, &mut |v| {
        out.insert(v.to_string());
    });
    out
}

fn collect_vars(e: Option<&Expr>, f: &mut impl FnMut(&str)) {
    let Some(e) = e else { return };
    match e {
        Expr::Id(v) => f(v),
        Expr::Fld(b, _) => {
            if let Expr::Id(v) = &**b {
                f(v);
            }
        }
        Expr::Arr(b, subs) => {
            collect_vars(Some(b), f);
            for i in subs {
                collect_vars(Some(i), f);
            }
        }
        Expr::Call(_, args) => args.iter().for_each(|a| collect_vars(Some(a), f)),
        Expr::Bin(_, l, r) => {
            collect_vars(Some(l), f);
            collect_vars(Some(r), f);
        }
        Expr::Un(_, x) => collect_vars(Some(x), f),
        Expr::Num(_) | Expr::Str(_) => {}
    }
}

/// Does `e` mention `name` anywhere? Allocation-free: the substitution and
/// path scans run this for every identifier they meet.
fn mentions(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Id(v) => v == name,
        Expr::Fld(b, _) => matches!(&**b, Expr::Id(v) if v == name),
        Expr::Arr(b, subs) => mentions(b, name) || subs.iter().any(|i| mentions(i, name)),
        Expr::Call(_, args) => args.iter().any(|a| mentions(a, name)),
        Expr::Bin(_, l, r) => mentions(l, name) || mentions(r, name),
        Expr::Un(_, x) => mentions(x, name),
        Expr::Num(_) | Expr::Str(_) => false,
    }
}

/// Fold the operators DBP spells as library calls, and the casts.
fn fold_call(name: &str, args: &[Expr], dst: Option<&str>) -> Option<Expr> {
    let up = name.to_uppercase();
    if up.starts_with("CAST ") {
        let words: Vec<&str> = up.split_whitespace().collect();
        let (src, to) = (*words.get(1)?, *words.last()?);
        let [a] = args else { return None };
        if let Some(v) = as_num(a) {
            return Some(match to {
                "FLOAT" | "DOUBLE" => Expr::Num(Num::Float(v.as_f64())),
                "INTEGER" | "BYTE" if matches!(src, "FLOAT" | "DOUBLE") => {
                    Expr::Num(Num::Int(v.as_f64() as i64))
                }
                _ => a.clone(),
            });
        }
        return match to {
            "DOUBLE" | "FLOAT" => Some(a.clone()),
            "INTEGER" => Some(Expr::Call("INT".to_string(), vec![a.clone()])),
            _ => None,
        };
    }
    if let Some(op) = cmp_fn(&up)
        && args.len() == 2
    {
        return Some(Expr::Bin(
            op.to_string(),
            Box::new(args[0].clone()),
            Box::new(args[1].clone()),
        ));
    }
    if up == "ADD" && args.len() == 2 {
        return Some(Expr::Bin(
            "+".to_string(),
            Box::new(args[0].clone()),
            Box::new(args[1].clone()),
        ));
    }
    if up == "ADD" && args.len() == 3 {
        // ADD(dest, a, b)
        return Some(Expr::Bin(
            "+".to_string(),
            Box::new(args[1].clone()),
            Box::new(args[2].clone()),
        ));
    }
    if let Some(op) = arith_fn(&up)
        && args.len() == 2
    {
        return Some(Expr::Bin(
            op.to_string(),
            Box::new(args[0].clone()),
            Box::new(args[1].clone()),
        ));
    }
    if up == "STR"
        && args.len() == 2
        && let Some(dst) = dst
        && args[0] == Expr::Id(dst.to_string())
        && !matches!(args[1], Expr::Call(..))
    {
        return Some(Expr::Call("STR".to_string(), vec![args[1].clone()]));
    }
    if up == "EQUATE"
        && args.len() == 2
        && let Some(dst) = dst
        && args[0] == Expr::Id(dst.to_string())
    {
        // EquateSS(dest, value) is how the compiler spells a string
        // assignment; a zero second argument is the empty string.
        return Some(match as_num(&args[1]) {
            Some(Num::Int(0)) | Some(Num::Float(0.0)) if args[1] == Expr::Num(Num::Int(0)) => {
                Expr::Str("\"\"".to_string())
            }
            _ => args[1].clone(),
        });
    }
    None
}

/// Evaluate a binary operator over two literals.
fn fold_bin(op: &str, a: Num, b: Num) -> Option<Num> {
    let truth = |v: bool| Num::Int(i64::from(v));
    match op {
        "=" => Some(truth(a == b)),
        "<>" => Some(truth(a != b)),
        "<" => Some(truth(a.as_f64() < b.as_f64())),
        ">" => Some(truth(a.as_f64() > b.as_f64())),
        "<=" => Some(truth(a.as_f64() <= b.as_f64())),
        ">=" => Some(truth(a.as_f64() >= b.as_f64())),
        "AND" => Some(truth(truthy(a) && truthy(b))),
        "OR" => Some(truth(truthy(a) || truthy(b))),
        "+" | "-" | "*" => {
            if let (Num::Int(x), Num::Int(y)) = (a, b) {
                return match op {
                    "+" => x.checked_add(y).map(Num::Int),
                    "-" => x.checked_sub(y).map(Num::Int),
                    _ => x.checked_mul(y).map(Num::Int),
                };
            }
            Some(Num::Float(match op {
                "+" => a.as_f64() + b.as_f64(),
                "-" => a.as_f64() - b.as_f64(),
                _ => a.as_f64() * b.as_f64(),
            }))
        }
        "/" => {
            if b.as_f64() == 0.0 {
                return None;
            }
            if let (Num::Int(x), Num::Int(y)) = (a, b)
                && x % y == 0
            {
                return x.checked_div(y).map(Num::Int);
            }
            Some(Num::Float(a.as_f64() / b.as_f64()))
        }
        _ => None,
    }
}

fn truthy(n: Num) -> bool {
    n.as_f64() != 0.0
}

/// Simplify an expression as far as it will go.
fn simplify(e: &Expr, dst: Option<&str>) -> Expr {
    match e {
        Expr::Call(name, args) => {
            let args: Vec<Expr> = args.iter().map(|a| simplify(a, dst)).collect();
            fold_call(name, &args, dst).unwrap_or_else(|| Expr::Call(name.clone(), args))
        }
        Expr::Bin(op, l, r) => {
            let l = simplify(l, dst);
            let r = simplify(r, dst);
            if let (Some(lv), Some(rv)) = (as_num(&l), as_num(&r))
                && let Some(v) = fold_bin(op, lv, rv)
            {
                return Expr::Num(v);
            }
            // `(x <> 0)` where x is already a test: the zero test is redundant
            if op == "<>" && as_num(&r) == Some(Num::Int(0)) && is_bool(&l) {
                return l;
            }
            if op == "=" && as_num(&r) == Some(Num::Int(0)) && is_bool(&l) {
                return Expr::Un("NOT", Box::new(l));
            }
            // `0 AND x` is false and `1 OR x` is true, whatever x is
            if op == "AND" || op == "OR" {
                let zero = Num::Int(if op == "AND" { 0 } else { 1 });
                if let Some(lv) = as_num(&l)
                    && is_bool(&r)
                {
                    return if lv == zero { Expr::Num(lv) } else { l };
                }
                if let Some(rv) = as_num(&r)
                    && is_bool(&l)
                {
                    return if rv == zero { Expr::Num(rv) } else { l };
                }
                if l == r {
                    return l;
                }
            }
            if l == Expr::Num(Num::Int(0)) {
                match op.as_str() {
                    "+" | "-" => return r,
                    "*" => return Expr::Num(Num::Int(0)),
                    _ => {}
                }
            }
            Expr::Bin(op.clone(), Box::new(l), Box::new(r))
        }
        Expr::Un(op, x) => {
            let x = simplify(x, dst);
            if *op == "NOT"
                && let Some(v) = as_num(&x)
            {
                return Expr::Num(Num::Int(i64::from(!truthy(v))));
            }
            Expr::Un(op, Box::new(x))
        }
        _ => e.clone(),
    }
}

// =============================================================== output

fn prec_of_expr(e: &Expr) -> u8 {
    match e {
        Expr::Bin(op, _, _) => prec_of(op),
        Expr::Un(op, _) => {
            if *op == "-" {
                90
            } else {
                prec_of(op)
            }
        }
        _ => 100,
    }
}

fn render(e: &Expr) -> String {
    render_at(e, 0)
}

fn render_at(e: &Expr, ctx: u8) -> String {
    match e {
        Expr::Num(n) => lit(*n),
        Expr::Str(s) | Expr::Id(s) => s.clone(),
        Expr::Fld(b, f) => format!("{}.{f}", render_at(b, 100)),
        Expr::Arr(b, subs) => {
            let a: Vec<String> = subs.iter().map(|i| render_at(i, 0)).collect();
            format!("{}({})", render_at(b, 100), a.join(", "))
        }
        Expr::Call(n, args) => {
            let a: Vec<String> = args.iter().map(|a| render_at(a, 0)).collect();
            format!("{n}({})", a.join(", "))
        }
        Expr::Un(op, x) => {
            let p = prec_of_expr(e);
            let s = format!(
                "{}{}",
                if *op == "-" { "- " } else { "NOT " },
                render_at(x, p)
            );
            if ctx > p { format!("({s})") } else { s }
        }
        Expr::Bin(op, l, r) => {
            let p = prec_of_expr(e);
            let s = format!("{} {} {}", render_at(l, p), op, render_at(r, p + 1));
            if ctx > p { format!("({s})") } else { s }
        }
    }
}

fn rename(e: Option<&Expr>, table: &HashMap<String, String>) -> Option<Expr> {
    let e = e?;
    Some(match e {
        Expr::Id(v) => Expr::Id(table.get(v).cloned().unwrap_or_else(|| v.clone())),
        Expr::Fld(b, f) => Expr::Fld(Box::new(rename(Some(b), table).unwrap()), f.clone()),
        Expr::Arr(b, subs) => Expr::Arr(
            Box::new(rename(Some(b), table).unwrap()),
            subs.iter()
                .map(|i| rename(Some(i), table).unwrap())
                .collect(),
        ),
        Expr::Call(n, args) => Expr::Call(
            n.clone(),
            args.iter()
                .map(|a| rename(Some(a), table).unwrap())
                .collect(),
        ),
        Expr::Bin(op, l, r) => Expr::Bin(
            op.clone(),
            Box::new(rename(Some(l), table).unwrap()),
            Box::new(rename(Some(r), table).unwrap()),
        ),
        Expr::Un(op, x) => Expr::Un(op, Box::new(rename(Some(x), table).unwrap())),
        _ => e.clone(),
    })
}

/// `arr(3)` names the array `arr`; the subscript is not part of the name.
fn dst_base(dst: &str) -> &str {
    dst.split('(').next().unwrap_or(dst)
}

// ============================================================ statements

/// A statement kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum K {
    If,
    EndIf,
    IfGoto,
    For,
    Next,
    Do,
    Loop,
    Repeat,
    Until,
    Func,
    EndFunc,
    Assign,
    Call,
    Stmt,
    Comment,
    Label,
    Jump,
    Gap,
    Return,
    Nop,
    End,
    EndStmt,
}

/// One statement: a node in the block tree.
#[derive(Clone, Debug)]
struct Stmt {
    id: u32,
    kind: K,
    text: Option<String>,
    expr: Option<Expr>,
    didx: Option<Expr>,
    dst: Option<String>,
    /// `arr(i).field` -- the field offset written after an element access.
    dfield: Option<String>,
    /// The value a `CASE` test compares against, when it is not zero.
    cmp: Option<String>,
    target: Option<String>,
    cc: Option<&'static str>,
    var: Option<String>,
    lo: Option<Expr>,
    hi: Option<Expr>,
    step: Option<Expr>,
    body: Option<Block>,
    els: Option<Block>,
    line: Option<u32>,
    derived: bool,
    lead: Option<String>,
    tail: Option<String>,
    /// The raw instructions of an undecoded region.
    raw: Vec<String>,
}

impl Stmt {
    fn new(kind: K, id: u32) -> Self {
        Self {
            id,
            kind,
            text: None,
            expr: None,
            didx: None,
            dst: None,
            dfield: None,
            cmp: None,
            target: None,
            cc: None,
            var: None,
            lo: None,
            hi: None,
            step: None,
            body: None,
            els: None,
            line: None,
            derived: false,
            lead: None,
            tail: None,
            raw: Vec::new(),
        }
    }
}

/// A statement list. The id makes list identity comparable, which the path
/// scan relies on.
#[derive(Clone, Debug, Default)]
struct Block {
    id: u32,
    stmts: Vec<Stmt>,
}

impl Block {
    fn new(id: u32) -> Self {
        Self {
            id,
            stmts: Vec::new(),
        }
    }
}

/// Hands out statement and block ids.
#[derive(Default)]
struct Ids {
    next: u32,
}

impl Ids {
    fn take(&mut self) -> u32 {
        self.next += 1;
        self.next
    }
}

/// Split `lhs = rhs` on the first `=` that does not start `==`, mirroring the
/// assignment pattern the transcription is written with.
fn split_assign(s: &str) -> Option<(&str, &str)> {
    let b = s.as_bytes();
    if b.first()
        .is_none_or(|c| c.is_ascii_whitespace() || *c == b'=')
    {
        return None;
    }
    let eq = (1..b.len()).find(|&i| b[i] == b'=' && b.get(i + 1) != Some(&b'='))?;
    let rhs = s[eq + 1..].trim_start();
    (!rhs.is_empty()).then_some((&s[..eq], rhs))
}

/// Indentation and keywords to a tree of blocks. Line-number markers are
/// consumed (the last one seen is remembered on the following statement).
fn parse_block(lines: &[String], ids: &mut Ids) -> Block {
    let mut root = Block::new(ids.take());
    // (column that ends the block, the kind that opened it, the path of the
    // statement holding that block). Frames address the tree rather than
    // holding copies of it, so the statements appended land in the very block
    // the opener owns.
    let mut stack: Vec<Frame> = vec![Frame {
        col: -1,
        kind: None,
        path: Vec::new(),
    }];
    let mut line = None;

    for raw in lines {
        if raw.trim().is_empty() {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let s = raw.trim();
        if s.starts_with("'   ") && last_is_gap(&root, &stack) {
            let body = s[4..].trim_end().to_string();
            gap_mut(&mut root, &stack).raw.push(body);
            continue;
        }
        if let Some(Shape::LineMarker(n)) = patterns::shape(s) {
            line = Some(n);
            continue;
        }
        if s.starts_with("' [array index") {
            continue; // the bounds-check error block
        }
        if s.starts_with("' <<< ") {
            // A gap comment sits wherever the decompiler put it, which can be
            // outside the block it interrupts; it must not move the stack.
            let mut n = Stmt::new(K::Gap, ids.take());
            n.text = Some(s.to_string());
            n.line = line;
            container_mut(&mut root, &stack.last().unwrap().path)
                .stmts
                .push(n);
            continue;
        }
        let Some(n) = parse_stmt(s, line, ids) else {
            continue;
        };
        let kind = n.kind;
        while stack.len() > 1 && indent as isize <= stack.last().unwrap().col {
            let popped = stack.pop().unwrap();
            if is_closer(kind) && popped.kind == Some(open_of(kind)) {
                // this closer has already left its own block
                break;
            }
        }
        let parent = stack.last().unwrap().path.clone();
        if is_block(kind) {
            // A FUNCTION body is emitted at the header's own indentation, so
            // accept one column less before closing it.
            let open_col = if kind == K::Func {
                indent as isize - 1
            } else {
                indent as isize
            };
            let mut n = n;
            n.body = Some(Block::new(ids.take()));
            let holder = container_mut(&mut root, &parent);
            let at = holder.stmts.len();
            holder.stmts.push(n);
            let mut path = parent;
            path.push(at);
            stack.push(Frame {
                col: open_col,
                kind: Some(kind),
                path,
            });
        } else if is_closer(kind) {
            // the indentation scan already left the matching block; pop it
            if stack.len() > 1 && stack.last().unwrap().kind == Some(open_of(kind)) {
                stack.pop();
            }
        } else {
            container_mut(&mut root, &parent).stmts.push(n);
        }
    }
    root
}

/// One open block while scanning the source lines.
struct Frame {
    /// Indentation at or below which the block ends.
    col: isize,
    /// The keyword that opened it.
    kind: Option<K>,
    /// Path of statements, from the root, whose `body` this block is.
    path: Vec<usize>,
}

/// The statement list a frame appends to.
fn container_mut<'a>(root: &'a mut Block, path: &[usize]) -> &'a mut Block {
    let mut cur = root;
    for &i in path {
        cur = cur.stmts[i]
            .body
            .as_mut()
            .expect("a frame path only names block openers");
    }
    cur
}

/// The undecoded-region comment the current frame is collecting, if any.
fn gap_mut<'a>(root: &'a mut Block, stack: &[Frame]) -> &'a mut Stmt {
    container_mut(root, &stack.last().unwrap().path)
        .stmts
        .last_mut()
        .expect("a gap frame always has the comment as its last statement")
}

fn last_is_gap(root: &Block, stack: &[Frame]) -> bool {
    let mut cur = root;
    for f in stack {
        let Some(at) = f.path.last() else { continue };
        let Some(b) = cur.stmts.get(*at).and_then(|s| s.body.as_ref()) else {
            return false;
        };
        cur = b;
    }
    cur.stmts.last().is_some_and(|s| s.kind == K::Gap)
}

fn is_block(k: K) -> bool {
    matches!(k, K::If | K::For | K::Do | K::Repeat | K::Func)
}

fn is_closer(k: K) -> bool {
    matches!(k, K::EndIf | K::EndFunc | K::Next | K::Loop | K::Until)
}

fn open_of(k: K) -> K {
    match k {
        K::EndIf => K::If,
        K::EndFunc => K::Func,
        K::Next => K::For,
        K::Loop => K::Do,
        K::Until => K::Repeat,
        other => other,
    }
}

fn parse_stmt(s: &str, line: Option<u32>, ids: &mut Ids) -> Option<Stmt> {
    let id = ids.take();
    if s.starts_with('\'') {
        if matches!(patterns::shape(s), Some(Shape::Banner)) || s == "'" {
            return None; // recovered.dba's own file header
        }
        let mut n = Stmt::new(K::Comment, id);
        n.text = Some(s.to_string());
        n.line = line;
        return Some(n);
    }
    match patterns::shape(s) {
        Some(Shape::Label(target)) => {
            let mut n = Stmt::new(K::Label, id);
            n.target = Some(target.to_string());
            n.line = line;
            return Some(n);
        }
        Some(Shape::For { var, lo, hi, step }) => {
            let mut n = Stmt::new(K::For, id);
            n.var = Some(var.to_string());
            n.lo = parse_expr(lo).ok();
            n.hi = parse_expr(hi).ok();
            n.step = parse_expr(step.unwrap_or("1")).ok();
            n.line = line;
            return Some(n);
        }
        Some(Shape::If {
            cond,
            op,
            value,
            target: Some(target),
        }) => {
            let mut n = Stmt::new(K::IfGoto, id);
            n.expr = parse_expr(cond).ok();
            n.cc = Some(if op == "<>" { "<>" } else { "=" });
            // the `CASE` form compares against its own value
            n.cmp = value.map(str::to_string);
            n.target = Some(target.to_string());
            n.line = line;
            return Some(n);
        }
        Some(Shape::If {
            cond,
            op,
            value,
            target: None,
        }) => {
            let mut n = Stmt::new(K::If, id);
            n.expr = parse_expr(cond).ok();
            n.cc = Some(if op == "<>" { "<>" } else { "=" });
            n.cmp = value.map(str::to_string);
            n.line = line;
            return Some(n);
        }
        Some(Shape::Next) => {
            let mut n = Stmt::new(K::Next, id);
            n.text = Some(s.to_string());
            n.line = line;
            return Some(n);
        }
        Some(Shape::ArrayDst { .. }) | Some(Shape::LineMarker(_)) | Some(Shape::Banner) => {}
        None => {}
    }
    // The block keywords carry no text of their own: what follows them in the
    // output is what they open or close.
    if let Some(kind) = match s {
        "ENDIF" => Some(K::EndIf),
        "LOOP" => Some(K::Loop),
        "DO" => Some(K::Do),
        "REPEAT" => Some(K::Repeat),
        "ENDFUNCTION" => Some(K::EndFunc),
        _ => None,
    } {
        let mut n = Stmt::new(kind, id);
        n.line = line;
        return Some(n);
    }
    if s == "RETURN" || s.starts_with("RETURN ") {
        let mut n = Stmt::new(K::Return, id);
        n.text = Some(s.to_string());
        // the value is read here: it is what the caller receives in EAX
        n.expr = s.strip_prefix("RETURN ").and_then(|v| parse_expr(v).ok());
        n.line = line;
        return Some(n);
    }
    if let Some(rest) = s.strip_prefix("UNTIL ") {
        let mut n = Stmt::new(K::Until, id);
        n.expr = parse_expr(rest).ok();
        n.line = line;
        return Some(n);
    }
    if s.starts_with("FUNCTION ") {
        let mut n = Stmt::new(K::Func, id);
        n.text = Some(s.to_string());
        n.line = line;
        return Some(n);
    }
    if let Some(rest) = s.strip_prefix("GOTO ") {
        let mut n = Stmt::new(K::Jump, id);
        n.target = Some(rest.trim().to_string());
        n.line = line;
        return Some(n);
    }
    if let Some((lhs, rhs)) = split_assign(s) {
        let dst = lhs.trim().trim_end_matches('#').to_string();
        let mut didx = None;
        let mut dfield = None;
        let dst = match patterns::shape(&dst) {
            Some(Shape::ArrayDst { name, index, field }) => {
                didx = parse_expr(index).ok();
                dfield = field.map(str::to_string);
                name.to_string()
            }
            _ => dst,
        };
        let mut n = Stmt::new(K::Assign, id);
        n.dst = Some(dst);
        n.didx = didx;
        n.dfield = dfield;
        n.line = line;
        return match parse_expr(rhs) {
            Ok(e) => {
                n.expr = Some(e);
                Some(n)
            }
            Err(_) => {
                let mut n = Stmt::new(K::Stmt, id);
                n.text = Some(s.to_string());
                n.line = line;
                Some(n)
            }
        };
    }
    match parse_expr(s) {
        // A call keeps only its expression: nothing downstream may read the
        // source line back as text.
        Ok(e @ Expr::Call(..)) => {
            let mut n = Stmt::new(K::Call, id);
            n.expr = Some(e);
            n.line = line;
            Some(n)
        }
        _ => {
            let mut n = Stmt::new(K::Stmt, id);
            n.text = Some(s.to_string());
            n.line = line;
            Some(n)
        }
    }
}

// ================================================================= passes

fn walk(blk: &Block, f: &mut impl FnMut(&Stmt)) {
    for n in &blk.stmts {
        f(n);
        if let Some(b) = &n.body {
            walk(b, f);
        }
        if let Some(b) = &n.els {
            walk(b, f);
        }
    }
}

fn walk_mut(blk: &mut Block, f: &mut impl FnMut(&mut Stmt)) {
    for n in &mut blk.stmts {
        f(n);
        if let Some(b) = &mut n.body {
            walk_mut(b, f);
        }
        if let Some(b) = &mut n.els {
            walk_mut(b, f);
        }
    }
}

/// `(loop nesting depth, statement)` in source order.
fn for_each_depth(blk: &Block, depth: usize, f: &mut impl FnMut(usize, &Stmt)) {
    for n in &blk.stmts {
        f(depth, n);
        let d = if n.kind == K::For { depth + 1 } else { depth };
        for b in [&n.body, &n.els].into_iter().flatten() {
            for_each_depth(b, d, f);
        }
    }
}

fn fold_all(blk: &mut Block) {
    for _ in 0..4 {
        walk_mut(blk, &mut |n| {
            if let Some(e) = n.expr.clone() {
                n.expr = Some(simplify(&e, n.dst.as_deref()));
            }
            if n.kind == K::For {
                if let Some(lo) = n.lo.clone() {
                    n.lo = Some(simplify(&lo, None));
                }
                if let Some(hi) = n.hi.clone() {
                    n.hi = Some(simplify(&hi, None));
                }
                if let Some(st) = n.step.clone() {
                    n.step = Some(simplify(&st, None));
                }
            }
        });
    }
    walk_mut(blk, &mut |n| {
        if matches!(n.kind, K::If | K::IfGoto) && n.expr.as_ref().is_some_and(is_bool) {
            n.cc = None; // `IF x = 1 THEN`, not `x <> 0`
        }
        if n.kind == K::Assign
            && let (Some(e), Some(dst)) = (&n.expr, &n.dst)
            && *e == Expr::Id(dst.clone())
        {
            n.kind = K::Nop;
        }
    });
}

/// Total occurrences of each variable: assignment targets and reads.
#[allow(dead_code)]
/// What one live definition in the inliner's environment looks like.
#[derive(Clone)]
struct Def {
    expr: Expr,
    pure: bool,
    moved: bool,
    owner: u32,
}

/// Copy-propagate the compiler's scratch variables.
///
/// Only values with no side effect are moved. A call -- RND, TIMER, GET
/// OBJECT X, any DLL command -- is left where the compiler put it: it is
/// evaluated at that point in the program, once, and moving it into a reader
/// would both change its value (a clock or a random sequence advances) and
/// risk evaluating it twice.
///
/// A store is visible to the statements that follow it in the same block.
/// Branches, loop heads and function boundaries end the region, and a store
/// inside a nested block never escapes it.
///
/// Pure values (arithmetic over variables) are substituted freely, also inside
/// nested blocks. A value with a side effect -- RND, TIMER, GET ... -- is
/// moved only into a following statement of the same block, and at most once,
/// so no call is evaluated twice and none is skipped by a branch. The
/// original store stays in the output while it still has uses.
struct Inliner {
    env: HashMap<String, Def>,
    /// Statements whose value has been moved into a reader.
    moved: HashSet<u32>,
    /// FOR counters: never substituted.
    loops: BTreeSet<String>,
    written: BTreeSet<String>,
    derived: bool,
}

impl Inliner {
    /// Longest inlined text; beyond this the store stays put.
    const MAXLEN: usize = 200;

    fn new(loops: BTreeSet<String>) -> Self {
        Self {
            env: HashMap::new(),
            moved: HashSet::new(),
            loops,
            written: BTreeSet::new(),
            derived: false,
        }
    }

    fn subst(&mut self, e: Option<&Expr>, depth: u32) -> Option<Expr> {
        let e = e?;
        if depth > 12 {
            return Some(e.clone());
        }
        match e {
            Expr::Id(v) => {
                if self.loops.contains(v) {
                    return Some(e.clone()); // FOR/NEXT counters are live values
                }
                let Some(d) = self.env.get(v) else {
                    return Some(e.clone());
                };
                if d.moved {
                    return Some(e.clone());
                }
                if mentions(&d.expr, v) {
                    return Some(e.clone()); // `x = f(x)` must not become f(f(x))
                }
                if !d.pure {
                    return Some(e.clone()); // only side-effect-free values move
                }
                self.derived = true;
                let s = render(&d.expr);
                if s == *v || s.len() >= Self::MAXLEN {
                    return Some(e.clone());
                }
                if let Some(d) = self.env.get_mut(v) {
                    d.moved = true;
                }
                let owner = self.env.get(v).map(|d| d.owner).unwrap_or(0);
                self.moved.insert(owner);
                let inner = self.env.get(v).map(|d| d.expr.clone());
                if depth < 6 {
                    self.subst(inner.as_ref(), depth + 1)
                } else {
                    inner
                }
            }
            Expr::Fld(b, f) => Some(Expr::Fld(Box::new(self.subst(Some(b), depth)?), f.clone())),
            Expr::Arr(b, subs) => Some(Expr::Arr(
                Box::new(self.subst(Some(b), depth)?),
                subs.iter()
                    .map(|i| self.subst(Some(i), depth))
                    .collect::<Option<Vec<_>>>()?,
            )),
            Expr::Call(n, args) => Some(Expr::Call(
                n.clone(),
                args.iter()
                    .map(|a| self.subst(Some(a), depth))
                    .collect::<Option<Vec<_>>>()?,
            )),
            Expr::Bin(op, l, r) => Some(Expr::Bin(
                op.clone(),
                Box::new(self.subst(Some(l), depth)?),
                Box::new(self.subst(Some(r), depth)?),
            )),
            Expr::Un(op, x) => Some(Expr::Un(op, Box::new(self.subst(Some(x), depth)?))),
            _ => Some(e.clone()),
        }
    }

    /// Drop `name` and every definition computed from it.
    fn invalidate(&mut self, name: &str) {
        self.env.remove(name);
        let stale: Vec<String> = self
            .env
            .iter()
            .filter(|(_, d)| mentions(&d.expr, name))
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            self.env.remove(&k);
        }
    }

    /// Run a nested block.
    ///
    /// The block inherits only the side-effect-free part of the environment,
    /// because a store made outside may not have run. Afterwards every name
    /// the block wrote is invalidated in the enclosing environment too: the
    /// block may have assigned a value the enclosing code then reads.
    fn descend(&mut self, blk: &mut Block) {
        let saved = std::mem::take(&mut self.env);
        self.env = saved
            .iter()
            .filter(|(_, d)| d.pure)
            .map(|(k, d)| (k.clone(), d.clone()))
            .collect();
        let saved_written = std::mem::take(&mut self.written);
        self.block(blk);
        let nested = std::mem::replace(&mut self.written, saved_written);
        self.env = saved;
        // a name the nested block assigned may hold a different value now
        for name in nested {
            self.invalidate(&name);
        }
    }

    fn block(&mut self, blk: &mut Block) {
        let mut prev_impure: BTreeSet<String> = BTreeSet::new();
        for n in &mut blk.stmts {
            match n.kind {
                K::Label | K::Jump | K::IfGoto => {
                    if n.kind == K::IfGoto
                        && let Some(e) = n.expr.clone()
                    {
                        n.expr = self.subst(Some(&e), 0);
                    }
                    self.env.clear();
                    prev_impure.clear();
                }
                K::If => {
                    self.derived = n.derived;
                    if let Some(e) = n.expr.clone() {
                        n.expr = self.subst(Some(&e), 0);
                    }
                    n.derived = self.derived;
                    // A conditional may or may not run, and the decompiler's
                    // block boundaries are not always the program's own;
                    // nothing defined before it can be assumed afterwards.
                    if let Some(b) = &mut n.body {
                        self.descend(b);
                    }
                    if let Some(b) = &mut n.els {
                        self.descend(b);
                    }
                    self.env.clear();
                    prev_impure.clear();
                }
                K::Until => {
                    if let Some(e) = n.expr.clone() {
                        n.expr = self.subst(Some(&e), 0);
                    }
                    self.env.clear();
                    prev_impure.clear();
                }
                K::For | K::Do | K::Repeat => {
                    if n.kind == K::For {
                        if let Some(v) = n.lo.clone() {
                            n.lo = self.subst(Some(&v), 0);
                        }
                        if let Some(v) = n.hi.clone() {
                            n.hi = self.subst(Some(&v), 0);
                        }
                        if let Some(v) = n.step.clone() {
                            n.step = self.subst(Some(&v), 0);
                        }
                    }
                    if let Some(b) = &mut n.body {
                        self.descend(b);
                    }
                    self.env.clear();
                    prev_impure.clear();
                }
                K::Func => {
                    self.env.clear();
                    if let Some(b) = &mut n.body {
                        self.block(b);
                    }
                    self.env.clear();
                    prev_impure.clear();
                }
                K::Assign => {
                    self.derived = n.derived;
                    if let Some(e) = n.expr.clone() {
                        n.expr = self.subst(Some(&e), 0);
                    }
                    if let Some(d) = n.didx.clone() {
                        n.didx = self.subst(Some(&d), 0);
                    }
                    n.derived = self.derived;
                    let dst = n.dst.clone();
                    if let Some(dst) = &dst {
                        self.invalidate(dst);
                        self.written.insert(dst.clone());
                    }
                    prev_impure.clear();
                    if let (Some(dst), None) = (dst.clone(), n.didx.clone())
                        && !dst.starts_with('[')
                    {
                        // A tainted value already carries a call that was
                        // moved here; moving it again would evaluate that call
                        // twice, so it stays where it is.
                        let pure = n.expr.as_ref().is_some_and(is_pure);
                        let expr = n.expr.clone().unwrap_or(Expr::Id(dst.clone()));
                        let owner = n.id;
                        self.env.insert(
                            dst.clone(),
                            Def {
                                expr,
                                pure,
                                moved: false,
                                owner,
                            },
                        );
                        if !pure {
                            prev_impure.insert(dst);
                        }
                    }
                }
                K::Call | K::Stmt => {
                    if let Some(e) = n.expr.clone() {
                        n.expr = self.subst(Some(&e), 0);
                    }
                    prev_impure.clear();
                }
                _ => {}
            }
        }
    }
}

/// The variables a FOR statement owns: they are live values, not
/// temporaries, and must never be substituted away.
fn loop_counters(blk: &Block) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    walk(blk, &mut |n| {
        if n.kind == K::For
            && let Some(v) = &n.var
        {
            out.insert(v.clone());
        }
    });
    out
}

/// Occurrences of each variable that actually read it.
fn stmt_reads(n: &Stmt) -> BTreeSet<String> {
    let mut vs = vars_of(n.expr.as_ref());
    vs.extend(vars_of(n.didx.as_ref()));
    if n.kind == K::For {
        vs.extend(vars_of(n.lo.as_ref()));
        vs.extend(vars_of(n.hi.as_ref()));
        vs.extend(vars_of(n.step.as_ref()));
    }
    vs
}

// ================================================== liveness and dead stores

/// One statement of the flattened control-flow graph.
struct Node {
    kind: K,
    /// Variables read here.
    reads: BTreeSet<String>,
    /// Variables wholly overwritten here.
    defs: BTreeSet<String>,
    /// Successors, filled in while the graph is built.
    succ: Vec<usize>,
    /// Branch target, resolved once every label is known.
    target: Option<String>,
    /// The statement this node came from.
    id: u32,
    /// The store's destination, for the nodes that delete.
    dst: Option<String>,
    /// The statement's own text, for the nodes that name something.
    text: Option<String>,
    /// The statement's expression, for the calls it makes.
    expr: Option<Expr>,
    /// Removing this node cannot change what the program does.
    removable: bool,
}

impl Node {
    fn from_stmt(s: &Stmt) -> Self {
        let mut defs = BTreeSet::new();
        let mut removable = false;
        match s.kind {
            K::Assign => {
                // a store into `arr(i)` or a bracket target is not a whole
                // overwrite of anything
                if s.didx.is_none()
                    && let Some(d) = &s.dst
                    && !d.starts_with('[')
                {
                    defs.insert(dst_base(d).to_string());
                    removable = true;
                }
            }
            K::For => {
                if let Some(v) = &s.var {
                    defs.insert(dst_base(v).to_string());
                }
            }
            _ => {}
        }
        Self {
            kind: s.kind,
            reads: stmt_reads(s),
            defs,
            succ: Vec::new(),
            target: s.target.clone(),
            id: s.id,
            dst: s.dst.clone(),
            text: s.text.clone(),
            expr: s.expr.clone(),
            removable,
        }
    }
}

/// Flatten the block tree into a control-flow graph.
///
/// The tree already carries the structure -- an `IF` node owns its branches, a
/// loop node owns its body -- so the edges come straight out of it: every
/// statement falls through to the next one, a branch adds its target, and the
/// last statement of a body falls through to whatever follows the whole
/// construct. That is exactly the graph a `GOTO`-based liveness analysis needs,
/// and it does not have to guess at block boundaries the way a linear scan did.
fn graph(blk: &Block) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut labels: HashMap<String, usize> = HashMap::new();
    let (_entry, _exits) = build_graph(blk, &mut nodes, &mut labels);
    for n in &mut nodes {
        if matches!(n.kind, K::Jump | K::IfGoto)
            && let Some(j) = n.target.as_ref().and_then(|t| labels.get(t)).copied()
        {
            n.succ.push(j);
        }
    }

    // A call can read and write anything the callee does, and the callee's
    // body is not reachable by fallthrough, so the graph needs an edge from
    // each call site to the function it names. Without it a global written in
    // the main program and read only inside a function would look dead.
    let mut funcs: HashMap<String, usize> = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if n.kind == K::Func
            && let Some(name) = n.text.as_deref().and_then(|t| t.split_whitespace().nth(1))
        {
            funcs.insert(name.to_string(), i);
        }
    }
    if !funcs.is_empty() {
        let mut calls: Vec<(usize, Vec<String>)> = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let mut names: Vec<String> = Vec::new();
            called_names(n.expr.as_ref(), &mut |name| names.push(name.to_string()));
            if let Some(t) = n.text.as_deref()
                && let Some(rest) = t.split_once("CALL ").map(|(_, r)| r)
                && let Some((name, _)) = rest.split_once('(')
                && !name.is_empty()
            {
                names.push(name.trim().to_string());
            }
            if !names.is_empty() {
                calls.push((i, names));
            }
        }
        for (i, names) in calls {
            for name in names {
                if let Some(&f) = funcs.get(&name)
                    && !nodes[i].succ.contains(&f)
                {
                    nodes[i].succ.push(f);
                }
            }
        }
    }
    nodes
}

/// Every function name a call expression names.
fn called_names(e: Option<&Expr>, f: &mut impl FnMut(&str)) {
    let Some(e) = e else { return };
    match e {
        Expr::Call(n, args) => {
            f(n);
            for a in args {
                called_names(Some(a), f);
            }
        }
        Expr::Arr(b, subs) => {
            called_names(Some(b), f);
            for i in subs {
                called_names(Some(i), f);
            }
        }
        Expr::Fld(b, _) => called_names(Some(b), f),
        Expr::Bin(_, l, r) => {
            called_names(Some(l), f);
            called_names(Some(r), f);
        }
        Expr::Un(_, x) => called_names(Some(x), f),
        _ => {}
    }
}

/// Add one block to the graph, returning its entry node and the nodes whose
/// successor is still to be decided (the block's exits).
fn build_graph(
    blk: &Block,
    nodes: &mut Vec<Node>,
    labels: &mut HashMap<String, usize>,
) -> (Option<usize>, Vec<usize>) {
    let mut entry = None;
    let mut exits: Vec<usize> = Vec::new();
    for s in &blk.stmts {
        let idx = nodes.len();
        nodes.push(Node::from_stmt(s));
        for e in exits.drain(..) {
            nodes[e].succ.push(idx);
        }
        if entry.is_none() {
            entry = Some(idx);
        }
        if s.kind == K::Label
            && let Some(t) = &s.target
        {
            labels.entry(t.clone()).or_insert(idx);
        }
        match s.kind {
            K::If => {
                let mut fallthrough = false;
                if let Some(b) = &s.body {
                    let (be, bx) = build_graph(b, nodes, labels);
                    match be {
                        Some(be) => nodes[idx].succ.push(be),
                        None => fallthrough = true,
                    }
                    exits.extend(bx);
                } else {
                    fallthrough = true;
                }
                if let Some(b) = &s.els {
                    let (ee, ex) = build_graph(b, nodes, labels);
                    match ee {
                        Some(ee) => nodes[idx].succ.push(ee),
                        None => fallthrough = true,
                    }
                    exits.extend(ex);
                } else {
                    fallthrough = true;
                }
                if fallthrough {
                    exits.push(idx);
                }
            }
            K::For | K::Do | K::Repeat => {
                // the body runs, then the loop node again
                if let Some(b) = &s.body {
                    let (be, bx) = build_graph(b, nodes, labels);
                    if let Some(be) = be {
                        nodes[idx].succ.push(be);
                    }
                    for x in bx {
                        nodes[x].succ.push(idx);
                    }
                }
                exits.push(idx);
            }
            K::Func => {
                // a function body is entered by its callers, not by the code
                // in front of it; the caller edges are added once the names
                // are known
                if let Some(b) = &s.body {
                    let (be, _bx) = build_graph(b, nodes, labels);
                    if let Some(be) = be {
                        nodes[idx].succ.push(be);
                    }
                }
                // no fallthrough: the next statement is not part of this
            }
            K::Jump | K::Return | K::End | K::EndStmt => {
                // `Jump` gets its only successor from the label table
            }
            _ => exits.push(idx),
        }
    }
    (entry, exits)
}

/// Which nodes can be dropped as dead stores.
///
/// A store is dead when the destination is not live after it -- not read on any
/// path before it is overwritten. The answer comes from a plain backward
/// liveness analysis over the flattened graph, so a loop, a backward `GOTO` or
/// a branch the decompiler could not structure costs nothing: the graph already
/// has those edges. A store with a side effect is dropped only when the inliner
/// moved its value into the reader, so the call still runs exactly once.
fn dead_ids(nodes: &[Node], moved: &HashSet<u32>, impure: &HashSet<u32>) -> HashSet<u32> {
    let mut names: BTreeSet<&str> = BTreeSet::new();
    for n in nodes {
        names.extend(n.reads.iter().map(String::as_str));
        names.extend(n.defs.iter().map(String::as_str));
    }
    let bit: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let words = names.len().div_ceil(64);
    let mask = |set: &BTreeSet<String>| -> Vec<u64> {
        let mut m = vec![0u64; words];
        for v in set {
            if let Some(&b) = bit.get(v.as_str()) {
                m[b / 64] |= 1 << (b % 64);
            }
        }
        m
    };
    let reads: Vec<Vec<u64>> = nodes.iter().map(|n| mask(&n.reads)).collect();
    let defs: Vec<Vec<u64>> = nodes.iter().map(|n| mask(&n.defs)).collect();

    let mut live: Vec<Vec<u64>> = vec![vec![0u64; words]; nodes.len()];
    loop {
        let mut changed = false;
        for i in (0..nodes.len()).rev() {
            let mut set = vec![0u64; words];
            for &s in &nodes[i].succ {
                for (w, v) in live[s].iter().enumerate() {
                    set[w] |= v;
                }
            }
            // a region we cannot read may use anything it likes
            if nodes[i].kind == K::Gap {
                set.iter_mut().for_each(|w| *w = u64::MAX);
            }
            for (w, d) in defs[i].iter().enumerate() {
                set[w] &= !d;
            }
            for (w, r) in reads[i].iter().enumerate() {
                set[w] |= r;
            }
            if set != live[i] {
                live[i] = set;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let live_at = |i: usize, name: &str| -> bool {
        let Some(&b) = bit.get(name) else {
            return false;
        };
        nodes[i].succ.iter().any(|&s| live[s][b / 64] & (1 << (b % 64)) != 0)
    };

    let mut out = HashSet::new();
    for (i, n) in nodes.iter().enumerate() {
        if !n.removable {
            continue;
        }
        let Some(dst) = &n.dst else { continue };
        let base = dst_base(dst);
        // a call's value may move, but only if the inliner moved it
        if impure.contains(&n.id) && !moved.contains(&n.id) {
            continue;
        }
        if !live_at(i, dst) && !live_at(i, base) {
            out.insert(n.id);
        }
    }
    out
}

/// Drop every statement whose id is in `dead`.
fn remove_ids(blk: &Block, dead: &HashSet<u32>) -> Block {
    let mut out = Block {
        id: blk.id,
        stmts: Vec::with_capacity(blk.stmts.len()),
    };
    for n in &blk.stmts {
        if dead.contains(&n.id) {
            continue;
        }
        let mut n = n.clone();
        if let Some(b) = &n.body {
            n.body = Some(remove_ids(b, dead));
        }
        if let Some(b) = &n.els {
            n.els = Some(remove_ids(b, dead));
        }
        out.stmts.push(n);
    }
    out
}

/// Statements whose value is a call that must not be duplicated or moved.
fn impure_ids(blk: &Block) -> HashSet<u32> {
    let mut out = HashSet::new();
    walk(blk, &mut |n| {
        if n.kind == K::Assign && n.expr.as_ref().is_some_and(|e| !is_pure(e)) {
            out.insert(n.id);
        }
    });
    out
}

/// `IF 0 THEN` and `IF 1 THEN`: drop the branch that cannot run.
///
/// A test that folded to a constant is only trustworthy when the fold is
/// sound. It is not, where the decompiler's block structure disagrees with the
/// program's own (an `IF` whose body the decompiler closed early), so a dead
/// branch that was reached by a substitution rather than by a literal is kept,
/// marked, instead of deleted.
fn constant_conditions(blk: &Block) -> Block {
    let mut out = Block {
        id: blk.id,
        stmts: Vec::new(),
    };
    for n in &blk.stmts {
        let mut n = n.clone();
        if let Some(b) = &n.body {
            n.body = Some(constant_conditions(b));
        }
        if let Some(b) = &n.els {
            n.els = Some(constant_conditions(b));
        }
        if matches!(n.kind, K::If | K::IfGoto)
            && let Some(v) = n.expr.as_ref().and_then(as_num)
        {
            let keep = truthy(v);
            if n.kind == K::IfGoto {
                if keep {
                    out.stmts.push(n);
                }
                continue;
            }
            if n.derived {
                // the constant came from copy propagation, not from the
                // source: keep the branch so nothing is lost
                out.stmts.push(n);
                continue;
            }
            if keep {
                out.stmts.extend(n.body.unwrap_or_default().stmts);
            } else {
                out.stmts.extend(n.els.unwrap_or_default().stmts);
            }
            continue;
        }
        out.stmts.push(n);
    }
    out
}

/// The compiler emits the function epilogue -- EarlyEnd and a jump to the
/// program end -- after the final RET. Nothing after RETURN runs, and the pair
/// says nothing the RETURN does not.
fn drop_epilogue(blk: &Block) -> Block {
    let mut out = Block {
        id: blk.id,
        stmts: Vec::new(),
    };
    for (i, n) in blk.stmts.iter().enumerate() {
        let mut n = n.clone();
        if let Some(b) = &n.body {
            n.body = Some(drop_epilogue(b));
        }
        if let Some(b) = &n.els {
            n.els = Some(drop_epilogue(b));
        }
        out.stmts.push(n);
        if blk.stmts[i].kind == K::Return {
            let tail_ok = blk.stmts[i + 1..].iter().all(|t| {
                t.text
                    .as_deref()
                    .is_some_and(|x| x == "EARLY END()" || x == "END")
                    || matches!(t.kind, K::End | K::EndStmt | K::Jump)
            });
            if tail_ok {
                break;
            }
        }
    }
    out
}

/// `a = X; b = a AND Y; IF b = 0 THEN` should read as one condition.
///
/// The inliner already substitutes pure values, so what is left is the tail: a
/// store that feeds a test through one or more intermediate stores. Those
/// intermediates move into the test.
///
/// A store may only be moved into a test it dominates: it has to sit in the
/// same block, or in a block that encloses the test, and never after it.
/// Otherwise `IF x = 5 THEN` would pick up an unrelated earlier store.
fn fold_test_chains(blk: &mut Block) {
    for _ in 0..4 {
        let mut pos: HashMap<u32, usize> = HashMap::new();
        let mut readers: HashMap<String, Vec<Stmt>> = HashMap::new();
        let mut order = 0usize;
        walk(blk, &mut |n| {
            pos.insert(n.id, order);
            order += 1;
            for v in stmt_reads(n) {
                readers.entry(v).or_default().push(n.clone());
            }
        });
        let readers: HashMap<String, Vec<Stmt>> = readers
            .into_iter()
            .map(|(k, v)| (k, dedup_consecutive(v)))
            .collect();
        let mut changed = false;
        collapse_tests(blk, &readers, &pos, &mut changed);
        if !changed {
            break;
        }
    }
}

/// Readers collected per name can be the same statement several times when it
/// mentions a name in more than one slot; the definition search must see it
/// once.
fn dedup_consecutive(v: Vec<Stmt>) -> Vec<Stmt> {
    let mut out: Vec<Stmt> = Vec::with_capacity(v.len());
    for s in v {
        if out.last().is_none_or(|l| l.id != s.id) {
            out.push(s);
        }
    }
    out
}

fn collapse_tests(
    blk: &mut Block,
    readers: &HashMap<String, Vec<Stmt>>,
    pos: &HashMap<u32, usize>,
    changed: &mut bool,
) {
    for n in &mut blk.stmts {
        if n.kind == K::If
            && let Some(e) = n.expr.clone()
        {
            let before = pos.get(&n.id).copied().unwrap_or(0);
            if let Some(cond) = collapse(&e, readers, before, pos, 0) {
                if render(&cond) != render(&e) {
                    // the condition now contains values copied from stores;
                    // a constant here is derived, not written in the source
                    n.derived = true;
                }
                n.expr = Some(cond);
                *changed = true;
            }
        }
        if let Some(b) = &mut n.body {
            collapse_tests(b, readers, pos, changed);
        }
        if let Some(b) = &mut n.els {
            collapse_tests(b, readers, pos, changed);
        }
    }
}

/// Replace names in `e` by the dominating store that defines them, as long as
/// that store is a side-effect-free expression.
fn collapse(
    e: &Expr,
    readers: &HashMap<String, Vec<Stmt>>,
    before: usize,
    pos: &HashMap<u32, usize>,
    depth: u32,
) -> Option<Expr> {
    if depth > 40 {
        return Some(e.clone());
    }
    match e {
        Expr::Id(v) => {
            let empty: Vec<Stmt> = Vec::new();
            let src = sole_def(readers.get(v).unwrap_or(&empty), v)?;
            let se = src.expr.as_ref()?;
            if !is_pure(se) {
                return Some(e.clone());
            }
            if pos.get(&src.id).copied().unwrap_or(1 << 30) >= before {
                return Some(e.clone()); // the store does not dominate the test
            }
            if mentions(se, v) {
                return Some(e.clone()); // `x = f(x)` must not become f(f(x))
            }
            collapse(se, readers, before, pos, depth + 1)
        }
        Expr::Bin(op, l, r) => Some(Expr::Bin(
            op.clone(),
            Box::new(collapse(l, readers, before, pos, depth)?),
            Box::new(collapse(r, readers, before, pos, depth)?),
        )),
        Expr::Un(op, x) => Some(Expr::Un(
            op,
            Box::new(collapse(x, readers, before, pos, depth)?),
        )),
        _ => Some(e.clone()),
    }
}

/// The single store that writes this name, if that is unambiguous.
fn sole_def<'a>(nodes: &'a [Stmt], name: &str) -> Option<&'a Stmt> {
    let defs: Vec<&Stmt> = nodes
        .iter()
        .filter(|n| n.kind == K::Assign && n.dst.as_deref() == Some(name) && n.didx.is_none())
        .collect();
    (defs.len() == 1).then(|| defs[0])
}

fn flow_fixes(blk: &mut Block, prog_end: &str) {
    for n in &mut blk.stmts {
        if let Some(b) = &mut n.body {
            flow_fixes(b, prog_end);
        }
        if let Some(b) = &mut n.els {
            flow_fixes(b, prog_end);
        }
        if n.kind == K::Jump && n.target.as_deref() == Some(prog_end) {
            n.kind = K::End;
        }
        if n.kind == K::IfGoto && n.target.as_deref() == Some(prog_end) {
            n.kind = K::EndStmt;
        }
    }
}

/// `IF c THEN ... GOTO L ENDIF / ... / L:` is an IF/ELSE.
fn fold_forward_gotos(blk: &Block, ids: &mut Ids) -> Block {
    let mut refs: BTreeMap<String, usize> = BTreeMap::new();
    walk(blk, &mut |n| {
        if matches!(n.kind, K::Jump | K::IfGoto)
            && let Some(t) = &n.target
        {
            *refs.entry(t.clone()).or_default() += 1;
        }
    });
    let mut out = Block {
        id: blk.id,
        stmts: Vec::new(),
    };
    let mut i = 0;
    while i < blk.stmts.len() {
        let n = &blk.stmts[i];
        if n.kind == K::If
            && let Some(body) = &n.body
            && body.stmts.last().is_some_and(|l| l.kind == K::Jump)
        {
            let tgt = body.stmts.last().unwrap().target.clone();
            let mut j = None;
            for (k, rest) in blk.stmts.iter().enumerate().skip(i + 1) {
                if rest.kind == K::Label {
                    if rest.target == tgt {
                        j = Some(k);
                    }
                    break;
                }
                if rest.kind == K::Func {
                    break;
                }
            }
            if let Some(j) = j
                && tgt.as_ref().is_some_and(|t| refs.get(t) == Some(&1))
            {
                let between = &blk.stmts[i + 1..j];
                if !between
                    .iter()
                    .any(|x| matches!(x.kind, K::Label | K::Jump | K::IfGoto | K::Func))
                {
                    // `IF c THEN <then> ENDIF <else> L:` == IF NOT c THEN
                    // <else> ELSE <then> ENDIF
                    let mut m = Stmt::new(K::If, ids.take());
                    m.expr = n.expr.clone();
                    m.cc = Some(if matches!(n.cc, None | Some("<>")) {
                        "="
                    } else {
                        "<>"
                    });
                    m.body = Some(Block {
                        id: ids.take(),
                        stmts: between.to_vec(),
                    });
                    m.els = Some(Block {
                        id: ids.take(),
                        stmts: body.stmts[..body.stmts.len() - 1].to_vec(),
                    });
                    m.line = n.line;
                    out.stmts.push(m);
                    i = j + 1;
                    continue;
                }
            }
        }
        out.stmts.push(n.clone());
        i += 1;
    }
    out
}

/// `v = a - s` immediately in front of `FOR v = a TO b` is the lowering of the
/// loop's initialiser; the FOR statement carries it already.
fn drop_for_seed(blk: &mut Block) -> Block {
    for n in &mut blk.stmts {
        if let Some(b) = &mut n.body {
            *b = drop_for_seed(b);
        }
        if let Some(b) = &mut n.els {
            *b = drop_for_seed(b);
        }
    }
    let out = &blk.stmts;
    let mut res = Block {
        id: blk.id,
        stmts: Vec::new(),
    };
    let mut i = 0;
    while i < out.len() {
        if i + 1 < out.len()
            && out[i].kind == K::Assign
            && out[i + 1].kind == K::For
            && seed_matches(&out[i], &out[i + 1])
        {
            // drop the seed only; the FOR itself is the next statement
            i += 1;
            continue;
        }
        res.stmts.push(out[i].clone());
        i += 1;
    }
    res
}

/// `v = a - s` in front of `FOR v = a TO b` is the loop initialiser the
/// lowering emits; compare numerically, the fold pass may already have
/// evaluated it.
fn seed_matches(stmt: &Stmt, loop_: &Stmt) -> bool {
    if stmt.dst != loop_.var || stmt.expr.is_none() {
        return false;
    }
    let (Some(e), Some(lo), Some(st)) = (
        stmt.expr.as_ref().and_then(as_num),
        loop_.lo.as_ref().and_then(as_num),
        loop_.step.as_ref().and_then(as_num),
    ) else {
        return false;
    };
    e.as_f64() == lo.as_f64() - st.as_f64()
}

// ================================================================= naming
//
// Names come from what a slot is *used* for, not from the (unrecoverable)
// identifier the programmer typed:
//
//   i, j, k ...  FOR/NEXT counters, by how deeply the loops nest
//   arrN         indexed arr(i), or dimensioned with DIM ARRAY
//   recN         a user-type record, read as rec.field
//   pathN        a file-system path string
//   strN         any other string
//   tN           a timestamp: written from TIMER(), read as TIMER() - tN
//   fN           a float (converted through CAST ... TO FLOAT/DOUBLE)
//   nN           a plain integer
//   pN / vN      a function's parameters / locals (frame slots, per function)
//
// Global slots are named once for the whole program. Frame slots live inside
// one function and are named per function: the same displacement means
// different things in different functions.

/// Commands that take a file-system path. FILE EXIST / MAKE FILE and friends
/// only read the path -- their result is a flag, not a string -- so they are
/// listed separately and do not make the *slot* a string.
const PATH_CMD: &[&str] = &[
    "OPEN FILE",
    "WRITE STRING",
    "READ STRING",
    "GET FILE NAME",
    "EXECUTE FILE",
    "SET DIR",
    "GET DIR",
    "SYSTEM COMMON APP DATA DIR",
    "SYSTEM DESKTOP DIR",
    "SYSTEM USER DIR",
    "CHDIR",
    "RENAME FILE",
    "COPY FILE",
    "FIND FILE",
];

const STRFN: &[&str] = &[
    "LEN", "UPPER", "LOWER", "LEFT", "RIGHT", "MID", "INSTR", "STR", "VAL", "SPACE", "CHR",
    "REVERSE",
];

/// What one variable slot is used for.
#[derive(Clone, Copy, Default)]
struct Roles {
    /// `arr(i)`
    arr: u32,
    /// `rec.field`
    rec: u32,
    /// string value
    str: u32,
    /// string handed to a file command
    path: u32,
    /// float conversion
    float: u32,
    /// assigned `TIMER()`
    time_written: u32,
    /// read as `TIMER() - v`
    time_read: u32,
}

/// The global roles, mutated as statements are scanned.
type RoleMap = HashMap<String, Roles>;

/// Note what one expression says about the slot it is stored in.
///
/// `self_var` is the slot the whole expression belongs to; `owner` is carried
/// down the tree so a literal inside a call's arguments is attributed to that
/// argument rather than to the slot the call's result lands in.
fn note_roles(e: Option<&Expr>, side: char, roles: &mut RoleMap, self_var: &str, owner: &str) {
    let Some(e) = e else { return };
    match e {
        Expr::Id(_) => {}
        Expr::Arr(b, subs) => {
            if let Expr::Id(v) = &**b {
                roles.entry(v.clone()).or_default().arr += 1;
            }
            for i in subs {
                note_roles(Some(i), side, roles, self_var, owner);
            }
        }
        Expr::Fld(b, _) => {
            if let Expr::Id(v) = &**b {
                roles.entry(v.clone()).or_default().rec += 1;
            }
        }
        Expr::Call(n, args) => {
            let u = n.to_uppercase();
            if u == "DIM ARRAY" {
                for a in args {
                    if let Expr::Id(v) = a {
                        roles.entry(v.clone()).or_default().arr += 1;
                    }
                }
            }
            if u.starts_with("CAST")
                && matches!(u.split_whitespace().last(), Some("FLOAT" | "DOUBLE"))
            {
                for a in args {
                    if let Expr::Id(v) = a {
                        roles.entry(v.clone()).or_default().float += 1;
                    }
                }
            }
            if PATH_CMD.contains(&u.as_str()) {
                for a in args {
                    if let Expr::Id(v) = a {
                        roles.entry(v.clone()).or_default().path += 1;
                    }
                }
            }
            if STRFN.contains(&u.as_str()) || u == "ADD" {
                for a in args {
                    if let Expr::Id(v) = a {
                        roles.entry(v.clone()).or_default().str += 1;
                    }
                }
            }
            if u == "TIMER" && side == 'w' {
                roles.entry(self_var.to_string()).or_default().time_written += 1;
            }
            for a in args {
                note_roles(Some(a), side, roles, self_var, "arg");
            }
        }
        Expr::Bin(op, l, r) => {
            // `TIMER() - t` makes `t` the reference the clock is compared
            // against; the TIMER() operand itself is not a timestamp slot.
            if matches!(op.as_str(), "+" | "-")
                && side == 'r'
                && **l == Expr::Call("TIMER".to_string(), Vec::new())
                && let Expr::Id(v) = &**r
            {
                roles.entry(v.clone()).or_default().time_read += 1;
            }
            note_roles(Some(l), side, roles, self_var, owner);
            note_roles(Some(r), side, roles, self_var, owner);
        }
        Expr::Un(_, x) => note_roles(Some(x), side, roles, self_var, owner),
        Expr::Str(_) => {
            // only a literal the slot itself builds makes it a string; the
            // owner is "arg" from the top, so this needs a slot-owned literal
            if owner == "self" {
                roles.entry(self_var.to_string()).or_default().str += 1;
            }
        }
        Expr::Num(_) => {}
    }
}

fn stmt_vars(n: &Stmt) -> BTreeSet<String> {
    let mut vs = vars_of(n.expr.as_ref());
    vs.extend(vars_of(n.didx.as_ref()));
    if n.kind == K::For {
        vs.extend(vars_of(n.lo.as_ref()));
        vs.extend(vars_of(n.hi.as_ref()));
        if let Some(v) = &n.var {
            vs.insert(v.clone());
        }
    }
    if let Some(d) = &n.dst {
        vs.insert(d.clone());
    }
    vs
}

fn scan_roles(blk: &Block, roles: &mut RoleMap) {
    for n in &blk.stmts {
        for v in stmt_vars(n) {
            roles.entry(v.clone()).or_default();
            let side = if n.dst.as_deref() == Some(v.as_str()) {
                'w'
            } else {
                'r'
            };
            note_roles(n.expr.as_ref(), side, roles, &v, "arg");
            if n.didx.is_some() {
                note_roles(n.didx.as_ref(), 'r', roles, &v, "arg");
            }
            if n.kind == K::For {
                note_roles(n.lo.as_ref(), 'r', roles, &v, "arg");
                note_roles(n.hi.as_ref(), 'r', roles, &v, "arg");
            }
        }
        if let Some(b) = &n.body {
            scan_roles(b, roles);
        }
        if let Some(b) = &n.els {
            scan_roles(b, roles);
        }
    }
}

fn role_of(r: &Roles) -> &'static str {
    if r.arr > 0 {
        return "arr";
    }
    if r.rec > 0 && r.str == 0 {
        return "rec";
    }
    if r.path > 0 {
        return "path";
    }
    if r.str > 0 {
        return "str";
    }
    if r.time_written > 0 || r.time_read > 0 {
        return "t";
    }
    if r.float > 0 {
        return "f";
    }
    "n"
}

fn describe(r: &Roles, base: &str) -> String {
    let mut bits: Vec<&str> = Vec::new();
    if r.arr > 0 {
        bits.push("array");
    }
    if r.rec > 0 {
        bits.push("user-type record");
    }
    if r.path > 0 {
        bits.push("file path");
    }
    if r.str > 0 && base != "str" {
        bits.push("string");
    }
    if r.time_written > 0 {
        bits.push("TIMER() snapshot");
    }
    if r.time_read > 0 {
        bits.push("elapsed-time reference");
    }
    if r.float > 0 && base != "f" {
        bits.push("float");
    }
    if bits.is_empty() {
        "integer".to_string()
    } else {
        bits.join(", ")
    }
}

/// Hands out unique names per base.
#[derive(Default)]
struct Namer {
    taken: HashSet<String>,
    pools: BTreeMap<&'static str, u32>,
}

impl Namer {
    fn new(taken: &HashSet<String>) -> Self {
        Self {
            taken: taken.clone(),
            pools: BTreeMap::new(),
        }
    }

    fn take(&mut self, base: &'static str) -> String {
        loop {
            let n = self.pools.entry(base).or_default();
            *n += 1;
            let name = format!("{base}{}", *n);
            if !self.taken.contains(&name) {
                self.taken.insert(name.clone());
                return name;
            }
        }
    }

    /// A loop counter: one letter, suffixed only if the letter is taken.
    fn counter(&mut self, base: char) -> String {
        let mut name = base.to_string();
        let mut k = 2;
        while self.taken.contains(&name) {
            name = format!("{base}{k}");
            k += 1;
        }
        self.taken.insert(name.clone());
        name
    }
}

/// `(global table, per-function tables, legend)`.
type Tables = (
    HashMap<String, String>,
    HashMap<String, HashMap<String, String>>,
    Legend,
);

fn naming(blk: &Block) -> Tables {
    let mut roles: RoleMap = RoleMap::new();
    scan_roles(blk, &mut roles);

    let mut legend: Legend = Vec::new();
    let mut gtab: HashMap<String, String> = HashMap::new();
    let mut nm = Namer::default();

    // FOR/NEXT counters of the main program, by how deeply the loops nest.
    // A slot reused by several loops keeps one name: it is one variable.
    const LETTERS: &[u8] = b"ijklmnopqrstuvwxyz";
    for_each_depth(blk, 0, &mut |depth, n| {
        if n.kind != K::For {
            return;
        }
        let (Some(v), false) = (
            n.var.as_deref(),
            n.var.as_deref().is_some_and(|v| gtab.contains_key(v)),
        ) else {
            return;
        };
        if !v.starts_with("var$") {
            return;
        }
        let base = LETTERS[depth.min(LETTERS.len() - 1)] as char;
        let name = nm.counter(base);
        gtab.insert(v.to_string(), name.clone());
        legend.push((
            name,
            v.to_string(),
            format!(
                "FOR/NEXT counter (source line {})",
                n.line.map_or("?".into(), |l| l.to_string())
            ),
        ));
    });

    // global variable-space slots
    let mut gvars: Vec<String> = roles
        .keys()
        .filter(|k| k.starts_with("var$"))
        .cloned()
        .collect();
    gvars.sort_unstable();
    for v in gvars {
        if gtab.contains_key(&v) {
            continue;
        }
        let base = role_of(&roles[&v]);
        let name = nm.take(base);
        let why = describe(&roles[&v], base);
        gtab.insert(v.clone(), name.clone());
        legend.push((name, v, why));
    }

    // function frame slots, one table per function
    let mut tables: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut funcs: Vec<Stmt> = Vec::new();
    walk(blk, &mut |n| {
        if n.kind == K::Func {
            funcs.push(n.clone());
        }
    });
    for f in funcs {
        let fname = f
            .text
            .as_deref()
            .and_then(|t| t.split_whitespace().nth(1))
            .unwrap_or("")
            .to_string();
        let mut local = Namer::new(&nm.taken);
        let mut table: HashMap<String, String> = HashMap::new();
        let empty = Block::new(u32::MAX);
        let body = f.body.as_ref().unwrap_or(&empty);
        for_each_depth(body, 0, &mut |depth, n| {
            if n.kind != K::For {
                return;
            }
            let (Some(v), false) = (
                n.var.as_deref(),
                n.var.as_deref().is_some_and(|v| table.contains_key(v)),
            ) else {
                return;
            };
            if !v.starts_with("loc") {
                return;
            }
            let base = LETTERS[depth.min(LETTERS.len() - 1)] as char;
            let name = local.counter(base);
            table.insert(v.to_string(), name.clone());
            legend.push((name, v.to_string(), format!("counter of {fname}")));
        });
        let mut fvars: BTreeSet<String> = BTreeSet::new();
        walk(body, &mut |n| fvars.extend(stmt_vars(n)));
        for v in fvars
            .iter()
            .filter(|v| v.starts_with("loc") || v.starts_with("par"))
        {
            if table.contains_key(v) {
                continue;
            }
            let name = if v.starts_with("par") {
                let n = local.take("p");
                legend.push((n.clone(), v.clone(), format!("parameter of {fname}")));
                n
            } else {
                let n = local.take("v");
                legend.push((n.clone(), v.clone(), format!("local of {fname}")));
                n
            };
            table.insert(v.clone(), name);
        }
        tables.insert(fname, table);
    }
    (gtab, tables, legend)
}

fn apply_names(
    blk: &mut Block,
    table: &HashMap<String, String>,
    tables: &HashMap<String, HashMap<String, String>>,
) {
    for n in &mut blk.stmts {
        if let Some(e) = &n.expr {
            n.expr = rename(Some(e), table);
        }
        if let Some(d) = &n.didx {
            n.didx = rename(Some(d), table);
        }
        n.lo = n.lo.as_ref().and_then(|e| rename(Some(e), table));
        n.hi = n.hi.as_ref().and_then(|e| rename(Some(e), table));
        n.step = n.step.as_ref().and_then(|e| rename(Some(e), table));
        if let Some(d) = &n.dst {
            n.dst = Some(table.get(d).cloned().unwrap_or_else(|| d.clone()));
        }
        if let Some(v) = &n.var {
            n.var = Some(table.get(v).cloned().unwrap_or_else(|| v.clone()));
        }
        if let Some(b) = &mut n.body {
            let mut local = table.clone();
            if n.kind == K::Func
                && let Some(fname) = n.text.as_deref().and_then(|t| t.split_whitespace().nth(1))
                && let Some(t) = tables.get(fname)
            {
                local.extend(t.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            apply_names(b, &local, tables);
        }
        if let Some(b) = &mut n.els {
            apply_names(b, table, tables);
        }
    }
}

// ================================================================ emitting

/// A run of at least this many initialisers is bracketed as one block.
const INIT_RUN: usize = 8;

fn is_init(n: &Stmt) -> bool {
    n.kind == K::Assign
        && n.dst.is_some()
        && n.expr.is_some()
        && patterns::is_init_text(&format!(
            "{} = {}",
            n.dst.as_deref().unwrap_or(""),
            render(n.expr.as_ref().unwrap())
        ))
}

/// Bracket a long run of `x = 0` -- the program's initialiser block -- so it
/// reads as one thing instead of a thousand lines.
fn mark_inits(blk: &mut Block) {
    let mut i = 0;
    while i < blk.stmts.len() {
        if !is_init(&blk.stmts[i]) {
            i += 1;
            continue;
        }
        let j = blk.stmts[i..].iter().take_while(|n| is_init(n)).count() + i;
        if j - i >= INIT_RUN {
            blk.stmts[i].lead = Some(format!("' --- global initialisation: {} slots ---", j - i));
            blk.stmts[j - 1].tail = Some("' --- end of initialisation ---".to_string());
        }
        i = j;
    }
    for n in &mut blk.stmts {
        if let Some(b) = &mut n.body {
            mark_inits(b);
        }
        if let Some(b) = &mut n.els {
            mark_inits(b);
        }
    }
}

fn emit(blk: &Block, out: &mut Vec<String>, depth: usize) {
    let ind = "  ".repeat(depth);
    for n in &blk.stmts {
        if let Some(l) = &n.lead {
            out.push(format!("{ind}{l}"));
        }
        if let Some(t) = &n.tail {
            out.push(format!("{ind}{t}"));
        }
        match n.kind {
            // the block headers emit their own ENDIF / NEXT / LOOP
            K::Next | K::Loop | K::EndIf => {}
            K::Return => match &n.expr {
                Some(e) => out.push(format!("{ind}RETURN {}", render(e))),
                None => out.push(format!("{ind}RETURN")),
            },
            K::Nop | K::Comment | K::Stmt | K::EndFunc => {
                if let Some(t) = &n.text {
                    out.push(format!("{ind}{t}"));
                }
            }
            K::Gap => {
                let body = n.raw.join(" ; ");
                let words: Vec<&str> = n.text.as_deref().unwrap_or("").split_whitespace().collect();
                let count = words.get(2).copied().unwrap_or("");
                out.push(format!("{ind}' [undecoded {count}] {body}"));
            }
            K::Label => out.push(format!("{ind}{}:", n.target.as_deref().unwrap_or(""))),
            K::Jump => out.push(format!("{ind}GOTO {}", n.target.as_deref().unwrap_or(""))),
            K::End | K::EndStmt => out.push(format!("{ind}END")),
            K::IfGoto => {
                let test = render(n.expr.as_ref().unwrap_or(&Expr::Num(Num::Int(0))));
                let cond = match &n.cmp {
                    Some(v) => format!(
                        "{test} {} {v}",
                        if n.cc == Some("<>") { "<>" } else { "=" }
                    ),
                    None => format!("{test}{}", cc_suffix(n.cc)),
                };
                out.push(format!(
                    "{ind}IF {cond} THEN GOTO {}",
                    n.target.as_deref().unwrap_or("")
                ));
            }
            K::If => {
                let c = cc_suffix(n.cc);
                let text = render(n.expr.as_ref().unwrap_or(&Expr::Num(Num::Int(0))));
                if n.derived && n.expr.as_ref().and_then(as_num).is_some() {
                    out.push(format!("{ind}' (this test folded to a constant from a"));
                    out.push(format!("{ind}'  copied value; the decompiler's block"));
                    out.push(format!("{ind}'  boundaries here do not match the"));
                    out.push(format!("{ind}'  program's, so the branch is kept)"));
                }
                out.push(format!("{ind}IF {text}{c} THEN"));
                if let Some(b) = &n.body {
                    emit(b, out, depth + 1);
                }
                if let Some(b) = &n.els {
                    out.push(format!("{ind}ELSE"));
                    emit(b, out, depth + 1);
                }
                out.push(format!("{ind}ENDIF"));
            }
            K::For => {
                let step = n
                    .step
                    .as_ref()
                    .map(render)
                    .unwrap_or_else(|| "1".to_string());
                let s = if step == "1" {
                    String::new()
                } else {
                    format!(" STEP {step}")
                };
                out.push(format!(
                    "{ind}FOR {} = {} TO {}{s}",
                    n.var.as_deref().unwrap_or(""),
                    n.lo.as_ref().map(render).unwrap_or_default(),
                    n.hi.as_ref().map(render).unwrap_or_default()
                ));
                if let Some(b) = &n.body {
                    emit(b, out, depth + 1);
                }
                out.push(format!("{ind}NEXT {}", n.var.as_deref().unwrap_or("")));
            }
            K::Do => {
                out.push(format!("{ind}DO"));
                if let Some(b) = &n.body {
                    emit(b, out, depth + 1);
                }
                out.push(format!("{ind}LOOP"));
            }
            K::Repeat => {
                out.push(format!("{ind}REPEAT"));
                if let Some(b) = &n.body {
                    emit(b, out, depth + 1);
                }
                out.push(format!(
                    "{ind}UNTIL {}",
                    render(n.expr.as_ref().unwrap_or(&Expr::Num(Num::Int(0))))
                ));
            }
            K::Until => out.push(format!(
                "{ind}UNTIL {}",
                render(n.expr.as_ref().unwrap_or(&Expr::Num(Num::Int(0))))
            )),
            K::Func => {
                out.push(format!("{ind}{}", n.text.as_deref().unwrap_or("")));
                if let Some(b) = &n.body {
                    emit(b, out, depth + 1);
                }
                out.push(format!("{ind}ENDFUNCTION"));
            }
            K::Assign => {
                let lhs = match &n.didx {
                    None => n.dst.clone().unwrap_or_default(),
                    Some(i) => format!(
                        "{}({}){}",
                        n.dst.as_deref().unwrap_or(""),
                        render(i),
                        n.dfield.as_deref().unwrap_or("")
                    ),
                };
                out.push(format!(
                    "{ind}{lhs} = {}",
                    render(n.expr.as_ref().unwrap_or(&Expr::Num(Num::Int(0))))
                ));
            }
            K::Call => out.push(format!(
                "{ind}{}",
                render(n.expr.as_ref().unwrap_or(&Expr::Num(Num::Int(0))))
            )),
        }
    }
}

fn cc_suffix(cc: Option<&str>) -> &'static str {
    match cc {
        None => "",
        Some("<>") => " <> 0",
        _ => " = 0",
    }
}

// =================================================================== main

/// Rewrite `transcription` into readable BASIC, returning the lines and the
/// legend of inferred names.
/// `prog_end` is the label of the program epilogue -- `JMP` there is `END`.
pub fn tidy(transcription: &str, prog_end: Option<&str>) -> (Vec<String>, Legend) {
    let lines: Vec<String> = transcription.split('\n').map(str::to_string).collect();
    let mut ids = Ids::default();
    let mut nodes = parse_block(&lines, &mut ids);
    fold_all(&mut nodes);
    nodes = drop_for_seed(&mut nodes); // undo the loop lowering first

    let loops = loop_counters(&nodes);
    let mut moved: HashSet<u32> = HashSet::new();
    for _ in 0..12 {
        let mut inl = Inliner::new(loops.clone());
        inl.block(&mut nodes);
        moved.extend(inl.moved.iter().copied());
        fold_all(&mut nodes);
        let impure = impure_ids(&nodes);
        let dead = dead_ids(&graph(&nodes), &moved, &impure);
        nodes = remove_ids(&nodes, &dead);
    }

    fold_test_chains(&mut nodes);
    nodes = constant_conditions(&nodes);
    nodes = drop_epilogue(&nodes);
    nodes = fold_forward_gotos(&nodes, &mut ids);
    flow_fixes(&mut nodes, prog_end.unwrap_or(""));
    fold_all(&mut nodes); // the swapped branches changed shape
    fold_test_chains(&mut nodes);
    nodes = constant_conditions(&nodes);
    let impure = impure_ids(&nodes);
    let dead = dead_ids(&graph(&nodes), &moved, &impure);
    nodes = remove_ids(&nodes, &dead);

    let (gtab, tables, legend) = naming(&nodes);
    mark_inits(&mut nodes);
    apply_names(&mut nodes, &gtab, &tables);

    let mut out = Vec::new();
    emit(&nodes, &mut out, 0);
    (out, legend)
}

/// The legend: what each inferred name was, and what its use says.
///
/// Only the names whose use says something are listed; the rest are counted,
/// because they are a long list of indistinguishable integer slots.
pub fn write_legend(out: &mut String, legend: &Legend, nfunc: usize) {
    out.push_str("'\n' Variable legend -- the offset in the DBP variable space\n");
    out.push_str("' each name was given, and what its use says about it.  Names\n");
    out.push_str("' are assigned in order of first appearance, so n42 is simply\n");
    out.push_str("' the 42nd such slot, not a count of anything.\n");
    out.push_str("'\n");
    let mut plain = 0usize;
    let mut counters: BTreeMap<&str, usize> = BTreeMap::new();
    let mut locals_n = 0usize;
    for (name, old, why) in legend {
        if why == "integer" {
            plain += 1;
            continue;
        }
        if let Some(rest) = why.strip_prefix("counter of ") {
            *counters.entry(rest).or_default() += 1;
            continue;
        }
        if why.starts_with("local of") || why.starts_with("parameter of") {
            locals_n += 1;
            continue;
        }
        let _ = writeln!(out, "'   {name:<8} = {old:<12} {why}");
    }
    out.push_str("'\n");
    if !counters.is_empty() {
        let _ = write!(
            out,
            "'   every function that loops names its counter i, j, k by\n'   nesting depth; {} of the {nfunc} do.\n",
            counters.len()
        );
    }
    let _ = write!(
        out,
        "'   {plain} further integer slots (n4..) with no distinguishing\n'   use, and {locals_n} function frame slots (p1.., v1..) named per\n'   function.  All are listed in recovered.dba by offset.\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_are_retyped_the_way_a_programmer_would_write_them() {
        assert_eq!(lit(Num::Int(0x3F800000)), "1.0");
        assert_eq!(lit(Num::Int(0x3E99999A)), "0.3");
        assert_eq!(lit(Num::Int(5)), "5");
        // A DWORD with the high bit set is a negative integer, not a float.
        assert_eq!(lit(Num::Int(norm_int(0xFFFF_F830))), "-2000");
    }

    #[test]
    fn comparison_chains_associate_to_the_left() {
        let e = parse_expr("a > b = 0").unwrap();
        let Expr::Bin(op, l, _) = &e else {
            panic!("expected a binary node")
        };
        assert_eq!(op, "=");
        assert!(matches!(&**l, Expr::Bin(op, _, _) if op == ">"));
        assert_eq!(render(&e), "a > b = 0");
    }

    #[test]
    fn dbp_operator_calls_become_operators() {
        let e = simplify(&parse_expr("EQUAL(a, b)").unwrap(), None);
        assert_eq!(render(&e), "a = b");
        // ADD(dest, a, b) computes `a + b`; the constant fold lands on the
        // next pass, exactly as it does in the reference pass.
        let e = simplify(&parse_expr("ADD(dst, 1, 2)").unwrap(), None);
        assert_eq!(render(&e), "1 + 2");
        assert_eq!(render(&simplify(&e, None)), "3");
        let e = simplify(&parse_expr("CAST INTEGER TO DOUBLE(a)").unwrap(), None);
        assert_eq!(render(&e), "a");
    }

    #[test]
    fn a_signed_frame_slot_is_one_name() {
        // `loc-8` must lex as a single name; splitting it invents a variable
        // called `loc` and renumbers every slot after it.
        for text in ["loc-8", "loc+4", "par2", "var$12.3", "[ECX-16]"] {
            let e = parse_expr(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert!(
                matches!(&e, Expr::Id(v) if v == text),
                "{text} lexed as {e:?}"
            );
        }
        // A bare `loc` is not a name at all.
        assert!(parse_expr("loc + 1").is_ok());
        assert!(matches!(parse_expr("loc + 1").unwrap(), Expr::Bin(op, _, _) if op == "+"));
    }

    #[test]
    fn the_loop_seed_goes_and_the_loop_stays() {
        let src = "n4 = 5 - 1\nFOR n4 = 5 TO 9\n  n8 = n4 * 2\nNEXT n4\n";
        let (out, _) = tidy(src, None);
        let text = out.join("\n");
        assert!(text.contains("FOR n4 = 5 TO 9"), "{text}");
        assert!(text.contains("NEXT n4"), "{text}");
        assert!(!text.contains("n4 = 4"), "the seed survived:\n{text}");
    }
    #[test]
    fn a_zero_test_on_a_boolean_is_dropped() {
        let e = simplify(&parse_expr("GREATER(a, b) <> 0").unwrap(), None);
        assert_eq!(render(&e), "a > b");
    }
}
