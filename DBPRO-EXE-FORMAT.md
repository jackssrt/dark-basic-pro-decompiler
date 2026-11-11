# DarkBASIC PRO `_virtual.dat` — format and decompilation notes

Everything below was derived from the compiler source in `DBProCompiler/DBPCompiler/`
and verified byte-for-byte against `_virtual.dat` blobs taken from real games
(*Remember*, *lostmemory*) and the sample in `tests/data/`.

## 1. How DarkEXE loads and runs the compiled program

`DBProCompiler/DBPCompilerEXE/DarkEXE.cpp`:

1. `WinMain` extracts the appended file table into `%TEMP%\dbpdataN\`
   (`CFileReader::CreateVirtualFileTable`).  That directory contains all the
   DBPro DLLs and `_virtual.dat`.
2. `CEXE.StartInfo(unpackDir, key)` then `CEXE.Load(<tmp>\\_virtual.dat)`
   (`EXEBlock::Load`, `EXEBlock.cpp:522`).
3. `CreateTempWindow`, then `RunProgram` → `CEXE.Init` → `CEXE.Run`.
4. `CEXE.Run` (`EXEBlock.cpp:1565`) is literally:

```cpp
g_CORE_Program = ( GDI_RetVoidParamVoidPFN ) m_pMachineCodeBlock;
g_CORE_Program();
```

`m_pMachineCodeBlock` is loaded with `PAGE_EXECUTE_READWRITE`
(`LoadValueArrayBytes`, `EXEBlock.cpp:558`), so calling it as a bare
`void(*)(void)` works.  No stack frame is needed because **the machine code
block is a complete x86 function**: it begins with

```
60                    PUSHAD
89 25 <@$_ESP_>       MOV dword[@$_ESP_], ESP
```

and ends with `8B 25 <@$_ESP_> / 61` (`MOV ESP,[@$_ESP_]` / `POPAD`).
`@$_ESP_` is a reserved variable-space slot at byte offset 0, saved by
`BUILD_STARTPROGRAM` (`ParseInstruction.cpp:582`) and restored by
`BUILD_ENDPROGRAMANDQUIT`, so the prologue/epilogue balance is preserved even
though the compiler never emits `PUSH EBP / MOV EBP,ESP`.

Everything between is straight-line x86 plus `CALL EBX` into the DBPro DLLs.
The "commands" are not called by name: the compiler stores an index into a
**command table** and patches it at load time (see §4).

`CEXE.Init` also links the DLLs (`GetProcAddress` for each exported symbol) and
resolves the escape/break variables the runtime uses for `END` and `BREAK`.

## 2. `_virtual.dat` container layout

Straight from `CEXEBlock::Load` / `Save`.  All integers little-endian DWORDs.

| # | field | encoding |
|---|---|---|
| 1 | `m_dwInitialDisplayMode` | DWORD — 0 hidden, 1 windowed, 2 windowed desktop, 3 full exclusive, 4 windowed fullscreen |
| 2 | `m_dwInitialDisplayWidth` | DWORD |
| 3 | `m_dwInitialDisplayHeight` | DWORD |
| 4 | `m_dwInitialDisplayDepth` | DWORD |
| 5 | `m_pInitialAppName` | fixed **256 bytes** (window caption; rest is padding) |
| 6 | DLL count + index array + name strings + "already loaded" array | |
| 7 | reference count, then three DWORD arrays: position, type, index | |
| 8 | runtime error string table | |
| 9 | **`m_dwSizeOfMCB`** then that many raw bytes of machine code | |
| 10 | command count, DLL-id array, decorated-name string array | |
| 11 | string table (count + length-prefixed strings) | |
| 12 | DATA items: count, `count*10` bytes, then `count` strings | |
| 13 | variable-space size, data-space size | |
| 14 | dynamic variable offsets + types | |
| 15 | user-type pattern blob | |

A string array entry is `DWORD length` followed by `length` bytes (no NUL).
A value array entry is `count` DWORDs; a byte array is `count` bytes.

Section 15, the user-type block, is one NUL-terminated string built by
`CASMWriter::UpdateStructurePatternData` (`ASMWriter.cpp:2189`).  It holds one
record per struct table entry with a declaration chain — every `TYPE`, and every
user `FUNCTION` (`Statement.cpp:3886`) — concatenated with no separator:

```text
<name>:<struct table index>:<field type letters>:
```

`UpdateStructurePatternDataRec` writes one letter per field in declaration
order: `L` integer, `F` float, `S` string, `B` boolean, `Y` byte, `W` word,
`D` dword, `O` double float, `R` double integer, `-` anything else.  A nested
user type is expanded in place instead of contributing a letter of its own, so
the letters are the flattened layout.  `CStructTable::CalculateSize`
(`StructTable.cpp:289`) puts each field at the running offset with the size of
its type, so the offsets follow from the letters: 4 bytes each, 8 for `O`/`R`.
A user function's chain starts with a synthetic `returnvalue` integer slot, so
its first letter is that return slot and the parameters follow.

Field *names* are not in the format — the compiler uses them only while
compiling — and no record says whether it is a type or a function.  The
recovered code resolves the second question: functions appear in the table in
construction order, which is source order, and the recovered functions are in
address order, which is also source order, each with its parameter count from
its call sites.  Matching from the end of the block gives names to the
functions and leaves the struct layouts behind.

For *Remember*: 40 DLLs, 55862 references, 9999 error strings, MCB 365233 bytes,
342 commands, 336 strings, no DATA, variable space 1603 bytes, 23 dynamic
variables, 2562 bytes of user-type patterns.

## 3. The machine code block is already symbolic

This is the key observation.  `CASMWriter::CreateASMMiddleCore`
(`ASMWriter.cpp:433`) emits an operand as the 32-bit placeholder `0xFFFFFFFF`
and records the byte offset in `m_pProgramRefs`.
`CASMWriter::UpdateMCBRefData` (`ASMWriter.cpp:1579`) later classifies each
reference, and `CEXEBlock::Init` (`EXEBlock.cpp:1398-1541`) substitutes the real
value at run time.  An operand the ref table covers is therefore `0xFFFFFFFF`
on disk, and the ref table says what it should be.

**A literal constant is the exception.**  A number the programmer wrote is
emitted as its real bytes and gets *no* ref slot, so it sits in the block as an
ordinary immediate.  In *Remember* that is 5492 operands against 22822 that do
go through the table, and the split is not cosmetic: patching a ref-backed
operand means editing the ref table, patching a literal means editing the code.
`PATCHING.md` covers both, and how to tell them apart.

Reference types:

| type | meaning | value |
|---|---|---|
| 1 | DLL command call | index into the command table |
| 2 | string literal | index into the string table |
| 3 | variable | byte offset into the variable space |
| 4 | immediate | the literal itself (compiler-generated constants only) |
| 5 | label | byte offset of a jump target in the MCB |
| 6 | DATA label | index into the data space |

Reference type 5 (label) is patched to a **relative** displacement, not to an
absolute address: `EXEBlock::Init` writes `label - (operand offset + 4)`.  Type
4 (immediate) is written verbatim, and the compiler puts more than programmer
constants there — the displacement of every leap marker is a type-4 reference
holding the finished rel32 (`ASMWriter::WriteASMLeapMarkerEnd`), and so are
struct offsets and `[EBP+A]` displacements.  The reference *kind* of a jump is
therefore meaningful: `LABEL` is a branch the programmer wrote, `IMMEDIATE` is
compiler scaffolding that belongs to the statement around it.

Decoding the MCB therefore needs no disassembly heuristics beyond the opcode
table itself, which is `CASMWriter::GenerateASMCodes` (`ASMWriter.cpp:100-379`).
Transcribing that table gives a complete, exact decoder: the *Remember* MCB
decodes to 87045 instructions with **zero** undecodable bytes, and all 55862
references are consumed exactly once.

The decoder tells the two apart from the ref table alone: an operand whose byte
offset appears in `refpos` is ref-backed, and one that does not is a literal
read straight out of the block (`container.rs:386-417`).

Two traps in that table:

* Templates whose operand slot exists but is always written empty
  (`PUSH EAX`, `PUSH EBP`, `MOV EBP ESP`, `MOV EAX ECX4`, …).  Deciding by the
  ref table alone makes the decoder swallow the next instruction's operand.
* Two-operand forms via `WriteASMLine2`/`2IMM`: `MOV dword[MEM],IMM`
  (`C7 05 addr value`), `MOV SIB[...],IMM`, `LOOP`.

`LOOP` is written as `E2` followed by a 4-byte immediate but the write pointer
only advances one byte (`CreateASMMiddleCore` case 0), so only the `rel8` is
real; the following three bytes are overwritten by the next instruction.

## 4. Statement boundaries and line numbers

Every BASIC statement is preceded by the runtime-error hook emitted by
`ASMTASK_RUNTIMEERRORHOOK` (`ASMWriter.cpp:3825`):

```
C7 05 <ref @16> <ref:line>    MOV dword[@$_SLN_], <source line>
A1  <ref @4>                  MOV EAX, [@$_ERR_]
3D  <ref 0>                   CMP EAX, 0
0F 85 <ref $labelend>         JNE $labelend
```

`@16` is the reserved slot `$_SLN_` (`VarTable.cpp` `SetVarDefaults`:
`$_ESP_`=0, `$_ERR_`=4, `$_ESC_`=8, `$_REK_`=12, `$_SLN_`=16, `$_TEMPA_`=20,
`$_TEMPB_`=24).  **The immediate is the original BASIC line number**, so the
statement order and the original line numbering come back exactly: *Remember*
recovers 5363 statement markers over 2765 distinct source lines.

## 5. Code-generation patterns worth knowing

All operand order rules come from `MathOp::WriteDBMBit` (`MathOp.cpp:2556`),
which passes the **LEFT operand as P1** and the **RIGHT as P2**, while
`WriteASMTaskCore` emits **P2 first**.  So in the instruction stream *the first
value loaded is the right-hand operand*.

### Library command call

```
<argN> PUSH EAX ... <arg1> PUSH EAX     (right to left)
MOV EBX, <command index>                BB <ref type 1>
CALL EBX                                FF D3
[MOV <dest>, EAX]                       ; statement-level calls store first
POP EBX x n
```

Expression-level calls (the DLL-call path in `WriteDBMBit`) pop first and
store afterwards; both orders are accepted.

### Arithmetic (hard-coded, integer types only)

```
<right>  PUSH EAX
<left>                    (EAX = left, EBX = right after the pop)
POP EBX
ADD/SUB/AND/OR/XOR/IMUL  EAX,EBX
MOV <dest>, EAX
```

With an immediate right operand it collapses to `<left>; ADD EAX,imm`.

### Comparison

```
<right>  PUSH EAX
<left>   MOV EBX,EAX
         POP EDX                      ; EDX = right
         MOV EAX,0                    ; SETcc only writes AL
         CMP EBX,EDX                  ; opcode 3B DA = CMP EBX,EDX
         SETE/SETNE/SETG/SETGE/SETL/SETLE EAX
