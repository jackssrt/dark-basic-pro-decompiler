# Patching a compiled program

How to change a constant, a variable, a command call or an instruction in a
compiled DarkBASIC PRO game, and how to prove the change landed.

Work on a copy.  Every recipe here rewrites bytes in place, and a wrong offset
corrupts the program silently.

## The three places a value lives

Everything you might want to change is one of these.  Find which before
editing anything.

| where | what it holds | how to spot it |
|---|---|---|
| **literal** | a number the programmer wrote | an operand whose byte offset is *not* in the ref table |
| **ref slot** | a variable, string, command, label, DATA offset or compiler constant | an operand at an offset that *is* in the ref table; the code holds `FF FF FF FF` |
| **section** | the command table, string table, variable-space size | past the end of the machine code block |

A literal is a plain immediate in the code and edits in place.  A ref-backed
operand edits the ref table and the code stays `FF FF FF FF`.  See
`DBPRO-EXE-FORMAT.md` §3 for why the two exist.

Type 4 ("immediate") slots are not all programmer constants: the compiler also
stores *displacements* there — the distance of an array null/bounds check, the
`[EBP+A]` frame offset of a local, the offset of a field inside a user type.
Changing one of those changes the control flow or the address a statement
touches, not a value.  `DBPRO-EXE-FORMAT.md` §5 decodes every shape the compiler
emits, so read the statement the slot belongs to before editing it.

## Steps

### 1. Find the statement

Decompile the blob and search the transcription for the assignment or call:

```sh
dbp-decompile out/_virtual.dat          # writes recovered.dba
grep -n "var\$728 = " out/recovered.dba
```

The listing gives you the address, the resolved operand and the raw bytes
together:

```sh
dbp-list out/_virtual.dat --from 0x1828 --to 0x1840
```

```
001828  CMP EAX4               0                              3D FF FF FF FF
00182D  JNE                    L_059292                       0F 85 FF FF FF FF
001833  MOV EAX IMM4           70                             B8 46 00 00 00
001838  MOV MEM EAX4           @v1(728)                       A3 FF FF FF FF
```

**Done when** you have the MCB address of the instruction to change and you can
see its bytes.  A `--from`/`--to` window around it is the working view for
every step below.

### 2. Classify the operand

An operand is **ref-backed** when its four bytes are `FF FF FF FF` *and* its
offset is in the ref table; otherwise it is a **literal**.

Check with the crate rather than by eye — 30 literals in *Remember* are also
`FF FF FF FF` (the constant −1), so the bytes alone lie:

```rust
let prog = dbp::container::Prog::load_file("out/_virtual.dat")?;
let slot = ins.addr + ins.opcode;          // first operand
let backed = prog.refpos.contains(&slot);  // false => literal
```

**Done when** you know which of the two you are editing.  Getting this wrong is
the one mistake that produces a program which still decompiles cleanly and
behaves differently.

### 3a. Change a literal

Write the new value over the four bytes at the operand's offset:

```rust
let at = mcb_off + slot;
data[at..at + 4].copy_from_slice(&new_value.to_le_bytes());
```

`var$728 = 70` at `0x1833` is `B8 46 00 00 00`; changing the operand at
`0x1834` to `123` gives `B8 7B 00 00 00`.  The instruction length is
unchanged, so no other offset moves.

**Done when** the listing prints the new value at the same address.

### 3b. Change a ref-backed operand

Rewrite the entry in the **third** ref array, `refidx`.  The three arrays are
contiguous in the file — `refpos`, then `reftype`, then `refidx` — each
`4 * count` bytes, with the count in the four bytes before `refpos`:

```rust
let i = prog.refpos.iter().position(|p| *p == slot).unwrap();
let file_at = refidx_off + i * 4;
data[file_at..file_at + 4].copy_from_slice(&new_value.to_le_bytes());
```

What `new_value` means depends on `reftype[i]`:

