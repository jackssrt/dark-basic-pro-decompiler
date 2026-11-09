# AGENTS.md

Rust static decompiler for DarkBASIC PRO compiled programs. It reads the
`_virtual.dat` blob (directly, or extracted from a game exe/`.pck`), decodes its
symbolic Machine Code Block, recovers BASIC statements and control flow, and
writes a readable program. It never runs the target program or the compiler.

Start with `README.md` for the user-facing overview and current coverage
numbers. `DBPRO-EXE-FORMAT.md` is the format reference; `PATCHING.md` is the
procedure for editing a compiled program.

## Commands

```sh
cargo build --release
cargo test
cargo bench                        # criterion: pipeline stages + matcher evidence
cargo run --release --bin dbp-decompile -- <game.exe|_virtual.dat> [-o out/] [-q]
cargo run --release --bin dbp-list -- <blob> [--from 0x100] [--to 0x200]
cargo run --release --bin dbp-extract -- <game.exe> [-o dir] [--list]
```

- Use `--release` for real games: hundreds of MB of input and hundreds of
  thousands of instructions. The test fixture alone is fine in debug.
- `dbp-decompile` writes `listing.asm`, `recovered.dba`, `readable.dba`,
  `report.txt`, and `mcb.elf`; it exits non-zero if any check warns.
- A blob is recognised by the file name `_virtual.dat`. Any other name --
  including `tests/data/sample_virtual.dat` -- is treated as a container, so
  copy the fixture to `_virtual.dat` before running the CLI on it.
- Given an exe, `blob_for` writes the extracted `_virtual.dat` next to the
  input; decompiler outputs go to `-o` (default: alongside the input).
- Compressed containers need the game's `compress.dll` on Windows. Elsewhere
  that path fails by design; use `--compress-dll` on a Windows host.

## Pipeline

`pipeline::run` is the single end-to-end entry:

1. `Prog::load_file` parses `_virtual.dat` and decodes the MCB into `Ins` using
   the static `TEMPLATES` table plus the reference table.
2. `check_container` reports trailing bytes, undecodable bytes, unused refs.
3. `Dec::run` makes four passes: linear statements, label resolution, control
   flow (`IF`, `REPEAT`, `DO`, `FOR`, `FUNCTION`), and BASIC text.
4. `utypes::align` matches the blob's struct/function signatures, then
   `Dec::name_functions` gives recovered functions their real names.
5. `image::build` emits the patched MCB as a loadable ELF32 image.
6. `Dec::render` produces `recovered.dba`, the machine-faithful transcription.
7. `tidy::tidy` produces `readable.dba` and the name legend.
8. `check_transcription`, `check_structure`, `check_rewrite` produce warnings.

## Module map

| file               | responsibility                                                                                                         | change here when                                                        |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------- |
| `src/container.rs` | `_virtual.dat` sections, reference tables, MCB decode, `Prog`/`Ins`/`Ref`                                              | a blob section is misparsed, or operand widths disagree with a template |
| `src/templates.rs` | static instruction templates from `CASMWriter` (runtime bytes + operand-slot flags)                                    | adding or correcting an instruction; keep `container.rs` widths in sync |
| `src/decompile.rs` | statement recognisers, expression recovery, labels, control flow, text emission                                        | a real instruction run is unrecognised, or flow is structured wrongly   |
| `src/extract.rs`   | attached-file tables in exe/`.pck`/dbc, `_virtual.dat` lookup, Windows `compress.dll` FFI                              | a container variant is not found, or extraction format changes          |
| `src/utypes.rs`    | user-type and user-function signature block, field layout, function alignment                                          | struct/function signatures decode or align incorrectly                  |
| `src/listing.rs`   | symbolised `listing.asm`, variable/command naming, byte ranges                                                         | the listing should show different names, bytes, or ranges               |
| `src/patterns.rs`  | hand-written classifiers for statement line shapes                                                                     | a line shape needs recognising; keep the matchers direct, not regex     |
| `src/tidy.rs`      | readability rewrite: parse blocks, copy-propagate/inline, fold operators, undo loop lowering, rename variables, legend | readable output is wrong, redundant, or loses a side effect             |
| `src/names.rs`     | decorated MSVC DLL export -> DarkBASIC keyword                                                                         | a recovered call still shows a decorated C++ name                       |
| `src/image.rs`     | ELF32 image of the patched MCB, with symbols and command stubs                                                         | the Ghidra image needs different sections, symbols, or layout           |
| `src/pipeline.rs`  | orchestration, report, and all checks                                                                                  | adding a check, changing the report, or fixing `blob_for`               |
| `src/bin/*.rs`     | clap CLIs over the library                                                                                             | CLI arguments or output wording                                         |

