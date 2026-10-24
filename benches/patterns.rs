//! The statement shapes the readability pass recognises, matched by hand
//! against the same shapes matched by compiled regexes.
//!
//! The library uses the hand-written matchers ([`dbp::patterns`]); this bench
//! is the evidence that was the right call. Both run over the lines of a real
//! transcription.

use criterion::{Criterion, criterion_group, criterion_main};
use dbp::patterns;
use regex::Regex;
use std::path::PathBuf;
use std::sync::LazyLock;

/// The regexes the shapes were originally written as.
struct Pat {
    assign: Regex,
    array_dst: Regex,
    for_: Regex,
    if_: Regex,
    ifgoto: Regex,
    label: Regex,
    line: Regex,
    banner: Regex,
    init: Regex,
    next_: Regex,
}

static PAT: LazyLock<Pat> = LazyLock::new(|| {
    Pat {
    assign: Regex::new(r"^([^\s=][^=]*?)\s*=\s*(.+)$").unwrap(),
    array_dst: Regex::new(r"^([A-Za-z_]\w*\$?\w*)\((.+)\)$").unwrap(),
    for_: Regex::new(r"^FOR\s+(\S+)\s*=\s*(.+?)\s+TO\s+(.+?)(?:\s+STEP\s+(.+))?$").unwrap(),
    if_: Regex::new(r"^IF\s+(.*?)\s*(<>|=)\s*0\s+THEN$").unwrap(),
    ifgoto: Regex::new(r"^IF\s+(.*?)\s*(<>|=)\s*0\s+THEN\s+GOTO\s+(\S+)$").unwrap(),
    label: Regex::new(r"^(L[0-9A-F]+):$").unwrap(),
    line: Regex::new(r"^' ==== source line (\d+) ====$").unwrap(),
    banner: Regex::new(
        r"^' (DarkBASIC PRO|source blob|machine code|commands|expressions|control flow|statements|gaps|Names:|par<n>|loc<disp>|variable-space|=+$|-+$)",
    )
    .unwrap(),
    init: Regex::new(r#"^\$?[A-Za-z_]\w*\$?\w* = (-?[\d.]+|"")$"#).unwrap(),
    next_: Regex::new(r"^NEXT(\s+[A-Za-z_][\w$]*)?$").unwrap(),
}
});

/// Every line of the sample transcription.
fn lines() -> Vec<String> {
    let blob = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/sample_virtual.dat");
    let prog = dbp::container::Prog::load_file(blob).expect("sample blob parses");
    let mut dec = dbp::decompile::Dec::new(&prog);
    dec.run();
    dec.render()
        .join("\n")
        .split('\n')
        .map(str::to_string)
        .collect()
}

fn bench_hand(c: &mut Criterion) {
    let lines = lines();
    c.bench_function("hand-written matchers", |b| {
        b.iter(|| {
            let mut hits = 0usize;
            for l in &lines {
                hits += usize::from(patterns::shape(l).is_some());
            }
            std::hint::black_box(hits)
        })
    });
}

fn bench_regex(c: &mut Criterion) {
    let lines = lines();
    c.bench_function("compiled regexes", |b| {
        b.iter(|| {
            let p = &*PAT;
            let mut hits = 0usize;
            for l in &lines {
                hits += usize::from(p.assign.is_match(l))
                    + usize::from(p.array_dst.is_match(l))
                    + usize::from(p.for_.is_match(l))
                    + usize::from(p.if_.is_match(l))
                    + usize::from(p.ifgoto.is_match(l))
                    + usize::from(p.label.is_match(l))
                    + usize::from(p.line.is_match(l))
                    + usize::from(p.banner.is_match(l))
                    + usize::from(p.init.is_match(l))
                    + usize::from(p.next_.is_match(l));
            }
            std::hint::black_box(hits)
        })
    });
}

/// The whole statement classifier, as the readability pass drives it: one
/// `shape()` call per line, against the regex set built once per process.
fn bench_shape_vs_all_regexes(c: &mut Criterion) {
    let lines = lines();
    c.bench_function("one shape() call", |b| {
        b.iter(|| {
            let mut hits = 0usize;
            for l in &lines {
                hits += usize::from(patterns::shape(l).is_some());
            }
            std::hint::black_box(hits)
        })
    });
    c.bench_function("ten regex scans", |b| {
        b.iter(|| {
            let p = &*PAT;
            let mut hits = 0usize;
            for l in &lines {
                hits += usize::from(p.for_.is_match(l) || p.if_.is_match(l) || p.label.is_match(l));
            }
            std::hint::black_box(hits)
        })
    });
}

criterion_group!(benches, bench_hand, bench_regex, bench_shape_vs_all_regexes);
criterion_main!(benches);
