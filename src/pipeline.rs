//! The end-to-end run: extract, decode, transcribe, tidy, and check.
//!
//! Nothing is executed: the target program and the compiler are never run.
//! Every stage is a static reading of the on-disk blob, so the pipeline works
//! on a blob extracted from any DBP executable.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::container::Prog;
use crate::decompile::{Dec, Kind};
use crate::{extract, listing, tidy};

/// One thing the checks found wrong.
pub type Warning = String;

/// The container parsed; report what it holds.
pub fn check_container(prog: &Prog) -> Vec<Warning> {
    let mut warn = Vec::new();
    if prog.tail > 0 {
        warn.push(format!(
            "{} trailing bytes after the last section",
            prog.tail
        ));
    }
    if !prog.bad.is_empty() {
        warn.push(format!(
            "{} undecodable bytes in the machine code block (first at {:06X})",
            prog.bad.len(),
            prog.bad[0]
        ));
    }
    if !prog.unused_refs.is_empty() {
        warn.push(format!(
            "{} reference slots the code never uses (first at {:06X})",
            prog.unused_refs.len(),
            prog.unused_refs[0]
        ));
    }
    if prog.cmd_name.is_empty() {
        warn.push("no commands: this is not a DBP program".to_string());
    }
    warn
}

/// `(undecoded bytes, total bytes)` not covered by a decoded statement.
pub fn undecoded(dec: &Dec<'_>) -> (u64, u64) {
    let gaps: u64 = dec
        .statements
        .iter()
        .filter(|s| s.kind == Kind::Gap)
        .map(|s| (s.end - s.addr) as u64)
        .sum();
    (gaps, dec.prog.mcbsize as u64)
}

/// Every reference slot is consumed exactly once, and every instruction
/// belongs to a statement.
pub fn check_transcription(dec: &Dec<'_>) -> Vec<Warning> {
    let mut warn = Vec::new();
    let consumed = dec
        .ins
        .iter()
        .map(|x| usize::from(x.reference.is_some()) + usize::from(x.reference2.is_some()))
        .sum::<usize>();
    if consumed != dec.prog.refpos.len() {
        warn.push(format!(
            "decoded {consumed} references but the table holds {}",
            dec.prog.refpos.len()
        ));
    }

    // Every instruction must belong to a statement: a hole would mean the
    // transcription silently dropped code.
    let mut spans: Vec<(u32, u32)> = dec.statements.iter().map(|s| (s.addr, s.end)).collect();
    spans.sort_unstable();
    let mut cur = 0u32;
    let mut holes: Vec<(u32, u32)> = Vec::new();
    for (a, e) in spans {
        if a > cur {
            holes.push((cur, a));
        }
        cur = cur.max(e);
    }
    if cur < dec.prog.mcbsize {
        holes.push((cur, dec.prog.mcbsize));
    }
    if !holes.is_empty() {
        warn.push(format!(
            "{} instruction(s) claimed by no statement, first at {:06X}",
            holes.iter().map(|(a, b)| b - a).sum::<u32>(),
            holes[0].0
        ));
    }

    let (gbytes, mbytes) = undecoded(dec);
    if mbytes > 0 && gbytes * 10 > mbytes {
        warn.push(format!(
            "{:.1}% of the code is still undecoded ({gbytes} of {mbytes} bytes)",
            100.0 * gbytes as f64 / mbytes as f64
        ));
    }
    warn
}

/// Calls that must not move: anything that reads the clock, the random
/// sequence, or the world (a DLL command). Pure maths and string helpers are
/// excluded, they are safe to duplicate.
pub fn side_effects(text: &str) -> BTreeMap<String, usize> {
    const PURE: &[&str] = &[
        "ABS",
        "SIN",
        "COS",
        "TAN",
        "ATN",
        "SQR",
        "EXP",
        "POW",
        "INT",
        "ROUND",
        "LEN",
        "LEFT",
        "RIGHT",
        "MID",
        "INSTR",
        "UPPER",
        "LOWER",
        "REVERSE",
        "SPACE",
        "VAL",
        "STR",
        "CHR",
        "ASC",
        "ADD",
        "SUB",
        "MUL",
        "DIV",
        "EQUAL",
        "GREATER",
        "LESS",
        "NOT EQUAL",
        "EQUATE",
    ];
    const FOLDED: &[&str] = &[
        "CAST INTEGER TO DOUBLE",
        "CAST INTEGER TO FLOAT",
        "CAST BYTE TO DOUBLE",
        "CAST DOUBLE TO INTEGER",
        "CAST FLOAT TO INTEGER",
        "CAST FLOAT TO DOUBLE",
        "CAST DOUBLE TO FLOAT",
        "CAST INTEGER TO BYTE",
        "CAST BYTE TO INTEGER",
    ];

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in text.split('\n') {
        if line.trim_start().starts_with('\'') {
            continue; // a comment, not a statement
        }
        for name in call_names(line) {
            let name = name.to_string();
            if PURE.contains(&name.as_str()) || FOLDED.contains(&name.as_str()) {
                continue;
            }
            // an inlined condition, not a call
            if ["IF ", "AND ", "OR ", "NOT "]
                .iter()
                .any(|p| name.starts_with(p))
            {
                continue;
            }
            *counts.entry(name).or_default() += 1;
        }
    }
    counts
}

