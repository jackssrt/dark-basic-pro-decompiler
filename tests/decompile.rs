//! End-to-end checks over a real compiled DBP program.
//!
//! The fixture is a `_virtual.dat` taken from a compiled game; every assertion
//! here is about the artifact a user would read.

use std::path::{Path, PathBuf};

use dbp::container::Prog;
use dbp::decompile::{Dec, Kind};
use dbp::{extract, listing, pipeline, tidy};

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/sample_virtual.dat")
}

/// Build a `.pck` holding the given files, the way the compiler writes one.
fn pck(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, body) in files {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
    }
    out.extend_from_slice(&[0; 20]); // the DBP exe footer
    out
}

#[test]
fn the_container_parses_and_every_instruction_decodes() {
    let prog = Prog::load_file(sample()).expect("sample blob parses");
    assert!(!prog.appname.is_empty());
    assert!(
        prog.bad.is_empty(),
        "undecodable bytes at {:?}",
        &prog.bad[..prog.bad.len().min(5)]
    );
    assert!(
        prog.unused_refs.is_empty(),
        "unused references at {:?}",
        &prog.unused_refs[..prog.unused_refs.len().min(5)]
    );
    assert_eq!(prog.tail, 0);
    assert!(!prog.cmd_name.is_empty());

    // Every reference slot is consumed by exactly one operand.
    let consumed: usize = prog
        .ins
        .iter()
        .map(|x| usize::from(x.reference.is_some()) + usize::from(x.reference2.is_some()))
        .sum();
    assert_eq!(consumed, prog.refpos.len());

    // The instructions tile the machine code block without gaps.
    let mut at = 0u32;
    for ins in &prog.ins {
        assert_eq!(ins.addr, at, "instruction stream has a hole at {at:#06x}");
        at += ins.size;
    }
}

#[test]
fn every_instruction_belongs_to_a_statement() {
    let prog = Prog::load_file(sample()).unwrap();
    let mut dec = Dec::new(&prog);
    dec.run();
    let mut spans: Vec<(u32, u32)> = dec.statements.iter().map(|s| (s.addr, s.end)).collect();
    spans.sort_unstable();
    let mut cur = 0;
    for (a, e) in spans {
        assert!(a <= cur, "statements overlap at {a:#06x}");
        cur = cur.max(e);
    }
    assert_eq!(
        cur, prog.mcbsize,
        "the transcription stops short of the code"
    );
}

#[test]
fn the_transcription_recovers_the_program_shape() {
    let prog = Prog::load_file(sample()).unwrap();
    let mut dec = Dec::new(&prog);
    dec.run();
    let lines = dec.render();

    assert!(
        lines.iter().any(|l| l.contains("GET DATE$")),
        "no library call recovered:\n{}",
        lines.join("\n")
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("IF ") && l.contains("THEN"))
    );
    assert!(dec.stats.cmd > 0 && dec.stats.assign > 0);
    assert_eq!(dec.gaps, 0, "undecoded regions remain");

    // Source-line markers carry the original line numbers.
    let marked: Vec<u32> = dec.statements.iter().filter_map(|s| s.line).collect();
    assert!(marked.contains(&7), "line markers missing");
    assert!(dec.statements.iter().any(|s| s.kind == Kind::Line));
}

#[test]
fn the_readability_pass_balances_and_keeps_the_calls() {
    let prog = Prog::load_file(sample()).unwrap();
    let mut dec = Dec::new(&prog);
    dec.run();
    let transcription = dec.render().join("\n");
    let (pretty, legend) = tidy::tidy(&transcription);

    assert!(pipeline::check_structure(&pretty.join("\n")).is_empty());
    assert!(pipeline::check_rewrite(&transcription, &pretty.join("\n")).is_empty());
    assert!(!pretty.is_empty());
    // Every inferred name is explained in the legend, and the same slot is
    // never given two names.
    let mut seen = std::collections::HashSet::new();
    for (name, _old, why) in &legend {
        assert!(seen.insert(name.clone()), "duplicate name {name}");
        assert!(!why.is_empty());
    }
}

#[test]
fn next_with_an_argument_is_a_statement_not_a_closer() {
    // `NEXT ARRAY INDEX(a)` is a DarkBASIC statement: counting it as a loop
    // terminator unbalances the block count.
    let balanced = "FOR i = 1 TO 3\n  NEXT ARRAY INDEX(i)\nNEXT i\n";
    assert!(
        pipeline::check_structure(balanced).is_empty(),
        "{:?}",
        pipeline::check_structure(balanced)
    );
    let unbalanced = "FOR i = 1 TO 3\n  NEXT ARRAY INDEX(i)\n";
    assert_eq!(pipeline::check_structure(unbalanced).len(), 1);
}

