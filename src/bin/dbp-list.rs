//! Print the symbolised listing of a DBP machine code block.

use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use dbp::{container::Prog, listing, pipeline};

/// Emit the fully symbolised instruction listing of a compiled DBP program.
#[derive(Debug, Parser)]
#[command(name = "dbp-list", version, about)]
struct Args {
    /// The `_virtual.dat` blob, or a compiled game to take it from.
    blob: PathBuf,
    /// First byte offset to show.
    #[arg(long, default_value_t = 0, value_parser = parse_offset)]
    from: u32,
    /// Stop before this byte offset.
    #[arg(long, value_parser = parse_offset)]
    to: Option<u32>,
}

/// Offsets are written the way the listing prints them: decimal or `0x`-hex.
fn parse_offset(text: &str) -> Result<u32, String> {
    let t = text.trim();
    let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => t.parse(),
    };
    parsed.map_err(|e| format!("{text:?} is not a byte offset: {e}"))
}

fn main() -> ExitCode {
    let args = Args::parse();
    let mut out = BufWriter::new(io::stdout().lock());
    match run(&args, &mut out) {
        Ok(()) => {
            let _ = out.flush();
            ExitCode::SUCCESS
        }
        Err(e) => {
            let _ = out.flush();
            let _ = writeln!(io::stderr(), "dbp-list: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run<W: Write>(args: &Args, out: &mut W) -> io::Result<()> {
    let blob = pipeline::blob_for(&args.blob)?;
    let prog = Prog::load_file(&blob).map_err(io::Error::other)?;
    listing::write_listing(&prog, args.from, args.to, out)
}