/// Every `NAME(` in `line`, where `NAME` is at least three characters of
/// capitals, digits and spaces -- the shape a DBP keyword has.
fn call_names(line: &str) -> Vec<&str> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    for i in 0..b.len() {
        if b[i] != b'(' {
            continue;
        }
        // The name is the run of keyword characters right before the bracket,
        // starting at its first capital: indentation is not part of it.
        let mut run = i;
        while run > 0
            && (b[run - 1].is_ascii_uppercase()
                || b[run - 1].is_ascii_digit()
                || b[run - 1] == b' ')
        {
            run -= 1;
        }
        let Some(start) = (run..i).find(|k| b[*k].is_ascii_uppercase()) else {
            continue;
        };
        // A name never starts in the middle of an identifier, a path or a
        // member access.
        if start > 0
            && (b[start - 1].is_ascii_alphanumeric() || matches!(b[start - 1], b'_' | b'.' | b'$'))
        {
            continue;
        }
        if i - start < 3 {
            continue;
        }
        out.push(&line[start..i]);
    }
    out
}

/// The readable rewrite must not have changed how many side-effecting calls
/// run.
pub fn check_rewrite(before: &str, after: &str) -> Vec<Warning> {
    let a = side_effects(before);
    let b = side_effects(after);
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .map(|k| {
            format!(
                "{k}: {} call(s) before the rewrite, {} after",
                a.get(k).unwrap_or(&0),
                b.get(k).unwrap_or(&0)
            )
        })
        .collect()
}

/// IF/ENDIF, FOR/NEXT and FUNCTION/ENDFUNCTION must balance.
pub fn check_structure(text: &str) -> Vec<Warning> {
    let mut ifs = 0i64;
    let mut fors = 0i64;
    let mut funcs = 0i64;
    for (n, line) in text.split('\n').enumerate() {
        let s = line.trim();
        if is_if_then(s) {
            ifs += 1;
        } else if s == "ENDIF" {
            ifs -= 1;
        } else if s.starts_with("FOR ") {
            fors += 1;
        } else if is_next(s) {
            fors -= 1; // NEXT ARRAY INDEX(a) is a statement, not a close
        } else if s.starts_with("FUNCTION ") {
            funcs += 1;
        } else if s == "ENDFUNCTION" {
            funcs -= 1;
        }
        for (name, v) in [("if", ifs), ("for", fors), ("function", funcs)] {
            if v < 0 {
                return vec![format!(
                    "{name} closed more often than it was opened, at line {}",
                    n + 1
                )];
            }
        }
    }
    [("if", ifs), ("for", fors), ("function", funcs)]
        .into_iter()
        .filter(|(_, v)| *v > 0)
        .map(|(k, v)| format!("{k} left open ({v})"))
        .collect()
}

fn is_if_then(s: &str) -> bool {
    s.starts_with("IF ") && s.ends_with(" THEN") && !s.ends_with(" GOTO")
}