#[test]
fn a_rewrite_that_keeps_every_call_passes() {
    let before = "SYNC(1)\nMAKE DIR(a)\n";
    assert!(pipeline::check_rewrite(before, before).is_empty());
    // Losing a call is caught; adding one is caught.
    assert_eq!(pipeline::check_rewrite(before, "SYNC(1)\n").len(), 1);
    assert_eq!(pipeline::check_rewrite("SYNC(1)\n", before).len(), 1);
}

#[test]
fn a_pure_helper_may_be_duplicated_by_the_rewrite() {
    let before = "x = ABS(y)\nSYNC(1)\n";
    let after = "x = ABS(y)\nx = ABS(y)\nSYNC(1)\n";
    assert!(pipeline::check_rewrite(before, after).is_empty());
}

#[test]
fn the_pipeline_reports_a_clean_run() {
    let blob = sample();
    let out = pipeline::run(&blob, blob.parent().unwrap()).expect("pipeline runs");
    assert!(out.warnings.is_empty(), "warnings: {:#?}", out.warnings);
    assert!(out.listing.contains("PUSH REGISTERS"));
    assert!(out.recovered.contains("DarkBASIC PRO source recovered"));
    assert!(out.readable.starts_with("' ===="));
    assert!(
        out.report
            .contains("container parsed, every reference consumed once")
    );
}

#[test]
fn the_listing_shows_symbols_and_bytes() {
    let prog = Prog::load_file(sample()).unwrap();
    let text = listing::listing(&prog, 0, None);
    let first = text.lines().next().unwrap();
    assert!(first.starts_with("000000  PUSH REGISTERS"), "{first}");
    assert!(first.ends_with("60"), "{first}");
    // Variable slots are named, not left as bare offsets.
    assert!(text.contains("@v1("), "no named variable slot");
    assert!(text.contains("cmd["), "no resolved command");
}

#[test]
fn the_blob_is_found_inside_a_compiled_game() {
    let blob = std::fs::read(sample()).unwrap();
    let dir = std::env::temp_dir().join("dbp-test-pck");
    std::fs::create_dir_all(&dir).unwrap();
    let container = dir.join("game.pck");
    std::fs::write(
        &container,
        pck(&[("sounds\\beep.wav", b"RIFF"), ("_virtual.dat", &blob)]),
    )
    .unwrap();

    let data = std::fs::read(&container).unwrap();
    let found =
        extract::extract(&data, &container, &extract::Options::default()).expect("blob found");
    assert_eq!(found, blob);
    let _ = std::fs::remove_file(&container);
}

#[test]
fn a_container_without_the_blob_says_so() {
    let dir = std::env::temp_dir().join("dbp-test-pck");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("empty.pck");
    std::fs::write(&path, pck(&[("readme.txt", b"nothing here")])).unwrap();
    let data = std::fs::read(&path).unwrap();
    let err = extract::extract(&data, &path, &extract::Options::default()).unwrap_err();
    assert!(err.to_string().contains("_virtual.dat"), "{err}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_truncated_blob_reports_where_it_ran_out() {
    let data = std::fs::read(sample()).unwrap();
    let err = Prog::from_bytes(&data[..64]).unwrap_err();
    assert!(err.to_string().contains("truncated"), "{err}");
}

#[test]
fn a_range_of_the_listing_can_be_printed_on_its_own() {
    let prog = Prog::load_file(sample()).unwrap();
    let all = listing::listing(&prog, 0, None);
    let lo = prog.ins[10].addr;
    let hi = prog.ins[20].addr;
    let window = listing::listing(&prog, lo, Some(hi));
    assert_eq!(window.lines().count(), 10);
    // Every windowed line is the full listing's line for the same address;
    // variable numbering restarts from the window, so only the head matches.
    for line in window.lines() {
        let addr = &line[..6];
        assert!(
            all.lines().any(|l| l.starts_with(addr)),
            "{line} is not in the full listing"
        );
    }
    assert!(
        window
            .lines()
            .next()
            .unwrap()
            .starts_with(&format!("{lo:06X}"))
    );
}

#[test]
fn the_sample_is_the_same_program_as_the_reference_fixture() {
    // Guards the fixture itself: a different game would make every other
    // assertion in this file meaningless.
    let prog = Prog::load_file(sample()).unwrap();
    assert_eq!(prog.appname, "another chance");
    assert_eq!(prog.mcbsize, 2461);
    assert!(Path::new(&sample()).exists());
}
