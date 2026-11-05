# quill

[![tests](https://github.com/udaysehgal/quill/actions/workflows/test.yml/badge.svg)](https://github.com/udaysehgal/quill/actions/workflows/test.yml)

A compiler from Scheme to native ARM64 machine code, written in Rust.

**[Try it in your browser](https://udaysehgal.github.io/quill/)**: the playground runs the compiler as WebAssembly and shows the assembly and macro expansion live as you type.

quill turns a Scheme program straight into Apple Silicon assembly. It doesn't use LLVM or an interpreter. The code generator, garbage collector and runtime are each small enough to read in one sitting.

```scheme
(define (make-counter)
  (let ([n 0])
    (lambda () (set! n (add1 n)) n)))

(define-syntax while
  (syntax-rules ()
    [(_ c body ...) (let loop () (when c body ... (loop)))]))

(let ([count (make-counter)] [i 0])
  (while (< i 3) (count) (set! i (add1 i)))
  `(counted ,(count) times))       ; => (counted 4 times)
```

## Quick start

You need macOS on Apple Silicon, [Rust](https://rustup.rs), and the Xcode command line tools (`xcode-select --install`). quill uses `clang` to assemble and link.

```sh
cargo build --release
./target/release/quill examples/queens.scm -o queens
./queens
```

```
quill <file.scm> [-o output] [-S | -E]
  -o output   name of the executable (default a.out)
  -S          print the generated assembly instead
  -E          print the program after macro expansion (core forms)
```

The program's top-level forms run in order. If the last expression has a value, it is printed.

## The language

**Continuations:** first-class, re-entrant `call/cc` (also `call-with-current-continuation`) and `dynamic-wind`. Generators, coroutines, early exits and backtracking (`amb`) all work. See [how they're implemented](#how-call-cc-works).

**Core:** `define`, `lambda` (with rest arguments: `(lambda (a . rest) ...)`), `let`, `let*`, `letrec`, named `let`, `set!`, `if`, `cond`, `case`, `when`, `unless`, `and`, `or`, `begin`, `do`, `quote`, quasiquote (`` ` `` `,` `,@`), internal definitions, and proper tail calls.

**Macros:** `define-syntax` / `syntax-rules`, with literals, nested ellipses and dotted patterns. Macros are partly hygienic: names that a template binds itself, like `tmp` in a `swap!` macro, are renamed on every expansion. This means they can't capture the user's variables.

**Data:**
- integers (62-bit; overflow is an error, not a silent wraparound)
- booleans and characters (Unicode)
- strings
- symbols (interned, including ones made by `string->symbol`)
- pairs and lists
- vectors (including `#(...)` literals)
- procedures

**Library:** these are inlined as machine code:
- arithmetic and comparisons
- type predicates
- pair, vector, string and character operations
- `apply`
- `display`, `write`, `newline`
- `error`, `exit`

Another ~70 functions are written in Scheme in [`src/prelude.scm`](src/prelude.scm), for example `map`, `for-each`, `filter`, `fold-left`, `append`, `sort`, `assoc`, `equal?`, `string-append`, `number->string`, `string->number` and `vector-map`. Only the functions a program actually uses are compiled into it. A user definition with the same name takes precedence.

**Errors** include the source location:

```
error: prog.scm:2: car: expected a pair, got 5
quill: prog.scm:4: unbound variable: foo
error: stack overflow (recursion too deep; set QUILL_STACK_MB for a bigger stack)
```

## How it works

```
source ──reader──▶ syntax tree ──expander──▶ core forms ──compiler──▶ ARM64 assembly ──clang──▶ executable
                   (with lines)   (macros,      (9 forms)    (+ runtime.c: GC, printing)
                                  derived syntax)
```

- **[`src/reader.rs`](src/reader.rs)** parses the source text and records the line each list starts on.
- **[`src/expand.rs`](src/expand.rs)** rewrites everything into nine core forms. This covers `syntax-rules` macros, `cond`/`case`/`do`/named `let`, quasiquote, and internal `define`s (which become `letrec*`).
- **[`src/compiler.rs`](src/compiler.rs)** generates code from the core forms. [`compiler/primitives.rs`](src/compiler/primitives.rs) inlines the built-in operations, with their type checks.
- **[`src/lib.rs`](src/lib.rs)** exposes the pipeline as a library. The command-line tool and the [browser playground](playground/) both use it.
- **[`runtime/runtime.c`](runtime/runtime.c)** is the garbage collector, the printer, error reporting, symbol interning and stack setup.

Design notes (more detail in the comments at the top of `compiler.rs`):

- **Tagged values.** Every value is one 64-bit word. Integers are stored shifted left by 2, so `+` is a single `adds` instruction followed by an overflow check. Pointers carry a 3-bit type tag (pair, closure, symbol, vector or string). Characters, booleans, `()` and void are distinct constant patterns.
- **Closures.** A closure is a record holding a code pointer plus the values it captured. A variable that is ever `set!` lives in a heap box, so every closure that captured it sees the change. A lambda that captures nothing becomes a static object and is never allocated.
- **Calling convention.** The first 8 arguments go in `x0`–`x7`; the rest go in an argument buffer. The argument count is passed in `x9`, so the callee can check it. Calls in tail position tear down the caller's frame and jump to the callee, so loops written as recursion run in constant stack space.
- **Garbage collection.** The collector copies live data between two heap areas and grows the heap when it is more than half full. Its roots are the global variables plus every stack slot of every frame, which it finds by walking the chain of saved frame pointers. This works because function entry zeroes the frame's slots and live values are always in slots whenever allocation can happen.
- **Continuations.** See [below](#how-call-cc-works).
- **Stack.** Programs run on their own 1 GB stack (only the pages actually used take memory), with a guard region at the bottom. Recursion a million calls deep works, and running out gives a clear error message instead of a crash.

## How call/cc works

`call/cc` copies the whole Scheme stack into the heap, and invoking the continuation copies it back. The core is about 80 lines of hand-written assembly (`scm_callcc` and `scm_cont_entry` in [`compiler.rs`](src/compiler.rs)).

- **Capture.** `scm_callcc` allocates a vector and copies every word from its caller's `sp` up to the stack base into it. The continuation is a closure that holds this copy plus the caller's `sp`, frame pointer and return address. `f` is then tail-called with it, so an ordinary return from `f` also goes straight to the caller.
- **Invoke.** The continuation's code sets `sp`, copies the saved words back to the same addresses and jumps to the saved return address, with the value in `x0`. The copy loop uses only registers, because the region it overwrites may include its own frame.
- **The GC needed no changes.** The saved stack is an ordinary vector. Each word in it is either a tagged value or a saved frame pointer or return address. Those are 16- and 4-byte aligned, so their low bits make them look like fixnums, and the collector skips them. It still updates every real pointer inside a saved stack when it moves objects.
- **dynamic-wind** is written in Scheme ([`prelude.scm`](src/prelude.scm)). A global list records the `before`/`after` thunks we're inside. Before a jump, the continuation runs the `after`s of the extents it leaves and the `before`s of the ones it re-enters, using their common tail.

The trade-off is that capturing costs time and memory in proportion to stack depth. That's fine for generators and early exits, but slow for a continuation captured inside a very deep recursion. Production Schemes such as Chez use a segmented stack, so capture is O(1) and frames are copied back lazily, one segment at a time.

## Design trade-offs

| Decision | Why | Cost |
|---|---|---|
| Direct-to-assembly code generation, no IR | Every construct maps to code you can read in the `-S` output | No optimization passes: values go through stack slots even when a register would do |
| Every live value in a stack slot when allocation can happen | The GC finds all roots by walking frames, with no stack maps | More loads and stores than a register allocator would produce |
| Copying GC | Allocation is a pointer bump plus one compare, and fragmentation is impossible | Needs twice the live data in memory during a collection |
| 62-bit fixnums that trap on overflow | `+` is one `adds` plus a branch, and results are never silently wrong | No bignums yet |
| Stack-copying `call/cc` | Simple, and needed no changes to the calling convention or GC | Capture is O(stack depth) |
| Prelude written in Scheme, tree-shaken | The library is short, and unused functions cost nothing | Library code runs without the inlined fast paths primitives get |

## Tests

```sh
cargo test          # reader unit tests
./tests/run.sh      # ~375 end-to-end tests: compile, run, compare output
```

Every end-to-end test runs twice: once normally, and once starting from a 1 KB heap. With such a small heap the garbage collector runs on almost every allocation, so a missed root shows up quickly.

Environment variables for compiled programs:
- `QUILL_HEAP_KB`: starting heap size.
- `QUILL_STACK_MB`: stack size (default 1024).
- `QUILL_GC_STATS=1`: print collection statistics when the program exits.

## Not supported (yet)

- Floats, big integers and rationals.
- Reading input, ports and files.
- `define-record-type`.
- Fully hygienic macros, local macros (`let-syntax`), and macros that expand into `define-syntax`.
- Platforms other than macOS on ARM64.
