# Design notes

Why umbra is built the way it is and what each choice costs. Cycle counts come
from the table in [README.md](README.md#performance): release build,
`perf stat -e cycles` on one pinned core, against PUC Lua 5.4.

## NaN-boxed values instead of a tagged union

Every `Value` is one `u64`. Doubles pass through bit for bit. Everything else
lives in NaN space, with a 12-bit signature, a 4-bit tag, and a 48-bit payload
holding a heap pointer, a small integer, or a bool/none discriminant.

Lua does it the other way. Its `TValue` is a 16-byte struct, payload plus type
tag, because once the payload is a pointer the tag needs its own word. Halving
that to 8 bytes buys three things. Register windows, table arrays, and the
stack are all `Vec<Value>`, so a cache line holds 8 values instead of 4 and a
typical function's register file fits in one or two lines. Table density
doubles, since the open-addressed `KeyMap` stores `(hash, key, value)` triples
and the value half is now 8 bytes. And copying a value is a single register
move with no tag word to keep in sync.

The cost shows up in the numbers. A 48-bit payload can't hold every `i64`, so
every instruction that produces an integer range-checks the result against
`INLINE_INT_MIN..=INLINE_INT_MAX` and heap-boxes anything outside as a
`GcBigInt`. Lua just stores the `i64`. That check is most of why `fib(30)`
takes 262M cycles to Lua's 164M. fib is integer arithmetic and calls and
nothing else, so it pays on every operation. The table benchmark (306M vs
134M) pays again on every index.

There's a second cost. NaN can't be a value, because all the NaN bit patterns
are the box. `0/0` produces `none` instead of a float NaN. I took that trade
for the representation.

## Register VM instead of a stack machine

The bytecode names registers directly, like Lua's. `Add r1, r2, r3`. So
`a + b * c` compiles to two instructions instead of a push/pop sequence. Fewer
instructions means fewer dispatches, and dispatch is the one thing a bytecode
interpreter can never make free.

What it costs is the compiler. It does register allocation, has to model
multi-value calls and varargs spilling past the window, and every opcode
carries operand fields that get packed on emit and decoded on dispatch. A
stack VM is a much easier target. It also runs roughly 2 to 3 times the
instruction count for the same program, and no dispatch trick wins that back.

## `none` instead of `nil`

Readability, nothing deeper. `none` reads as "no value" to someone who has
never seen Lua. `nil` reads as a proper noun you have to look up. The runtime
still calls the type `nil` internally, and `tostring(none)` prints `nil`,
because every GC and VM comment already used that word and renaming the
internals would have been churn for no gain. The keyword is the user-facing
part.

## `!=` for not-equal, `~=` for xor-assign

The syntax looks like C, and in C `!=` is not-equal. Keeping Lua's `~=` for
inequality in a language that otherwise reads as C would be a permanent typo
generator. Everyone would write `!=` first. With `~=` freed up, `~` does what
it does in C: binary xor, and `~=` is xor-assign. The real cost is that
pasted Lua code with `~=` in it silently changes meaning. I accepted that.
umbra was never trying to be source-compatible with Lua.

## UTF-8 strings, with a separate bytes type

Strings are UTF-8 text. `string.char(200)` produces the two-byte `U+00C8`,
`utf8.*` iterates codepoints, and `string.sub`/`reverse` slice on bytes but
re-validate so you can't split a multi-byte character and keep the halves.

This is the choice Swift, Rust, and Python 3 made, and it's the right one for
a scripting layer where text is the common case and mojibake is the common
bug. But it leaves nowhere for raw bytes to live, and `string.pack`, binary
file I/O, and network framing all need raw bytes. So there's a `bytes` type:
a mutable buffer with `bytes.*` functions, 1-based indexing, and `#`.
`string.pack` returns one, `string.unpack` accepts one or a string,
`io.open(path, "rb")` reads them and `f:write` takes them. Adding the type
meant widening the NaN-box tag from 3 bits to 4, since all 8 tags were
already spoken for. That widened the signature check by two ALU ops. I
measured it and couldn't see it.

The pack format itself is Lua 5.4's, alignment and endianness included, and
its output is byte-identical to real Lua's across 51 differential cases.

## Incremental GC

The collector is tri-color mark-and-sweep over intrusive object headers.
Every heap object starts with a `GcHeader` holding color, kind, and a next
pointer, so marking reads the header through the value pointer itself and
the sweep walks one linked list. No side tables. No per-object bookkeeping
beyond the header.

Marking is incremental in the Lua 5.1 through 5.3 style, a
Pause/Propagate/Atomic/Sweep state machine stepped at allocation checkpoints
and paced by bytes. The price is a write barrier on every table, upvalue,
and global store. Store a white object into a black container and the marker
would miss it, so the barrier re-grays the container. That barrier sits on
`SetTable` and `SetField`, already the hottest path in the VM, which is why
the collector started life stop-the-world. Every program pays the barrier
tax, while only programs with big heaps pay for pauses. The
5M-short-lived-tables benchmark, at 3.24G cycles to Lua's 1.69G back when it
was stop-the-world, made the pause cost real enough to pay the tax.

The stepmul is 3200, not Lua's default 200. I measured 200, 400, 800, 1600,
and 3200. Higher stepmul shrinks the allocation span a cycle covers, and the
span is what bounds in-flight garbage. At 3200 the largest single step in a
5M-allocation churn is about 1.5% of a full cycle's work, and RSS on that
churn dropped from 53MB to 46MB against the stop-the-world version.

## No threaded dispatch

The interpreter loop is a `match` on the opcode. C interpreters get computed
goto (`&&label`) and jump straight from one instruction's tail to the next's
head, with the dispatch replicated at every call site so the branch predictor
sees a distinct indirect jump per opcode pair. Rust has no computed goto. On
stable Rust you get a `match`, which is one indirect jump at one shared site,
or a function-pointer table, which is the same thing with worse inlining.

So the loop shrinks what dispatch has to do instead. Operands decode lazily
per arm. Conditional ops fuse with the jump that follows them. Compare-and-
branch is a single instruction. The register-window base pointer is cached
across dispatch. I also tried caching `pc` and the step counter in locals the
way Lua's `savepc` discipline does, five different ways, and every one of
them made instruction count go up. LLVM's register allocator is already at
capacity for a 60-arm match, and every value you pin costs a spill somewhere
else. The remaining gap to Lua's dispatch is part of the 1.6× on `fib(30)`.
Closing it means a JIT, which I'm not building.

## Panic containment at the FFI boundary

Every `pub extern "C"` entry point wraps its VM call in `catch_unwind` and
turns a panic into a script error plus a poisoned flag on the state. A Rust
panic unwinding into a C caller is undefined behavior, so the boundary has to
catch it no matter what. The question was whether to poison or recover. A
panic means a VM invariant broke, and continuing risks memory unsafety in the
host, so it poisons. One internal bug kills that state for good. That's the
point.
