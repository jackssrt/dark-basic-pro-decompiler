//! Decompile a DBP `_virtual.dat` end to end.

use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use dbp::pipeline::{self, Outcome};

/// Run the whole pipeline over a compiled DBP program.
///
/// Nothing is executed: the target program and the compiler are never run.
/// Every stage is a static reading of the on-disk blob, so the pipeline works
/// on a blob extracted from any DBP executable.
///
/// Stages:
///   1. container parse and exact symbolic disassembly
///   2. the symbolised listing
///   3. statements, control flow, the transcription
///   4. the readability rewrite, and the checks on it
#[derive(Debug, Parser)]
#[command(name = "dbp-decompile", version, about, long_about = None)]
struct Args {
    /// A `_virtual.dat`, or a game exe / .pck to take one from.
    blob: PathBuf,
    /// Where to write the output (default: alongside the input).
    #[arg(short, long)]
    outdir: Option<PathBuf>,
    /// Print only errors.
    #[arg(short, long)]
    quiet: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let mut out = BufWriter::new(io::stdout().lock());
    match run(&args, &mut out) {
        Ok(code) => {
            let _ = out.flush();
            code
        }
        Err(e) => {
            let _ = out.flush();
            let _ = writeln!(io::stderr(), "dbp-decompile: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run<W: Write>(args: &Args, out: &mut W) -> io::Result<ExitCode> {
    let given = args.blob.canonicalize().map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("no such file: {}: {e}", args.blob.display()),
        )
    })?;
    let outdir = args.outdir.as_deref().map_or_else(
        || {
            given
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf()
        },
        PathBuf::from,
    );
    std::fs::create_dir_all(&outdir)?;

    let quiet = args.quiet;
    let mut say = move |line: &str| {
        if !quiet {
            let _ = writeln!(out, "{line}");
        }
    };

    let blob = pipeline::blob_for(&given)?;
    if blob != given {
        say(&format!(
            "[0/4] extracted {} from {}",
            file_name(&blob),
            file_name(&given)
        ));
    }
    say("[1/4] parsing container and decoding machine code ...");
    say("[2/4] writing symbolised listing ...");
    say("[3/4] decompiling to BASIC ...");
    say("[4/4] rewriting for readability ...");

    let Outcome {
        listing,
        recovered,
        readable,
        report,
        warnings,
        ..
    } = pipeline::run(&blob, &outdir).map_err(io::Error::other)?;

    for (name, body) in [
        ("listing.asm", &listing),
        ("recovered.dba", &recovered),
        ("readable.dba", &readable),
        ("report.txt", &report),
    ] {
        std::fs::write(outdir.join(name), body)?;
    }
    for line in report.lines() {
        say(line);
    }
    Ok(if warnings.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
