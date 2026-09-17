# umbra

A Lua-shaped scripting language written from scratch in Rust. Lexer, parser,
bytecode compiler, register VM, incremental garbage collector. About 11k lines,
no dependencies.

I wanted something a host program could embed through a C header the way you
embed Lua, with the parts of Lua I kept tripping over sanded off.

## Language

If you know Lua you know most of this. The syntax leans C:

- `let` / `var` bindings, `fn` functions and closures with proper upvalue
  semantics, implicit returns
- Tables with metatables and the usual metamethods (`__index`, `__newindex`,
  `__add`, `__call`, `__gc`, `__tostring`, weak `__mode`, and so on). Enough
  for prototype-style OOP.
- Coroutines (`coroutine.create/resume/status/wrap/isyieldable`, `yield()`)
- `switch`/`case`, ternary `cond ? a : b`, `++`/`--`, compound assignment
  (`+=`, `-=`, ...). `!=` is not-equal. `~=` is xor-assign.
- String interpolation: `"balance: ${account.balance}"`
- `<close>` variable attribute. `close()` runs when the scope exits, whether
  by fall-through, `break`, `continue`, `return`, or an error unwinding
  through it. The error is passed as close's second argument.
- `none` instead of `nil`. `not`/`and`/`or`. `@` line comments and
  `/* */` block comments.