| `reftype[i]` | the value is | a good new value |
|---|---|---|
| 1 | index into the command table | another command index — check the argument count matches |
| 2 | index into the string table | another string index |
| 3 | byte offset into the variable space | another variable's offset (4-byte aligned) |
| 4 | the literal itself | any `u32` |
| 5 | byte offset of a jump target in the MCB | another instruction's `addr` |
| 6 | index into the data space | another DATA index |

**Done when** the listing prints the new operand at the same address, and the
address column has not moved.

### 4. Change the instruction itself

Overwrite the opcode byte.  Two constraints:

* **Same width.**  `B8` (`MOV EAX,imm32`) → `B9` (`MOV ECX,imm32`) → `3D`
  (`CMP EAX,imm32`) is safe; → `31` (`XOR EAX,EAX`) is not, because the operand
  bytes then become the next instruction.
* **The operand the neighbours assume.**  the compiler's idiom is
  *load into EAX, then store or compare*, so a `MOV EAX,x` that becomes
  `MOV ECX,x` still stores stale EAX.

**Done when** the listing shows the instruction you intended, at the same
address, with the same length.

### 5. Grow or shrink the block

Only when no existing instruction of the right width exists.  Growing means
every offset after the cut point moves, and the MCB length field — the four
bytes immediately before the block — has to be updated.

* **At the end of the block:** splice bytes in, bump the length field.  Nothing
  after the MCB is pointed at by an absolute offset, so the tail sections just
  move along.
* **In the middle:** also shift every `refpos` entry at or after the cut, and
  every `reftype == 5` label value at or after the cut.  In *Remember* that is
  54893 of 55862 slots.  Miss one and the decoder reads a variable offset as a
  string index.

```rust
for i in 0..count {
    if refpos[i] >= cut { refpos[i] += delta; }
    if reftype[i] == 5 && refidx[i] >= cut { refidx[i] += delta; }
}
```

**Done when** `Prog::from_bytes` still reports the same instruction count,
`unused_refs` empty, and `bad` no larger than before.

### 6. Put it back in the game

The blob lives in the exe's attached-file table as
`DWORD name_len, name, DWORD data_len, payload`.  The length field sits **four
bytes before the payload** — if the size changed, that field must change with
it, or the loader reads the wrong blob:

```rust
let len_at = payload_off - 4;
data[len_at..len_at + 4].copy_from_slice(&(payload.len() as u32).to_le_bytes());
```

Leaving it stale makes the table unparseable: extraction reports *no attached
files*, because every later record is now misaligned.

**Done when** `dbp-extract` pulls a blob back out of the patched exe and its
size is what you wrote.

### 7. Verify

Re-run the pipeline on the patched blob and read what it says:

```sh
dbp-decompile patched.dat -o out/
```

* the statement you changed now reads with the new value;
* `unused reference slots` is still 0;
* `undecoded` has not grown;
* the `IF/ENDIF`, `FOR/NEXT`, `FUNCTION` balance checks still pass.

**Done when** the report's checks are as clean as they were before the patch.
The checks catch a misaligned block; they do not catch a patch that decodes
cleanly but means something else, so confirm the recovered statement too.

## Worked example

`var$728 = 70`, source line 132 of *Remember*:

```
001819  MOV MEM IMM4   @_LINE_(16), 132     ← ref slot 0x181F, type 4
001823  MOV EAX MEM4   @_ERR_(4)             ← ref slot 0x1824, type 3
001828  CMP EAX4       0                     ← ref slot 0x1829, type 4
00182D  JNE            L_059292              ← ref slot 0x182D, type 5
001833  MOV EAX IMM4   70                    ← literal, no ref slot
001838  MOV MEM EAX4   @v1(728)              ← ref slot 0x1839, type 3
```

The constant is the literal at `0x1834`.  The MCB starts at file offset
`0xAF87B`, so the four bytes to overwrite are at file offset `0xB10AF`,
currently `46 00 00 00`.  Writing `123` there and re-running the pipeline gives
`var$728 = 123` in `recovered.dba`.

Note the second store to the same variable at `0x046452` (literal `160`,
source line 3120).  A patch to one of them does not change what the other does.