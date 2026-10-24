//! End-to-end timings for the stages of the pipeline.

use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use dbp::{container::Prog, decompile::Dec, listing, pipeline, tidy};

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/sample_virtual.dat")
}

fn bench_stages(c: &mut Criterion) {
    let blob = sample();
    let prog = Prog::load_file(&blob).expect("sample blob parses");

    let mut dec = Dec::new(&prog);
    dec.run();
    let lines = dec.render();
    let transcription = lines.join("\n");

    c.bench_function("container parse", |b| {
        b.iter(|| Prog::load_file(&blob).unwrap())
    });

    c.bench_function("listing", |b| {
        b.iter(|| std::hint::black_box(listing::listing(&prog, 0, None)))
    });

    c.bench_function("decompile statements", |b| {
        b.iter(|| {
            let mut d = Dec::new(&prog);
            d.run();
            std::hint::black_box(d.render())
        })
    });

    c.bench_function("readability pass", |b| {
        b.iter(|| std::hint::black_box(tidy::tidy(&transcription)))
    });

    c.bench_function("whole pipeline", |b| {
        b.iter(|| std::hint::black_box(pipeline::run(&blob, blob.parent().unwrap()).unwrap()))
    });
}

criterion_group!(benches, bench_stages);
criterion_main!(benches);