## Invariants

These are enforced by the checks and the tests. Treat them as the contract, not
as tunable thresholds.

- Every instruction in the MCB belongs to exactly one statement; statements
  tile `[0, mcbsize)`. Unrecognised runs become `Kind::Gap` and are rendered
  as comments -- code is never silently dropped. The second pass re-scans
  inside gaps because compiler preambles often precede a recognisable shape;
  refining a gap must not lose instructions.
- Every reference slot is consumed exactly once. A clean run has empty
  `Prog::bad`, empty `Prog::unused_refs`, and `Prog::tail == 0`.
- `IF/ENDIF`, `FOR/NEXT`, and `FUNCTION/ENDFUNCTION` balance in the readable
  output.
- `tidy` may not change the count or order of side-effecting calls. The pure
  list lives in `pipeline::side_effects`; clock, random, and DLL calls are
  impure.
- Only a reference-table label is a source-level branch. The compiler's own
  relative jumps (array null/bounds checks, the `DIV` guard) stay inside their
  statement.
- Decoding is static: no code path executes the target or the compiler.
- Any warning makes `dbp-decompile` exit non-zero. A change that adds warnings
  is unfinished.

## Tests and fixtures

- `tests/decompile.rs` is the end-to-end contract over
  `tests/data/sample_virtual.dat` (the game "another chance", 2461-byte MCB).
  Tests assert user-visible artifacts: recovered statements, balanced output,
  preserved calls, indentation, and clean pipeline warnings.
- `the_sample_is_the_same_program_as_the_reference_fixture` pins the app name
  and MCB size to catch fixture replacement. Keep it intact.
- Add a focused test in the same file for every new recogniser or rewrite:
  the construct decodes, coverage stays total, and `tidy` produces the shape a
  reader expects.
- `cargo test` runs unit tests, the integration tests, and doc tests.
  `the_pipeline_reports_a_clean_run` requires zero warnings.
- Benchmark commands: `cargo bench --bench decompile` (stage timings) and
  `cargo bench --bench patterns` (hand-written matchers vs. the equivalent
  regexes). The regex set exists only to defend the design choice.

## Conventions

- Rust 2024, MSRV 1.85. Runtime dependencies stay deliberately tiny: `clap`
  and `thiserror`. `regex` is a dev-dependency used only by the comparative
  benchmark; the library's matchers are hand-written in `patterns.rs`.
- Return typed errors (`thiserror`); do not turn malformed input into a panic.
- `unsafe` is confined to the Windows `compress.dll` FFI in `extract.rs`.
- Comments often cite the original compiler source (`ASMWriter.cpp`,
  `Statement.cpp`, `EXEBlock::Init`, ...). Preserve those references when
  moving or rewriting the code they explain.
- The tree is not rustfmt-clean at HEAD (`cargo fmt --check` reports diffs).
  Format only the code you touch; avoid a whole-tree reformat in unrelated
  changes.

## Notes and gotchas

- `report.txt` is the source of truth for undecoded coverage. `README.md`
  records the current numbers for large programs; do not restate them in code
  or tests.
- `dbp-list` numbering restarts at the start of a `--from/--to` window, so a
  windowed line can differ from the full listing in its variable/command
  numbering even though the address matches.
- `DBPRO-EXE-FORMAT.md` section 6 documents an older Python toolchain; those
  scripts are not in this repo.