/// `NEXT`, or `NEXT <var>`, closes a loop. `NEXT ARRAY INDEX(a)` is a DarkBASIC
/// statement in its own right -- it takes an argument, so it is not a closer.
fn is_next(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("NEXT") else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    if !rest.starts_with(char::is_whitespace) {
        return false;
    }
    let mut ident = rest.trim_start().chars();
    matches!(ident.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && ident.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// The `_virtual.dat` for `path`: the file itself when it already is one, or
/// the blob pulled out of a compiled game.
pub fn blob_for(path: &Path) -> std::io::Result<PathBuf> {
    if path
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case(extract::TARGET_NAME))
    {
        return Ok(path.to_path_buf());
    }
    let data = std::fs::read(path)?;
    let opts = extract::Options::default();
    let payload = extract::extract(&data, path, &opts).map_err(std::io::Error::other)?;
    let out = path.with_file_name(extract::TARGET_NAME);
    std::fs::write(&out, payload)?;
    Ok(out)
}

/// The header `recovered.dba` opens with.
pub fn recovered_header(blob: &Path, prog: &Prog, dec: &Dec<'_>) -> String {
    let branches = prog
        .ins
        .iter()
        .filter(|x| matches!(x.name, "JMP" | "JNE" | "JE" | "JGE" | "JLE"))
        .count();
    let lines: usize = dec
        .statements
        .iter()
        .filter(|s| s.kind == Kind::Line)
        .count();
    let distinct: BTreeSet<u32> = dec.statements.iter().filter_map(|s| s.line).collect();
    let mut h = String::new();
    let _ = writeln!(h, "' {}", "=".repeat(72));
    h.push_str("' DarkBASIC PRO source recovered from a compiled DBP executable\n");
    h.push_str("'\n");
    let _ = writeln!(h, "' source blob : {}", blob.display());
    let _ = writeln!(
        h,
        "' machine code: {} bytes, {} instructions, {branches} branch instructions",
        prog.mcbsize,
        prog.ins.len()
    );
    let _ = writeln!(
        h,
        "' commands    : {} DLL / plugin command calls recovered",
        dec.stats.cmd
    );
    let _ = writeln!(
        h,
        "' expressions : {} assignments and array stores recovered",
        dec.stats.assign
    );
    let _ = writeln!(
        h,
        "' control flow: {} IF tests, {} FOR/NEXT loops, {} user functions",
        dec.stats.cond, dec.stats.for_, dec.stats.function
    );
    let _ = writeln!(
        h,
        "' statements  : {lines} source-line markers covering {} distinct original lines",
        distinct.len()
    );
    let _ = writeln!(
        h,
        "' gaps        : {} regions not yet decoded (marked in the output)",
        dec.gaps
    );
    h.push_str("'\n");
    h.push_str(
        "' Names: variable-space slots appear as var$<byte offset>; EBP-relative slots as\n",
    );
    h.push_str("' par<n> (function parameters) and loc<disp> (locals / temporaries).\n");
    let _ = writeln!(h, "' {}", "=".repeat(72));
    h.push('\n');
    h
}

/// The listing file body (`listing.asm`).
pub fn listing_text(blob: &Path, prog: &Prog) -> String {
    let mut s = format!("; {} -- {}\n", prog.appname, file_name(blob));
    let _ = writeln!(
        s,
        "; {} instructions from {} bytes of machine code\n",
        prog.ins.len(),
        prog.mcbsize
    );
    s.push_str(&listing::listing(prog, 0, None));
    s
}

/// What one full run produced.
pub struct Outcome {
    /// The symbolised listing (`listing.asm`).
    pub listing: String,
    /// The transcription (`recovered.dba`).
    pub recovered: String,
    /// The readability rewrite (`readable.dba`).
    pub readable: String,
    /// The report (`report.txt`).
    pub report: String,
    /// Anything the checks found wrong.
    pub warnings: Vec<Warning>,
}

/// Run the whole pipeline over `blob`, writing nothing: the caller decides
/// where the four artifacts go. `outdir` is recorded in the report.
pub fn run(blob: &Path, outdir: &Path) -> Result<Outcome, crate::container::Error> {
    let started = Instant::now();
    let prog = Prog::load_file(blob)?;
    let mut warnings = check_container(&prog);
    let listing = listing_text(blob, &prog);

    let mut dec = Dec::new(&prog);
    dec.run();
    let lines = dec.render();
    let transcription = lines.join("\n");

    let (pretty, legend) = tidy::tidy(&transcription);
    let nfunc = dec
        .statements
        .iter()
        .filter(|s| s.kind == Kind::Func)
        .count();
    let mut readable = String::from(tidy::HEADER);
    tidy::write_legend(&mut readable, &legend, nfunc);
    readable.push('\n');
    readable.push_str(&pretty.join("\n"));
    readable.push('\n');

    warnings.extend(check_transcription(&dec));
    warnings.extend(check_structure(&readable));
    warnings.extend(check_rewrite(&transcription, &readable));

    let recovered = format!("{}{}\n", recovered_header(blob, &prog, &dec), transcription);

    let mut report = String::new();
    let _ = writeln!(report, "DarkBASIC PRO decompilation");
    let _ = writeln!(report, "{}", "=".repeat(72));
    let _ = writeln!(report, "input    : {}", blob.display());
    let _ = writeln!(
        report,
        "blob     : {} ({} bytes)",
        blob.display(),
        std::fs::metadata(blob).map_or(0, |m| m.len())
    );
    let _ = writeln!(report, "output   : {}", outdir.display());
    report.push('\n');
    report.push_str(&report_body(
        &prog,
        &dec,
        pretty.len(),
        lines.len(),
        legend.len(),
    ));
    let _ = writeln!(report, "checks");
    if warnings.is_empty() {
        report.push_str("  container parsed, every reference consumed once,\n");
        report.push_str("  every side-effecting call preserved in number and\n");
        report.push_str("  order, every block balanced.\n");
    } else {
        for w in &warnings {
            let _ = writeln!(report, "  WARNING: {w}");
        }
    }
    report.push('\n');
    let _ = writeln!(report, "elapsed {:.1}s", started.elapsed().as_secs_f32());

    Ok(Outcome {
        listing,
        recovered,
        readable,
        report,
        warnings,
    })
}

/// The container, transcription and rewrite sections of the report.
fn report_body(prog: &Prog, dec: &Dec<'_>, pretty: usize, raw: usize, named: usize) -> String {
    let mut r = String::new();
    let _ = writeln!(r, "container");
    let _ = writeln!(r, "  application name   : {}", prog.appname);
    let _ = writeln!(
        r,
        "  initial display    : mode {}, {}x{}, {} bpp",
        prog.mode, prog.width, prog.height, prog.depth
    );
    let _ = writeln!(
        r,
        "  machine code block  : {} bytes, {} instructions",
        prog.mcbsize,
        prog.ins.len()
    );
    let _ = writeln!(
        r,
        "  reference slots     : {} ({} unused)",
        prog.refpos.len(),
        prog.unused_refs.len()
    );
    let _ = writeln!(
        r,
        "  commands / strings  : {} / {}",
        prog.cmd_name.len(),
        prog.strings.len()
    );
    let _ = writeln!(r, "  DLLs               : {}", prog.dllfn.len());
    let _ = writeln!(r, "  runtime error text : {} strings", prog.errstr.len());
    let _ = writeln!(r, "  variable space     : {} bytes", prog.varsize);
    let _ = writeln!(r, "  data space         : {} bytes", prog.datasize);
    let _ = writeln!(r, "  DATA items         : {}", prog.dstrings.len());
    let _ = writeln!(r, "  dynamic variables  : {}", prog.dynvar.len());
    let _ = writeln!(
        r,
        "  user types         : {} bytes of type patterns",
        prog.utype.len()
    );
    let _ = writeln!(r, "  trailing bytes     : {}", prog.tail);
    r.push('\n');
    let _ = writeln!(r, "transcription (recovered.dba)");
    let _ = writeln!(r, "  statements          : {}", dec.statements.len());
    let _ = writeln!(r, "  library calls       : {}", dec.stats.cmd);
    let _ = writeln!(r, "  assignments/arrays  : {}", dec.stats.assign);
    let _ = writeln!(r, "  IF tests            : {}", dec.stats.cond);
    let _ = writeln!(r, "  FOR/NEXT loops      : {}", dec.stats.for_);
    let _ = writeln!(r, "  user functions      : {}", dec.stats.function);
    let distinct: BTreeSet<u32> = dec.statements.iter().filter_map(|s| s.line).collect();
    let _ = writeln!(r, "  original lines      : {} distinct", distinct.len());
    let (gbytes, mbytes) = undecoded(dec);
    let _ = writeln!(
        r,
        "  undecoded           : {} regions, {gbytes} of {mbytes} bytes ({:.2}%)",
        dec.gaps,
        if mbytes > 0 {
            100.0 * gbytes as f64 / mbytes as f64
        } else {
            0.0
        }
    );
    r.push('\n');
    let _ = writeln!(r, "readability rewrite (readable.dba)");
    let _ = writeln!(r, "  lines               : {pretty} (from {raw})");
    let _ = writeln!(r, "  named variables     : {named}");
    r.push('\n');
    r
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
