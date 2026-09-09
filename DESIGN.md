# Design notes

Why umbra is built the way it is, and what each choice costs. Cycle numbers
are from the table in [README.md](README.md#performance) — release build,
`perf stat -e cycles` on one pinned core, against PUC Lua 5.4.

## NaN-boxed values instead of a tagged union

Every `Value` is one `u64`. Doubles pass through bit-for-bit; everything else
lives in NaN space: 13 signature bits, a 3-bit tag, and a 48-bit payload that
holds a heap pointer, a small integer, or a bool/none discriminant.

The alternative is Lua's `TValue`: a 16-byte struct of payload + type tag.
16 bytes is what a correct tagged union costs on a 64-bit target — the tag
needs its own word once the payload is a pointer. NaN-boxing halves that:

- Register windows, table arrays, and the stack are all `Vec<Value>`; at 8
  bytes a cache line holds 8 values instead of 4, and the register file of a
  typical function fits in a line or two.
- Table density doubles — the open-addressed `KeyMap` stores `(hash, key,
  value)` triples, and the value half is 8 bytes.
- Copying a value is a register move; there is no tag word to keep in sync.

The cost is real and measurable. The 48-bit payload can't hold every `i64`,
so every integer-producing instruction range-checks its result against
`INLINE_INT_MIN..=INLINE_INT_MAX` and heap-boxes the overflow as a `GcBigInt`.
Lua's `TValue` just stores the `i64`. That check is a large part of why
`fib(30)` runs 325M cycles to Lua's 202M (1.6×) — fib is nothing but integer
arithmetic and calls, so it pays the check on every operation. The table
benchmark (2M array writes + reads, 422M vs 174M, 2.4×) pays it again on
every index computation.

The other cost: NaN can't be a value. All NaN bit patterns are the box, so
`0/0` produces `none` rather than a float NaN. That's a semantic sacrifice
made for the representation — see below.

## Register VM instead of a stack machine

The bytecode is register-based like Lua's: instructions name registers
directly (`Add r1, r2, r3`), so an expression like `a + b * c` compiles to
two instructions instead of a push/pop sequence. Fewer instructions means
fewer dispatches, and dispatch is the thing a bytecode interpreter can't
make free.

The cost is compiler complexity: the compiler does register allocation,
has to model multi-value calls and varargs spilling past the register
window, and every opcode carries operand fields that have to be packed and
decoded. A stack VM is a much simpler target — but it executes roughly 2-3×
the instruction count for the same program, and no amount of dispatch
cleverness wins that back.

## `none` instead of `nil`

Purely a readability choice. `none` reads as "no value" to someone who has
never seen Lua; `nil` reads as a proper noun you have to learn. The runtime
still calls the type `nil` internally (and `tostring(none)` says `nil`) —
the keyword is the user-facing rename, the internals kept the Lua name
because every GC and VM comment already speaks that language.

## `!=` for not-equal, `~=` for xor-assign

The syntax is C-flavored, and in C `!=` is not-equal. Keeping Lua's `~=`
for inequality in a language that otherwise looks like C would be a
permanent typo generator — every user would write `!=` first. The freed
`~` then does what it does in C: `~` is binary xor and `~=` is xor-assign.
The real cost is that pasted Lua code with `~=` silently changes meaning —
accepted, because umbra isn't trying to be source-compatible with Lua.

## UTF-8 strings instead of byte strings

Strings are UTF-8 text, not byte arrays. `string.char(200)` produces the
two-byte `U+00C8`, `utf8.*` iterates codepoints, and `string.sub`/`reverse`
slice on bytes but re-validate so you can't split a multi-byte character
and keep the halves.

The cost: `string.pack`/`unpack` can't return raw bytes, so pack output is
hex-encoded and not interchangeable with real Lua's; binary I/O through
`io` has to round-trip through text. That's the deliberate trade — the
language is for scripting a host, where text is the common case and
mojibake is the common bug. For the cases that genuinely need raw bytes
there's a separate `bytes` type (byte buffer + `bytes.*` functions, binary
`io` modes) so the string type doesn't have to be both things.

## Stop-the-world GC

The collector is a tri-color mark-and-sweep over intrusive object headers:
every heap object starts with a `GcHeader` (color, kind, next pointer), so
marking reads the header through the value pointer itself and the sweep
walks one linked list — no side tables, no per-object bookkeeping beyond
the header. Collection is paced two ways: an allocation-count threshold
(1024, raised to 2× the live count after each cycle, matching Lua's 200%
pause) and a byte threshold (1 MiB, same 2× rule) so a few large
allocations can't pile up dead memory waiting for the count.

It's stop-the-world: when a cycle triggers, the whole mark and sweep run
before the next instruction. The cost shows up exactly where you'd expect —
the 5M-short-lived-tables benchmark runs 3.24G cycles to Lua's 1.69G
(1.9×), and large heaps will see pauses Lua 5.4's generational collector
avoids. The reason it's still stop-the-world is that incremental marking
needs a write barrier on every table store, and `SetTable`/`SetField` is
already the hottest path in the VM — the barrier tax is paid by every
program, while pause time is only paid by programs with big heaps. That
trade is being revisited; the collector is being made incremental with the
barrier confined to the store path.

## No threaded dispatch

The interpreter loop is a `match` on the opcode. C interpreters get
computed goto (`&&label`) and can jump straight from one instruction's
tail to the next's head, replicating the dispatch at every call site so
the branch predictor sees a different indirect jump per opcode sequence.
Rust has no computed goto; the stable-Rust options are a `match` (one
indirect jump, shared dispatch site) or a function-pointer table (same
thing, worse inlining).

What the interpreter does instead is shrink what dispatch has to do:
opcodes decode operands lazily per arm, conditional ops fuse with their
following jump, compare+branch compiles to a single instruction, and the
register-window base pointer is cached across dispatch so the loop doesn't
recompute it. The remaining gap to Lua's dispatch is part of the 1.6× on
`fib(30)`; the fix would be a JIT, which is out of scope.

## Panic containment at the FFI boundary

Every `pub extern "C"` entry point wraps the VM call in `catch_unwind` and
converts a panic into a script error plus a poisoned flag on the VM. A
Rust panic unwinding into a C caller is undefined behavior, so the
boundary has to catch it regardless; the choice was to poison rather than
recover, because a panic means a VM invariant is broken and continuing
risks memory unsafety in the host. The cost is that one internal bug kills
the state permanently — which is the point.