- Integers and floats are distinct types. Integers are a full 64 bits (values
  past the NaN-box's 48-bit payload get heap-boxed, you never see it) and wrap
  on overflow like Lua's.
- Multiple returns. A trailing call or `...` in a return, argument list, or
  table constructor passes everything through. `return f(x)` is a real tail
  call: the frame is reused, so a tail-recursive loop doesn't grow the stack.
- `require` module system (`require "foo.bar"` loads `foo/bar.umbra`, cached)
- `goto`/labels, `pcall`/`xpcall` with line-attributed tracebacks

## Standard library

- `string` has `len sub rep upper lower reverse byte char format`, plus
  Lua-style pattern matching in `find match gmatch gsub`
- `string.pack` / `string.unpack` / `string.packsize` implement Lua 5.4's
  full format language (fixed-width and native ints, floats, fixed/length-
  prefixed/zero-terminated strings, endianness, `!`/`X` alignment). The
  output is byte-identical to real Lua's.
- `bytes` is a mutable byte buffer: `new from tostring concat len`, 1-based
  `b[i]` read and write, `#b`
- `table` has `insert remove concat sort pack unpack move`
- `io` has `open read write lines close` on file handles
- `os` has `time clock date getenv`. The clock is UTC only, so the `!`
  prefix on `os.date` does nothing and `os.time` ignores `isdst`.
- `utf8` has `char len codepoint codes offset`
- `math` has `floor ceil abs sqrt max min sin cos tan exp log modf random
  randomseed` and the constants
- `debug.traceback`, and the base functions: `print tostring tonumber type
  assert error pcall xpcall ipairs pairs unpack select setmetatable
  getmetatable rawget rawset rawequal require load`

## Architecture

```
source → lexer → parser (AST) → compiler → bytecode chunk → register VM → GC
```

- `src/lexer.rs` tokenizes. Interpolation gets lexed into parts the parser
  stitches back together.
- `src/parser.rs` is recursive descent with a Pratt loop for expressions.
  `switch` desugars to `if`/`else` right there in the parser.
- `src/compiler.rs` turns the AST into bytecode for a register VM
- `src/chunk.rs` is the bytecode chunk format: opcodes, constants, line table
- `src/vm.rs` holds the interpreter loop, metatables, coroutines, and the
  stdlib
- `src/gc.rs` is an incremental tri-color mark-and-sweep over strings, bytes,
  tables, closures, and bigints. Write barrier on stores, byte-paced steps,
  a host-settable cap on live objects.
- `src/value.rs` is the NaN-boxed value representation
- `src/api.rs` plus `umbra.h` is the C surface

Three knobs for hosts: an instruction-step budget (`umbra_set_step_limit`), a
hard cap on live GC objects (`umbra_set_max_objects`), and panic containment.
A Rust panic inside the VM gets caught and comes back as a script error
instead of unwinding into your C code.

Why things are built this way, with numbers, is in [DESIGN.md](DESIGN.md).

## Building

```sh
cargo build --release
```

You get `libumbra` as both a `cdylib` for C hosts and an `rlib` for Rust ones,
plus the `umbra` command-line binary. `build.rs` writes `umbra.h` by hand
because cbindgen doesn't understand Rust 2024's `#[unsafe(no_mangle)]` yet.

## Running scripts

```sh
umbra file.umbra [args...]   # run a script; args land in the global `arg` table
umbra -e 'print(1 + 1)'      # evaluate a string
umbra                        # REPL: expressions print their value, Ctrl-D exits
```

`arg` works like Lua's. `arg[0]` is the script path, `arg[1..]` the arguments,
`arg[-1]` the interpreter. Errors go to stderr with exit code 1.

```sh
cargo run --release -- example/nqueens.umbra
```

## Embedding from C

`umbra.h` is a small stack API modeled on Lua's:

```c
umbra_State *U = umbra_newstate();
umbra_register(U, "print", my_print);          /* umbra_CFunction */

if (umbra_dostring(U, src) != UMBRA_OK)
    fprintf(stderr, "error: %s\n", umbra_tostring(U, -1));

umbra_pushstring(U, "arg");
umbra_getglobal(U, "my_fn");
umbra_pcall(U, 1, 0);                          /* fn("arg"), protected */

umbra_close(U);
```

`example/host.c` is a whole host in one file. It registers `print`, loads a
script, runs it. The Makefile in `example/` builds it against the cdylib:

```sh
cargo build --release  # produces target/release/libumbra.so
cd example && make run-word-count
```

## Example scripts

Runnable programs in `example/`. Try `umbra example/nqueens.umbra`.

- `bank_account.umbra` does prototype OOP: classes, inheritance, method
  calls, string interpolation
- `scheduler.umbra` is a cooperative round-robin task scheduler on coroutines
- `word_count.umbra` uses `io.open` with `<close>`, `string.gmatch` patterns,
  `table.sort`
- `nqueens.umbra` is recursive backtracking over a shared table
- `dijkstra.umbra` runs shortest paths on a seeded graph with a hand-rolled
  binary-heap priority queue
- `json.umbra` is a serializer and recursive-descent parser roundtrip.
  Patterns, `__tostring`.
- `mandel.umbra` renders a 200x200 ASCII Mandelbrot. Float arithmetic in
  nested loops.

## Tests

```sh
cargo test
```

370 tests over the lexer, parser, value representation, VM semantics, GC
behavior (finalizers, weak tables, coroutines), panic containment, the C API,
and the long tail of stdlib edge cases.

## Performance

Release build, cycles on one pinned core (`perf stat -e cycles`, mean of 5),
against PUC Lua 5.4:

| | umbra | lua 5.4 | |
|---|---|---|---|
| `fib(30)` — call overhead | 262M | 164M | 1.6× |
| 2M array writes + reads | 306M | 134M | 2.3× |
| 200k string concat + `gmatch` | 269M | 387M | 0.7× |
| 5M short-lived tables, 100k live | 2.86G | 1.25G | 2.3× |

The bytecode has the superinstructions Lua 5.4 added: immediate-operand
arithmetic and compares, `GetField`, `SelfOp`, tail calls. Plus a per-callsite
global cache Lua doesn't have. What's left is representation. NaN-boxing
range-checks every integer result against the 48-bit inline payload, where
Lua's 16-byte `TValue` just stores the `i64`. And `match` dispatch can't do
the computed-goto threading a C interpreter gets. No JIT.

## Differences from Lua

These are on purpose and won't change.

- **Strings are UTF-8, not bytes.** `string.char(200)` gives you a two-byte
  character. `string.sub` and `reverse` slice on bytes and re-validate. Binary
  data goes in the separate `bytes` type: `string.pack` returns bytes,
  `string.unpack` takes bytes or strings, `io.open(path, "rb")` reads bytes
  and `f:write` accepts them.
- **NaN is `none`.** The value representation is NaN-boxed, so every NaN bit
  pattern is already spoken for. `0/0` gives `none`.
- **`none` instead of `nil`. `!=` is not-equal. `~=` is xor-assign.**
- **No `dofile`.** `load` compiles strings and `require` loads files. That is
  the entire filesystem surface for code.

## Limitations

- `yield` can't cross a `pcall`, metamethod, or `table.sort` comparator. You
  get "attempt to yield across a C-call boundary". Lua 5.1 had the same rule.
  5.2 lifted it with `lua_pcallk` continuation-passing, which this VM doesn't
  do.
- The GC is incremental in the Lua 5.1 through 5.3 style. Mark and sweep run
  in small byte-paced steps between bursts of your code, so pauses stay
  bounded as the heap grows. It isn't generational, and Lua 5.4's generational
  mode still wins on allocation-heavy churn.
- Expressions and blocks nest 100 levels deep at most. Past that you get a
  parse error ("expected fewer nesting levels") instead of a stack overflow.

## Sandboxing

`require`, `io`, and `os` touch the real filesystem and environment, same as
Lua's. For untrusted code, clear them out of the globals first:

```c
umbra_pushnil(U); umbra_setglobal(U, "io");
umbra_pushnil(U); umbra_setglobal(U, "os");
umbra_pushnil(U); umbra_setglobal(U, "require");
```

Then bound CPU and memory with `umbra_set_step_limit` and
`umbra_set_max_objects`. A panic inside the VM comes back as a script error.
It never unwinds into the host.

## License

MIT. See [LICENSE](LICENSE).
