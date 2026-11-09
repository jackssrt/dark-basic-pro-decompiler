# dark-basic-pro-decompiler

> [!WARNING]  
> this whole thing is vibecoded. im just putting that out there first. but it works nonetheless, and really well!


Turns a compiled DarkBASIC PRO game back into readable BASIC, without ever
running the program or the compiler.

A compiled DBP game carries a `_virtual.dat` blob whose Machine Code Block is a
fully symbolic instruction stream: the compiler wrote it through `CASMWriter`
(`DBProCompiler/DBPCompiler/ASMWriter.cpp`), recording every 32-bit operand in a
reference table and leaving a placeholder in the stream. Applying that table
decodes the block; the shapes it contains then read back as statements.

```
dbp-decompile playagain/_virtual.dat          # the whole pipeline
dbp-list     playagain/_virtual.dat --from 0x100
dbp-extract  game.exe -o out/                 # pull the blob out of a game
```

`dbp-decompile` writes five files:

| file | what it is |
|---|---|
| `listing.asm` | every instruction, fully symbolised, with its raw bytes |
| `recovered.dba` | one BASIC statement per line, a direct transcription |
| `readable.dba` | the same program after the readability pass |
| `report.txt` | what was found, and the checks that ran |
| `mcb.elf` | the machine code block as a loadable ELF32 image, for a disassembler |

## What it recovers

* the container (display mode, DLLs, commands, strings, variable space) and the
  instruction stream with its references applied;
* statements: library command calls, assignments, array element reads and
  stores (`arr(i).field`), comparisons, arithmetic, `INC`/`DEC`, `SYNC()`,
  `SELECT`/`CASE` dispatches and 64-bit literals;
* control flow: `IF/THEN`, `REPEAT/UNTIL`, `DO/LOOP`, `FOR/NEXT` and user
  `FUNCTION/ENDFUNCTION`, recovered from the branch graph.  Only a
  reference-table label is a source branch; the compiler's own relative jumps
  (array null and bounds checks, the `DIV` guard) stay inside their statement;
* user types and user-function signatures from the blob's own pattern block:
  struct names with their field layout, and the name and parameter types of
  every user function, which the transcription then calls by name;
* readability: scratch variables copy-propagated and dead stores deleted by a
  liveness analysis over the recovered control-flow graph, DBP operator calls
  (`EQUAL`, `ADD`, `CAST INTEGER TO DOUBLE` ...) rewritten as operators,
  IEEE-754 immediates re-typed as decimals, the compiler's loop lowering undone,
  and variables named after the role their use implies.

Undecoded regions are never hidden: they stay in the output as a comment
carrying the raw instructions, and they are counted in the report.

## Checks it runs

* every reference slot is consumed exactly once, and the reference table lines
  up with the machine code;
* every instruction belongs to exactly one statement;
* `IF/ENDIF`, `FOR/NEXT` and `FUNCTION/ENDFUNCTION` balance;
* the readability rewrite does not change how many side-effecting calls run --
  a clock, a random sequence or a DLL command stays where the compiler put it.

A non-empty warning list makes `dbp-decompile` exit non-zero.

## Performance

The stages, measured with `cargo bench --bench decompile` on the sample program
(2461 bytes of machine code, 601 instructions, 112 recovered statements):

| stage | |
|---|---|
| container parse | 142 µs |
| listing | 160 µs |
| statements and control flow | 209 µs |
| readability pass | 1.74 ms |
| whole pipeline | 2.37 ms |

On a larger program -- the 1.1 MB blob of *Remember* (365 KB of machine code,
87 045 instructions, 57 functions) -- the whole pipeline takes 0.9 s, against
11 s for the Python toolchain it was ported from.

Everything decodes: `lostmemory.exe` (841 KB of machine code, 189 379
instructions, 24 793 recovered statements) and *Remember* (365 KB, 87 045
instructions, 15 850 recovered statements) both come back with **0 bytes
undecoded**, and the checks pass; the sample programs decode cleanly too.
The last shapes to fall were multi-dimensional array subscripts, whose
per-dimension `POP EDX / IMUL / ADD` chain the compiler emits inside the
offset calculation, and 64-bit DLL results returned in `EDX:EAX`, which the
compiler spills to an adjacent pair of frame slots before pushing the two
dwords.

The readability pass recognises each statement's shape with the hand-written
matchers in `src/patterns.rs`. `cargo bench --bench patterns` measures them
against the regexes they replaced: 3.1 µs per transcription against 23.3 µs for
the equivalent compiled set, because compiling that set costs more than matching
it does.

## Layout

| module | |
|---|---|
| `container` | the `_virtual.dat` sections and the symbolic decoder |
| `templates` | the instruction templates `CASMWriter` emits |
| `names` | DLL export name -> DBP keyword |
| `listing` | the symbolised listing |
| `decompile` | statements, expressions, control flow, the transcription |
| `utypes` | the blob's user-type and user-function signatures |
| `patterns` | the statement shapes the readability pass recognises |
| `tidy` | the readability pass |
| `extract` | finding the blob inside a compiled game |
| `pipeline` | the whole run, and the checks |

## Building

```
cargo build --release
cargo test
cargo bench
```

Compressed containers (`compress.dll`) can only be expanded on Windows, where
the bundled DLL can be loaded; elsewhere the tool says so and asks for
`--compress-dll` on a Windows host.

The sample program in `tests/data/` is a `_virtual.dat` taken from a compiled
game; the tests and benchmarks run against it.  The port was validated against
a second, much larger program as well -- see `DBPRO-EXE-FORMAT.md` for what
that blob contains.

Two documents sit alongside this one.  `DBPRO-EXE-FORMAT.md` describes the
container and the symbolic machine code block.  `PATCHING.md` describes editing
one: changing a constant, a variable, a command call or an instruction, and
checking the change landed.