MOV <tmp>, EAX
```

`CMP EBX,EDX` computes `left - right`, so `SETG` is `>` on the source
left/right pair.  The compiler's own template name for `3B DA` ("CMP EDX EBX")
is mislabelled — that misreading inverts every comparison operator.

### IF

The comparison task emits no branch.  The enclosing statement emits
`ASMTASK_CONDITION` + `ASMTASK_CONDJUMPE`:

```
<load condition>   [NOT EAX]   CMP EAX,0   JE <else/endif>
```

`NOT` in the source sets a writer-global flag that inverts this to `JNE` and
inserts the `NOT EAX`.

### FOR / NEXT

`CStatement::DoLoop` (`Statement.cpp:686-1135`) lowers `FOR v = a TO b STEP s`
to three separate statements:

```
v = a - s                 (initialiser, before the loop)
L:  v = v + s              (the loop label points at the increment)
    <tmp> = v <= b
    IF <tmp> <> 0 THEN GOTO exit
    ...body...
    JMP L
```

which is what the `FOR` recogniser in `decompile.rs` matches.

### Arrays

With safe arrays on (`GetArrayCheckFlag`), every element access is bracketed by
the compiler's own safety code.  All of those jumps are **relative
displacements written as an immediate reference** (`WriteASMLeapMarkerJump`);
a source-level branch uses a `LABEL` reference instead.  That one difference is
what keeps an array statement from being mistaken for an `IF`.

An element access is four emitted blocks:

```text
; 1  ASMTASK_PUSHINTERNALARRAYINDEX -- only when the source had no subscript
MOV EAX,[@arr]   CMP EAX,0   JE <next>
MOV EDX,[EAX-4]              ; the array's list cursor
PUSH EDX
<next>:

