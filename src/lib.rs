//! Static decompiler for DarkBASIC PRO compiled programs.
//!
//! A compiled DBP game carries a `_virtual.dat` blob whose Machine Code Block
//! is a fully symbolic instruction stream (see [`container`]). This crate turns
//! it back into readable BASIC without ever running the target program:
//!
//! * [`container`] parses the blob and applies the reference tables,
//! * [`listing`] prints the symbolised instruction listing,
//! * [`decompile`] recovers statements and control flow,
//! * [`tidy`] rewrites the transcription to be readable,
//! * [`extract`] pulls the blob out of a compiled game executable,
//! * [`pipeline`] runs the whole thing and reports what it found.
//!
//! ```no_run
//! use dbp::{decompile::Dec, container::Prog};
//!
//! let prog = Prog::load_file("playagain/_virtual.dat")?;
//! let mut dec = Dec::new(&prog);
//! dec.run();
//! for line in dec.render() {
//!     println!("{line}");
//! }
//! # Ok::<(), dbp::container::Error>(())
//! ```

pub mod container;
pub mod decompile;
pub mod extract;
pub mod listing;
pub mod names;
pub mod patterns;
pub mod pipeline;
pub mod templates;
pub mod tidy;
