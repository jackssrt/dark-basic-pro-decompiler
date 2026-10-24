//! Extract `_virtual.dat` from a compiled DBP game.
//!
//! Handles every container DBP can emit:
//!
//! * normal exe -- `[win32 exe section][attached files][20 bytes extra]`
//! * exe + `.pck` sibling -- `[win32 exe section] + [<name>.pck][20 bytes extra]`
//! * `.pck` standalone -- `[attached files]`
//! * compressed variants -- `[..][compress.dll record][raw blob][20 bytes extra]`
//!
//! Compressed payloads need the game's own `compress.dll`, so that path only
//! works on Windows.

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use dbp::extract::{self, BadFormat, Options};

/// Pull the machine code blob out of a compiled DarkBASIC PRO game.
#[derive(Debug, Parser)]
#[command(name = "dbp-extract", version, about, long_about = None)]
struct Args {
    /// The game executable, or a `.pck`.
    game: PathBuf,
    /// Where to write the blob (default: the current directory).
    #[arg(short, long, default_value = ".")]
    outdir: PathBuf,
    /// List the attached files and write nothing.
    #[arg(short, long)]
    list: bool,
    /// Print only errors.
    #[arg(short, long)]
    quiet: bool,
    /// A `compress.dll` to use for a compressed container.
    #[arg(long)]
    compress_dll: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let mut out = io::stdout().lock();
    match run(&args, &mut out) {
        Ok(code) => {
            let _ = out.flush();
            code
        }
        Err(e) => {
            let _ = out.flush();
            let _ = writeln!(io::stderr(), "dbp-extract: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run<W: Write>(args: &Args, out: &mut W) -> io::Result<ExitCode> {
    let data = std::fs::read(&args.game)?;
    let opts = Options {
        compress_dll: args.compress_dll.clone(),
    };
    let (files, source) = match extract::resolve_table(&data, &args.game, &opts) {
        Ok(v) => v,
        Err(e @ BadFormat::NoTarget(_)) => return Err(io::Error::other(e)),
        Err(e) => return Err(io::Error::other(e)),
    };

    if !args.quiet {
        let _ = writeln!(
            out,
            "{}: {} attached file(s), table located via {source}",
            args.game.display(),
            files.len()
        );
        for f in &files {
            let _ = writeln!(out, "  {:<40} {:>10}", f.name, f.size);
        }
    }

    let Some(hit) = files.iter().find(|f| f.base_name() == extract::TARGET_NAME) else {
        return Err(io::Error::other(format!(
            "no {} in {}",
            extract::TARGET_NAME,
            args.game.display()
        )));
    };
    let payload = &data[hit.offset..hit.offset + hit.size];

    if args.list {
        if !args.quiet {
            let _ = writeln!(
                out,
                "{} found ({} bytes) - not written (--list)",
                extract::TARGET_NAME,
                hit.size
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    std::fs::create_dir_all(&args.outdir)?;
    let dest = args.outdir.join(extract::TARGET_NAME);
    std::fs::write(&dest, payload)?;
    if !args.quiet {
        let _ = writeln!(out, "wrote {} ({} bytes)", dest.display(), hit.size);
    }
    Ok(ExitCode::SUCCESS)
}