; 2  ASMTASK_CALCARRAYOFFSET -- pop the subscript into the offset temporary
MOV EAX,[@arr]   CMP EAX,0   JE <after>
POP EBX   MOV EAX,EBX   MOV [@tmp],EAX
<after>:

; 3  the element itself (CalculateArrayOffsetInEBX)
MOV EAX,[@arr]   CMP EAX,0   JE <error>
MOV EBX,[@tmp]               ; or an immediate
PUSH EDX   MOV EDX,[EAX-16]   CMP EDX,EBX   POP EDX   JGE <error>
CMP EBX,-1   JLE <error>
MOV EAX,[EAX+EBX*4]

; 4  the field inside the element, then the leap over the error block
MOV ECX,[EAX+off]   MOV EAX,ECX      ; read
MOV [EAX+off],ECX                    ; write (value already in ECX)
JMP <end>
<error>:  MOV [@$_ERR_],118           ; + the runtime-error hook, 26 bytes
<end>:
```

`[EAX-16]` is the element count, `[EAX-4]` the list cursor — the value
`ARRAY INDEX(arr)` returns, which is what `ARRAY INDEX TO TOP` / `NEXT ARRAY
INDEX` move.  A store evaluates its value between blocks 2 and 3, and block 4
is exactly `WriteASMEAXtoARR`; a read is `WriteASMARRtoEAX`.  The error block is
unreachable, so the transcription folds it into the statement.

A **multi-dimensional** array adds `POP EDX / IMUL EDX,[EAX-56+4*i] / ADD
EBX,EDX` per extra dimension inside block 2.  The compiler parses the source
subscript list from right to left, so the subscripts go on the stack in
reverse order: `POP EBX` takes the first source subscript, and each extra
`POP EDX` takes the next one.  The decompiler records the subscripts in source
order, renders them as `arr(a, b, ...)`, and folds the offset calculation into
the access that reads its temporary.  If the temporary is computed apart from
the access, the calculation stays as `tmp = ARRAY OFFSET(a, b, ...)`.

### SELECT / CASE

`SELECT` writes the tested value into EAX (`ASMTASK_ASSIGNTOEAX`) and then one
`CMP EAX,<case value>` + `JE <case label>` per case (`ASMTASK_CONDITIONDATA`,
`ParseJump.cpp`), ending with a `JMP` past the last case.  Comparing against an
immediate is what distinguishes a case from a normal `IF`, whose compare is
always `CMP EAX,0`.

### SYNC()

`SYNC()` is not a call in the compiled code: `BUILD_SYNC` emits
`CALL ProcessMessages` followed by `CMP EAX,1 / JE $labelend`, so the program
leaves when the runtime says it was asked to quit.  The transcription writes it
back as `SYNC()`.

### User functions

`PUSH EBP / MOV EBP,ESP / SUB ESP,n` + a stack-clearing loop, and
`MOV ESP,EBP / POP EBP / RET`.  Parameters arrive at `[EBP+8]`, `[EBP+12]`, …
and locals/temps at negative offsets (`CParseUserFunction::ActOnLocalVars`).
Calls are `CALL rel32` to the label `$label <name>`.

## 6. Tooling

The repository builds three binaries with `cargo build --release`:

| binary | purpose |
|---|---|
| `dbp-decompile` | runs the whole pipeline; start here |
| `dbp-extract` | pulls `_virtual.dat` out of a game exe or `.pck` |
| `dbp-list` | prints the symbolised listing, optionally for an address window |

One command, from a game executable or from a blob:

```sh
dbp-decompile game.exe -o out/
```

That writes `listing.asm`, `recovered.dba`, `readable.dba`, `report.txt` and
`mcb.elf` (the machine code block as a loadable ELF32 image), then checks its
own work and exits non-zero if any check warns.  The stages are also runnable
on their own:

```sh
dbp-extract game.exe -o out/                  # just the blob
dbp-list    out/_virtual.dat                  # the symbolised listing
dbp-list    out/_virtual.dat --from 0x100 --to 0x200
```

A file is treated as a blob only when it is named `_virtual.dat`; any other
name is parsed as a game container, so point the listing at the extracted blob,
or pass the executable and let the tool extract it.

`recovered.dba` is the artifact to check a claim against: it is a direct
transcription of the machine code, so every statement in it can be checked
against the listing.  `readable.dba` is the same program after the passes in
§7.

Neither the compiler nor the target executable is executed; everything is static
parsing of the on-disk blob.

### What the pipeline checks

The checks are what make the pipeline trustworthy on a program it has never
seen, so `dbp-decompile` runs them every time and a warning is a non-zero exit:

| check | what would make it fire |
|---|---|
| container | trailing bytes, undecodable bytes, unused reference slots, no commands |
| references | the decoded code consumes exactly as many reference slots as the table holds — the decoder has no leftovers and no double reads |
| transcription | an instruction belongs to no statement — a gap would have swallowed code |
| coverage | more than 10% of the code is still undecoded |
| side effects | the rewrite changed how many times any call that reads the clock, the random sequence or the world runs |
| structure | IF/ENDIF, FOR/NEXT or FUNCTION/ENDFUNCTION do not balance |

The side-effect check is the important one: it is what stops a readability
pass from quietly duplicating a `RND()` or dropping a `TIMER()`.

### How much of each program is decoded

`report.txt` prints the undecoded remainder as both a count of regions and the
bytes those regions cover, because the count alone is misleading: one large
region that splits into a decoded statement plus a smaller gap raises the
count while lowering the bytes.  The warning fires above 10% of the code, so a
program that has quietly stopped decoding cannot pass as clean.

The large programs checked so far decode completely.  On `lostmemory.exe`
(841,740 bytes of machine code):

| program | statements | commands | undecoded |
|---|---|---|---|
| `lostmemory.exe` | 24,793 | 7,739 | **0 of 841,740 bytes (0.00%)** |

The last shapes to fall were the array model above and multi-dimensional
subscripts.  An element access is four emitted blocks, and recognising it from
its pointer load alone leaves the null-guard preamble, the subscript
calculation and the error block behind as fragments; `lostmemory.exe` has 2,538
element accesses, each contributing a handful of those.  Decoding the four
blocks as they are emitted -- and treating a relative jump as part of its
statement rather than as a branch -- removes all of them.

### Why nothing is ever dropped

Decoding runs in two passes.  The first walks the instruction stream and each
recogniser claims what it understands; the rest becomes a *gap*, transcribed
verbatim as comments.  The second pass re-runs the same recognisers at every
instruction address *inside* each gap, because a compiler preamble — saving a
value, stashing it, loading a table — routinely sits in front of a shape that
is perfectly recognisable, and the first pass never gets past it to see it.

That second pass is what recovers statements hiding behind compiler preambles.
It is also why gap *counts* can go up while gap *bytes* go down: one 108-byte
region became a decoded statement plus several small gaps, which is a win, not
a regression.

`dbp-decompile` checks the invariant that makes this safe — every instruction
in the block belongs to some statement, so refining a gap cannot lose code — and
reports it on every run.

## 7. Reading the recovered program

`recovered.dba` is a transcription: every line corresponds to statements in
the listing, which is what makes it checkable.  It is not readable.  The
readability pass (`tidy.rs`) rewrites it as `readable.dba`, which is the same
program with:

* the source-line markers gone (statement order is unchanged, and
  `recovered.dba` still has every original line number);
* the compiler's scratch variables inlined through straight-line code, and the
  stores that nothing reads deleted;
* library operator calls written back as operators — `EQUAL`/`GREATER`/`LESS`
  as `=`/`>`/`<`, `ADD`/`SUB`/`MUL`/`DIV` as `+`/`-`/`*`/`/`, and
  `CAST ... TO DOUBLE/FLOAT` as the value itself;
* literals re-typed the way DBP would have written them: a DWORD with the high
  bit set is negative (`4294965296` is `-2000`), and an IEEE-754 bit pattern
  that decodes to a short decimal becomes that decimal (`1073741824` is `2.0`,
  `1050253722` is `0.3`).  The float test is a round-trip: the decimal must
  encode back to the same 32 bits, which is what separates a literal the
  programmer typed from a value the program computed;
* the loop lowering undone (`FOR`/`NEXT` come back), the
  `IF c THEN ... GOTO L ENDIF ... L:` shape turned into a real `IF/ELSE`, and
  jumps to the program epilogue turned into `END`;
* variables renamed from the role their use implies, with a legend in the file
  mapping each name back to its byte offset;
* user types listed with their field layout, and every user function called by
  its name from the blob's signature table (see §2).

Two rules keep this pass from changing what the program does:

* **only side-effect-free values move.**  A call — `RND`, `TIMER`, `GET ...`,
  any DLL command — stays exactly where the compiler put it, because moving one
  changes its value (a clock and a random sequence both advance) and inlining
  it twice evaluates it twice.  The number and order of such calls in
  `readable.dba` is identical to `recovered.dba`.
* **a definition is dropped only where nothing can read it.**  The pass builds
  the control-flow graph out of the recovered statements — branch targets,
  loop bodies, `IF/ELSE` arms, `RETURN` — and runs a backward liveness
  analysis over it, so a store is deleted only when its destination is not live
  after it on any path.  A scratch store that fed an `IF` test is live up to the
  moment the test is rewritten to use its value, and dead immediately after.
  A definition that folded to a constant from a value copied out of a branch the
  decompiler closed early is marked, and its branch is kept.

Names are role-based rather than recovered: the format stores byte offsets, so
the identifiers are simply not in the file.  `i`/`j` are loop counters by
nesting depth, `t` a `TIMER()` snapshot, `path` a file path, `arr` an array,
`rec` a user-type record, `f` a float, `n` an integer, `p`/`v` a function's
parameters and locals.  The BASIC keyword for each command is reconstructed
from the demangled DLL export; a handful (`SC_*`, and the `*DDD`/`*FFF`/`*OOO`/
`*SS` maths variants) collapse onto the same keyword and should be read with
the argument count in mind.

### Recovered result for *lostmemory.exe*

`recovered.dba` is 25,602 lines covering:

* 7,739 library command calls, with their arguments in source order
* 6,597 assignments, comparisons, arithmetic expressions and array stores
  (including the `DIM ARRAY` initialisers)
* 1,143 IF tests, 242 FOR/NEXT loops and 175 FUNCTION/ENDFUNCTION blocks
* the original line number for every statement (4,483 distinct source lines)
* **no undecoded regions**: all 189,379 instructions belong to a statement

`readable.dba` is the same program in 13,250 lines, with the same calls and no
undecoded regions.

Not recovered: variable *identifiers*, comments and REM statements, and the
argument-count-driven keyword distinctions where several DBP keywords share one
DLL export.
