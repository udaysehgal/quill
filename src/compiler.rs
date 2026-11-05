//! Code generation: core forms -> ARM64 assembly (Apple / Mach-O syntax).
//! The expander (`expand.rs`) has already rewritten everything else.
//!
//! # Value representation (64-bit tagged words)
//!
//! | type        | encoding                              | low bits |
//! |-------------|---------------------------------------|----------|
//! | fixnum      | `n << 2`                              | `..00`   |
//! | pair        | pointer + 1  -> `[car, cdr]`          | `001`    |
//! | closure     | pointer + 2  -> `[code, nfree, fv..]` | `010`    |
//! | symbol      | pointer + 3  -> `[len, bytes..]`      | `011`    |
//! | vector      | pointer + 5  -> `[len, elems..]`      | `101`    |
//! | string      | pointer + 6  -> `[len, bytes..]`      | `110`    |
//! | char        | `c << 8 \| 0x0F`                      | `0x0F`   |
//! | `#f` / `#t` | `0x2F` / `0x6F`                       |          |
//! | `()`        | `0x3F`                                |          |
//! | void        | `0x4F` (result of `set!`, `display`…) |          |
//!
//! Heap objects are 16-byte aligned; `len` fields are fixnums. Symbols, string
//! literals and closures without free variables are static data, never on
//! the heap.
//!
//! # Registers
//! - `x0`      accumulator: every expression leaves its result here
//! - `x0..x7`  the first 8 arguments of a call; the rest go in `scm_argbuf[8..]`
//! - `x8`      the closure being called;  `x9` the argument count
//! - `x10`     allocation size for `scm_alloc`;  `x1..x4`, `x9..x16` scratch
//! - `x27`     heap limit;  `x28` heap pointer (bump allocator)
//! - `x29`     frame pointer;  `x30` return address
//!
//! # Stack frames
//! ```text
//!   x29 -> | saved x29, x30 |
//!          | slot N-1       |
//!          | ...            |
//!   sp  -> | slot 0         |   slot k lives at sp + 8*k
//! ```
//! Parameters (and the closure, if it has free variables) are copied into the
//! first slots on entry, before anything can allocate. Slots hold only valid
//! values (the prologue zeroes them), and every live value is in a slot
//! whenever allocation can happen, so the garbage collector treats every
//! slot of every frame as a root, walking frames through the saved-x29 chain.
//!
//! # Variables
//! A variable is a stack slot or a field of the current closure. Variables
//! that are ever `set!` live in a heap "box" (a pair whose car is the value),
//! so closures that capture them share the box.
//!
//! # Tail calls
//! A call in tail position tears down the current frame and branches to the
//! callee, so the callee returns straight to our caller.

use crate::expand::Expander;
use crate::reader::{self, Expr, Line};
use crate::syntax::*;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

pub const FIXNUM_SHIFT: u32 = 2;
pub const CHAR_SHIFT: u32 = 8;
pub const CHAR_TAG: i64 = 0x0F;
pub const FALSE: i64 = 0x2F;
pub const TRUE: i64 = 0x6F;
pub const EMPTY: i64 = 0x3F;
pub const VOID: i64 = 0x4F;
pub const UNBOUND: i64 = 0x5F;
pub const PAIR_TAG: i64 = 1;
pub const CLOSURE_TAG: i64 = 2;
pub const SYMBOL_TAG: i64 = 3;
pub const VECTOR_TAG: i64 = 5;
pub const STRING_TAG: i64 = 6;

const FIXNUM_MAX: i64 = (1 << 61) - 1;
const FIXNUM_MIN: i64 = -(1 << 61);
/// Stack slots per frame (8 MB).
const MAX_SLOTS: usize = 1 << 20;
/// Size of `scm_argbuf`: the most arguments `apply` can pass.
pub const ARGBUF_SIZE: usize = 1 << 16;
const REG_ARGS: usize = 8;

const PRELUDE: &str = include_str!("prelude.scm");

#[derive(Clone, Debug)]
enum Loc {
    Slot(usize),
    Free(usize),
}

#[derive(Clone, Debug)]
struct Local {
    name: String,
    loc: Loc,
    boxed: bool,
}

/// Lexical environment of the current function, searched from the end so
/// inner bindings shadow outer ones.
type Env = Vec<Local>;

fn lookup<'a>(env: &'a Env, name: &str) -> Option<&'a Local> {
    env.iter().rev().find(|l| l.name == name)
}

enum Global {
    /// `(define (f ...) ...)`: called directly; as a value it's a static closure
    Func {
        label: String,
        closure: String,
        arity: usize,
        rest: bool,
    },
    /// `(define x ...)`: a word in the data section
    Var { label: String },
}

/// Code generation state for the function currently being compiled.
#[derive(Default)]
struct Frame {
    out: String,
    max_slots: usize,
    closure_slot: Option<usize>,
}

struct ErrorStub {
    label: String,
    message: String,
    /// instructions that put the offending value in x1
    culprit: String,
}

#[derive(Default)]
struct Compiler {
    f: Frame,
    /// finished functions
    text: String,
    /// static objects: closures, strings, symbols
    data: String,
    /// global variables (GC roots)
    globals_data: String,
    labels: usize,
    globals: HashMap<String, Global>,
    symbols: HashMap<String, String>,
    errors: Vec<ErrorStub>,
    prim_closures: HashMap<String, String>,
    /// source location for error messages
    file: String,
    line: u32,
}

macro_rules! emit {
    ($c:expr, $($arg:tt)*) => {{
        $c.f.out.push_str("    ");
        writeln!($c.f.out, $($arg)*).unwrap();
    }};
}

// after `emit!` so the submodule can use it
mod primitives;
pub use primitives::is_primitive;

/// Encode a literal as a tagged word, if it is an immediate.
fn immediate(e: &Expr) -> Result<Option<i64>> {
    Ok(Some(match e {
        Expr::Int(n) => {
            if !(FIXNUM_MIN..=FIXNUM_MAX).contains(n) {
                return Err(format!("integer {n} doesn't fit in a 62-bit fixnum"));
            }
            n << FIXNUM_SHIFT
        }
        Expr::Bool(false) => FALSE,
        Expr::Bool(true) => TRUE,
        Expr::Char(c) => ((*c as i64) << CHAR_SHIFT) | CHAR_TAG,
        Expr::List(items, _) if items.is_empty() => EMPTY,
        _ => return Ok(None),
    }))
}

fn arity_text(n: usize, rest: bool) -> String {
    if rest {
        format!("at least {n}")
    } else {
        n.to_string()
    }
}

/// Every symbol in `e`, including quoted ones (for deciding what prelude code to include).
fn all_symbols(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Symbol(s) => {
            out.insert(s.clone());
        }
        Expr::List(items, _) | Expr::Vector(items) => {
            items.iter().for_each(|i| all_symbols(i, out))
        }
        _ => {}
    }
}

/// Symbols that may refer to variables, in first-use order (skips quoted data).
fn referenced_symbols(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Symbol(s) if !out.contains(s) => out.push(s.clone()),
        Expr::List(items, _) if items.first() == Some(&sym("quote")) => {}
        Expr::List(items, _) => items.iter().for_each(|i| referenced_symbols(i, out)),
        _ => {}
    }
}

/// Could `name` be the target of a `set!` in `body`? (Conservative: ignores
/// shadowing, which can only cause an unnecessary box.)
fn is_assigned(name: &str, body: &[Expr]) -> bool {
    body.iter().any(|e| match e {
        Expr::List(items, _) => match items.as_slice() {
            [Expr::Symbol(q), ..] if q == "quote" => false,
            [Expr::Symbol(s), Expr::Symbol(target), ..] if s == "set!" && target == name => true,
            _ => is_assigned(name, items),
        },
        _ => false,
    })
}

/// Escape a string for `.asciz`.
fn asm_string(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(b as char),
            _ => write!(out, "\\{b:03o}").unwrap(),
        }
    }
    out
}

/// Internal aliases: `%cons` is `cons` even if the user rebinds `cons`.
fn primitive_name(name: &str) -> Option<&str> {
    if is_primitive(name) {
        return Some(name);
    }
    name.strip_prefix('%').filter(|p| is_primitive(p))
}

/// The user-visible name of a variable (fresh names from the expander are `name N`).
fn display_name(name: &str) -> &str {
    name.split(' ').next().unwrap_or(name)
}

impl Compiler {
    fn fresh(&mut self, prefix: &str) -> String {
        self.labels += 1;
        format!("{prefix}{}", self.labels)
    }

    fn label(&mut self, l: &str) {
        writeln!(self.f.out, "{l}:").unwrap();
    }

    // ───────────────────────────── stack slots ─────────────────────────────

    fn reserve(&mut self, k: usize) -> Result<()> {
        if k >= MAX_SLOTS {
            return Err("function needs too many stack slots".into());
        }
        self.f.max_slots = self.f.max_slots.max(k + 1);
        Ok(())
    }

    /// Address of slot `k`. Big offsets go through x16.
    fn slot_mem(&mut self, k: usize) -> String {
        let off = k * 8;
        if off <= 32760 {
            format!("[sp, #{off}]")
        } else {
            emit!(self, "add x16, sp, #{}, lsl #12", off >> 12);
            format!("[x16, #{}]", off & 0xfff)
        }
    }

    fn store_slot(&mut self, reg: &str, k: usize) -> Result<()> {
        self.reserve(k)?;
        let m = self.slot_mem(k);
        emit!(self, "str {reg}, {m}");
        Ok(())
    }

    fn load_slot(&mut self, reg: &str, k: usize) {
        let m = self.slot_mem(k);
        emit!(self, "ldr {reg}, {m}");
    }

    /// Load any 64-bit constant into `reg` using movz/movk.
    fn load_imm(&mut self, reg: &str, value: i64) {
        let v = value as u64;
        emit!(self, "movz {reg}, #{}", v & 0xffff);
        for shift in [16, 32, 48] {
            let chunk = (v >> shift) & 0xffff;
            if chunk != 0 {
                emit!(self, "movk {reg}, #{chunk}, lsl #{shift}");
            }
        }
    }

    /// `reg` = address of `label` + `tag`
    fn load_address(&mut self, reg: &str, label: &str, tag: i64) {
        emit!(self, "adrp {reg}, {label}@PAGE");
        emit!(self, "add {reg}, {reg}, {label}@PAGEOFF");
        if tag != 0 {
            emit!(self, "add {reg}, {reg}, #{tag}");
        }
    }

    /// After a `cmp`, turn condition `cond` into `#t`/`#f` in x0.
    fn emit_bool(&mut self, cond: &str) {
        emit!(self, "cset x0, {cond}"); // x0 = 0 or 1
        emit!(self, "lsl x0, x0, #6"); //  x0 = 0 or 0x40
        emit!(self, "add x0, x0, #{FALSE}"); // x0 = 0x2F or 0x6F
    }

    /// Pop the current frame (leaves x30 = our return address).
    fn emit_teardown(&mut self) {
        emit!(self, "mov sp, x29");
        emit!(self, "ldp x29, x30, [sp], #16");
    }

    /// If `tail`, return x0 from the current function.
    fn ret_if(&mut self, tail: bool) {
        if tail {
            self.emit_teardown();
            emit!(self, "ret");
        }
    }

    /// Label of a stub that reports `message` (with the current source
    /// location) plus the value `culprit` puts in x1, then exits.
    fn error_label(&mut self, message: &str, culprit: &str) -> String {
        let message = if self.line > 0 {
            format!("{}:{}: {message}", self.file, self.line)
        } else {
            message.to_string()
        };
        if let Some(e) = self
            .errors
            .iter()
            .find(|e| e.message == message && e.culprit == culprit)
        {
            return e.label.clone();
        }
        let label = format!("Lerr{}", self.errors.len());
        self.errors.push(ErrorStub {
            label: label.clone(),
            message,
            culprit: culprit.into(),
        });
        label
    }

    /// x0 = raw pointer to `bytes` (a multiple of 16) of fresh heap memory.
    /// May run the GC: every live value must be in a stack slot.
    fn alloc(&mut self, bytes: usize) {
        self.load_imm("x10", bytes as i64);
        emit!(self, "bl scm_alloc");
    }

    /// Replace the value in slot `k` with a box containing it.
    fn box_slot(&mut self, k: usize) -> Result<()> {
        self.alloc(16);
        self.load_slot("x1", k);
        emit!(self, "str x1, [x0]");
        self.load_imm("x1", EMPTY);
        emit!(self, "str x1, [x0, #8]");
        emit!(self, "add x0, x0, #{PAIR_TAG}");
        self.store_slot("x0", k)
    }

    // ───────────────────────────── static data ─────────────────────────────

    fn static_bytes(&mut self, prefix: &str, bytes: &[u8]) -> String {
        let label = self.fresh(prefix);
        writeln!(
            self.data,
            "    .p2align 4\n{label}:\n    .quad {}",
            (bytes.len() as i64) << FIXNUM_SHIFT
        )
        .unwrap();
        for chunk in bytes.chunks(16) {
            let list: Vec<String> = chunk.iter().map(|b| b.to_string()).collect();
            writeln!(self.data, "    .byte {}", list.join(",")).unwrap();
        }
        label
    }

    fn symbol_label(&mut self, name: &str) -> String {
        if let Some(l) = self.symbols.get(name) {
            return l.clone();
        }
        let label = self.static_bytes("scm_sym", name.as_bytes());
        self.symbols.insert(name.to_string(), label.clone());
        label
    }

    fn static_closure(&mut self, code: &str) -> String {
        let label = self.fresh("scm_clo");
        writeln!(
            self.data,
            "    .p2align 4\n{label}:\n    .quad {code}\n    .quad 0"
        )
        .unwrap();
        label
    }

    // ───────────────────────────── expressions ─────────────────────────────

    /// Compile `e` so its value ends up in x0. `si` is the next free stack slot.
    /// If `tail`, `e` is the last thing the function does, so return its value.
    fn compile(&mut self, e: &Expr, env: &Env, si: usize, tail: bool) -> Result<()> {
        if let Expr::List(items, Line(line)) = e
            && !items.is_empty()
        {
            let outer = self.line;
            if *line > 0 {
                self.line = *line;
            }
            let r = self
                .compile_list(items, env, si, tail)
                .map_err(|err| at_line(self.line, err));
            self.line = outer;
            return r;
        }
        if let Some(imm) = immediate(e)? {
            self.load_imm("x0", imm);
            self.ret_if(tail);
            return Ok(());
        }
        match e {
            Expr::Symbol(name) => self.compile_variable(name, env)?,
            Expr::Str(s) => {
                let label = self.static_bytes("scm_str", s.as_bytes());
                self.load_address("x0", &label, STRING_TAG);
            }
            Expr::Vector(_) => self.compile_quoted(e, si)?,
            _ => unreachable!("immediates and lists handled above"),
        }
        self.ret_if(tail);
        Ok(())
    }

    fn compile_list(&mut self, items: &[Expr], env: &Env, si: usize, tail: bool) -> Result<()> {
        let (op, args) = items.split_first().unwrap();
        if let Expr::Symbol(head) = op {
            if SPECIAL_FORMS.contains(&head.as_str()) {
                return self.compile_special(head, args, env, si, tail);
            }
            if lookup(env, head).is_none() {
                match self.globals.get(head) {
                    Some(Global::Func {
                        label, arity, rest, ..
                    }) => {
                        let (label, arity, rest) = (label.clone(), *arity, *rest);
                        return self
                            .compile_direct_call(head, &label, arity, rest, args, env, si, tail);
                    }
                    Some(Global::Var { .. }) => {}
                    None if head == "apply" => return self.compile_apply(args, env, si, tail),
                    None => {
                        if let Some(prim) = primitive_name(head) {
                            self.compile_primitive(prim, args, env, si)?;
                            self.ret_if(tail);
                            return Ok(());
                        }
                    }
                }
            }
        }
        self.compile_call(op, args, env, si, tail)
    }

    /// Load a local's slot or closure field into `reg`, without unboxing.
    fn load_raw(&mut self, local: &Local, reg: &str) {
        match local.loc {
            Loc::Slot(k) => self.load_slot(reg, k),
            Loc::Free(i) => {
                let cs = self
                    .f
                    .closure_slot
                    .expect("free variable without a closure");
                self.load_slot(reg, cs);
                emit!(self, "sub {reg}, {reg}, #{CLOSURE_TAG}");
                emit!(self, "ldr {reg}, [{reg}, #{}]", 16 + 8 * i);
            }
        }
    }

    fn compile_variable(&mut self, name: &str, env: &Env) -> Result<()> {
        if let Some(local) = lookup(env, name) {
            self.load_raw(local, "x0");
            if local.boxed {
                emit!(self, "ldur x0, [x0, #-{PAIR_TAG}]");
            }
            return Ok(());
        }
        match self.globals.get(name) {
            Some(Global::Var { label }) => {
                let label = label.clone();
                emit!(self, "adrp x0, {label}@PAGE");
                emit!(self, "ldr x0, [x0, {label}@PAGEOFF]");
                let err =
                    self.error_label(&format!("{name}: variable used before its definition"), "");
                emit!(self, "cmp x0, #{UNBOUND}");
                emit!(self, "b.eq {err}");
                Ok(())
            }
            Some(Global::Func { closure, .. }) => {
                let closure = closure.clone();
                self.load_address("x0", &closure, CLOSURE_TAG);
                Ok(())
            }
            None => {
                // variadic primitives and apply as values are library functions (%+ etc.)
                let lib = format!("%{name}");
                if let Some(Global::Func { closure, .. }) = self.globals.get(&lib) {
                    let closure = closure.clone();
                    self.load_address("x0", &closure, CLOSURE_TAG);
                    return Ok(());
                }
                if let Some(prim) = primitive_name(name) {
                    let closure = self.primitive_closure(prim)?;
                    self.load_address("x0", &closure, CLOSURE_TAG);
                    return Ok(());
                }
                if SPECIAL_FORMS.contains(&name) {
                    return Err(format!("bad syntax: {name} used as a value"));
                }
                Err(format!("unbound variable: {}", display_name(name)))
            }
        }
    }

    /// Using a fixed-arity primitive like `car` as a value: wrap it in a function once.
    fn primitive_closure(&mut self, name: &str) -> Result<String> {
        if let Some(l) = self.prim_closures.get(name) {
            return Ok(l.clone());
        }
        let arity = primitives::value_arity(name)
            .ok_or_else(|| format!("{name} can't be used as a value"))?;
        let params = Params {
            fixed: (0..arity).map(|i| format!("arg {i}")).collect(),
            rest: None,
        };
        let mut call = vec![sym(name)];
        call.extend(params.fixed.iter().map(|p| sym(p)));
        let code = self.fresh("scm_prim");
        let line = std::mem::take(&mut self.line); // errors inside report no line
        let r = self.compile_function(&code, name, &params, &[], &[list(call)], true);
        self.line = line;
        r?;
        let closure = self.static_closure(&code);
        self.prim_closures.insert(name.to_string(), closure.clone());
        Ok(closure)
    }

    /// Evaluate `args` into slots `base..`, left to right.
    fn args_to_slots(&mut self, args: &[Expr], env: &Env, base: usize) -> Result<()> {
        for (i, a) in args.iter().enumerate() {
            self.compile(a, env, base + i, false)?;
            self.store_slot("x0", base + i)?;
        }
        Ok(())
    }

    /// Pass the `n` arguments in slots `base..`: the first 8 in x0..x7, the
    /// rest in `scm_argbuf[8..]`; the count goes in x9.
    fn load_args(&mut self, base: usize, n: usize) -> Result<()> {
        if n > MAX_CALL_ARGS {
            return Err(format!(
                "at most {MAX_CALL_ARGS} arguments in one call (use apply for more)"
            ));
        }
        if n > REG_ARGS {
            self.load_address("x11", "_scm_argbuf", 0);
            for i in REG_ARGS..n {
                self.load_slot("x10", base + i);
                emit!(self, "str x10, [x11, #{}]", 8 * i);
            }
        }
        for i in 0..n.min(REG_ARGS) {
            self.load_slot(&format!("x{i}"), base + i);
        }
        emit!(self, "mov x9, #{n}");
        Ok(())
    }

    /// Call a top-level `(define (f ...))` by label (no closure needed).
    #[allow(clippy::too_many_arguments)]
    fn compile_direct_call(
        &mut self,
        name: &str,
        label: &str,
        arity: usize,
        rest: bool,
        args: &[Expr],
        env: &Env,
        si: usize,
        tail: bool,
    ) -> Result<()> {
        if args.len() < arity || (!rest && args.len() > arity) {
            return Err(format!(
                "{name} expects {} argument(s), got {}",
                arity_text(arity, rest),
                args.len()
            ));
        }
        self.args_to_slots(args, env, si)?;
        self.load_args(si, args.len())?;
        if tail {
            self.emit_teardown();
            emit!(self, "b {label}");
        } else {
            emit!(self, "bl {label}");
        }
        Ok(())
    }

    /// x8 = the closure in slot `k`; x10 = its code. Errors if it isn't a procedure.
    fn load_callee(&mut self, k: usize) {
        self.load_slot("x8", k);
        let err = self.error_label("application: not a procedure:", "mov x1, x8");
        emit!(self, "and x10, x8, #7");
        emit!(self, "cmp x10, #{CLOSURE_TAG}");
        emit!(self, "b.ne {err}");
        emit!(self, "ldur x10, [x8, #-{CLOSURE_TAG}]");
    }

    fn emit_jump_to_callee(&mut self, tail: bool) {
        if tail {
            self.emit_teardown();
            emit!(self, "br x10");
        } else {
            emit!(self, "blr x10");
        }
    }

    /// Call whatever closure `op` evaluates to.
    fn compile_call(
        &mut self,
        op: &Expr,
        args: &[Expr],
        env: &Env,
        si: usize,
        tail: bool,
    ) -> Result<()> {
        self.compile(op, env, si, false)?;
        self.store_slot("x0", si)?;
        self.args_to_slots(args, env, si + 1)?;
        self.load_args(si + 1, args.len())?;
        self.load_callee(si);
        self.emit_jump_to_callee(tail);
        Ok(())
    }

    /// `(apply f a... list)`: spread the arguments into the argument buffer.
    fn compile_apply(&mut self, args: &[Expr], env: &Env, si: usize, tail: bool) -> Result<()> {
        if args.len() < 2 {
            return Err("apply expects a procedure and a list".into());
        }
        self.args_to_slots(args, env, si)?;
        let fixed = args.len() - 2;
        self.load_address("x11", "_scm_argbuf", 0);
        for j in 0..fixed {
            self.load_slot("x10", si + 1 + j);
            emit!(self, "str x10, [x11, #{}]", 8 * j);
        }
        self.load_slot("x12", si + args.len() - 1);
        emit!(self, "mov x9, #{fixed}");
        let (top, done) = (self.fresh("L"), self.fresh("L"));
        let not_list = self.error_label("apply: expected a list, got", "mov x1, x12");
        let too_many = self.error_label(&format!("apply: more than {ARGBUF_SIZE} arguments"), "");
        self.label(&top);
        emit!(self, "cmp x12, #{EMPTY}");
        emit!(self, "b.eq {done}");
        emit!(self, "and x10, x12, #7");
        emit!(self, "cmp x10, #{PAIR_TAG}");
        emit!(self, "b.ne {not_list}");
        emit!(self, "cmp x9, #{}, lsl #12", ARGBUF_SIZE >> 12);
        emit!(self, "b.hs {too_many}");
        emit!(self, "ldur x10, [x12, #-{PAIR_TAG}]");
        emit!(self, "str x10, [x11, x9, lsl #3]");
        emit!(self, "ldur x12, [x12, #{}]", 8 - PAIR_TAG);
        emit!(self, "add x9, x9, #1");
        emit!(self, "b {top}");
        self.label(&done);
        for i in (0..REG_ARGS).step_by(2) {
            emit!(self, "ldp x{}, x{}, [x11, #{}]", i, i + 1, 8 * i);
        }
        self.load_callee(si);
        self.emit_jump_to_callee(tail);
        Ok(())
    }

    fn compile_body(&mut self, body: &[Expr], env: &Env, si: usize, tail: bool) -> Result<()> {
        let Some((last, init)) = body.split_last() else {
            return Err("empty body".into());
        };
        for e in init {
            self.compile(e, env, si, false)?;
        }
        self.compile(last, env, si, tail)
    }

    /// A quoted datum. Lists and vectors are built fresh each time they're evaluated.
    fn compile_quoted(&mut self, datum: &Expr, si: usize) -> Result<()> {
        if let Some(imm) = immediate(datum)? {
            self.load_imm("x0", imm);
            return Ok(());
        }
        match datum {
            Expr::Symbol(s) => {
                let label = self.symbol_label(s);
                self.load_address("x0", &label, SYMBOL_TAG);
            }
            Expr::Str(_) => self.compile(datum, &Env::new(), si, false)?,
            Expr::List(items, _) => {
                let n = items.len();
                let dotted = n >= 3 && items[n - 2] == sym(".");
                let elems = if dotted { &items[..n - 2] } else { &items[..] };
                for (i, item) in elems.iter().enumerate() {
                    self.compile_quoted(item, si + i)?;
                    self.store_slot("x0", si + i)?;
                }
                let tail = if dotted {
                    self.compile_quoted(&items[n - 1], si + elems.len())?;
                    self.store_slot("x0", si + elems.len())?;
                    Some(si + elems.len())
                } else {
                    None
                };
                self.list_from_slots(si, elems.len(), tail)?;
            }
            Expr::Vector(items) => {
                for (i, item) in items.iter().enumerate() {
                    self.compile_quoted(item, si + i)?;
                    self.store_slot("x0", si + i)?;
                }
                self.vector_from_slots(si, items.len())?;
            }
            _ => unreachable!("immediates handled above"),
        }
        Ok(())
    }

    fn compile_special(
        &mut self,
        head: &str,
        args: &[Expr],
        env: &Env,
        si: usize,
        tail: bool,
    ) -> Result<()> {
        match head {
            "quote" => {
                let [datum] = args else {
                    return Err("quote expects 1 argument".into());
                };
                self.compile_quoted(datum, si)?;
                self.ret_if(tail);
            }
            "%void" => {
                self.load_imm("x0", VOID);
                self.ret_if(tail);
            }
            "if" => {
                if !(2..=3).contains(&args.len()) {
                    return Err("if expects 2 or 3 arguments".into());
                }
                let (else_l, end_l) = (self.fresh("L"), self.fresh("L"));
                self.compile(&args[0], env, si, false)?;
                emit!(self, "cmp x0, #{FALSE}");
                emit!(self, "b.eq {else_l}");
                self.compile(&args[1], env, si, tail)?;
                if !tail {
                    emit!(self, "b {end_l}"); // in tail position the branch already returned
                }
                self.label(&else_l);
                match args.get(2) {
                    Some(alt) => self.compile(alt, env, si, tail)?,
                    None => self.compile_special("%void", &[], env, si, tail)?,
                }
                self.label(&end_l);
            }
            "and" | "or" => {
                if args.is_empty() {
                    return self.compile(&Expr::Bool(head == "and"), env, si, tail);
                }
                // stop early when a value is #f (and) or not #f (or); x0 already holds the answer
                let end_l = self.fresh("L");
                let branch = if head == "and" { "b.eq" } else { "b.ne" };
                let (last, init) = args.split_last().unwrap();
                for a in init {
                    self.compile(a, env, si, false)?;
                    emit!(self, "cmp x0, #{FALSE}");
                    emit!(self, "{branch} {end_l}");
                }
                self.compile(last, env, si, tail)?;
                self.label(&end_l);
                // early exits land here; the last expression returned by itself if tail
                if !init.is_empty() {
                    self.ret_if(tail);
                }
            }
            "begin" => {
                if args.is_empty() {
                    return self.compile_special("%void", &[], env, si, tail);
                }
                self.compile_body(args, env, si, tail)?;
            }
            "let" => {
                let [Expr::List(bindings, _), body @ ..] = args else {
                    return Err("let expects a binding list and a body".into());
                };
                // all right-hand sides see the *outer* env; each result gets its own slot
                let mut new_env = env.clone();
                let mut names: Vec<&str> = Vec::new();
                for (i, b) in bindings.iter().enumerate() {
                    let (name, rhs) = parse_binding(b)?;
                    if names.contains(&name) {
                        return Err(format!("let: duplicate binding {}", display_name(name)));
                    }
                    names.push(name);
                    self.compile(rhs, env, si + i, false)?;
                    self.store_slot("x0", si + i)?;
                    let boxed = is_assigned(name, body);
                    if boxed {
                        self.box_slot(si + i)?;
                    }
                    new_env.push(Local {
                        name: name.to_string(),
                        loc: Loc::Slot(si + i),
                        boxed,
                    });
                }
                self.compile_body(body, &new_env, si + bindings.len(), tail)?;
            }
            "lambda" => {
                let [params, body @ ..] = args else {
                    return Err("lambda expects a parameter list and a body".into());
                };
                let params = parse_params(params)?;
                self.compile_lambda(&params, body, env)?;
                self.ret_if(tail);
            }
            "set!" => {
                let [Expr::Symbol(name), rhs] = args else {
                    return Err("set! expects a variable name and a value".into());
                };
                self.compile(rhs, env, si, false)?;
                if let Some(local) = lookup(env, name) {
                    assert!(local.boxed, "assigned variable {name} wasn't boxed");
                    self.load_raw(local, "x1");
                    emit!(self, "stur x0, [x1, #-{PAIR_TAG}]");
                } else {
                    match self.globals.get(name) {
                        Some(Global::Var { label }) => {
                            let label = label.clone();
                            emit!(self, "adrp x1, {label}@PAGE");
                            emit!(self, "str x0, [x1, {label}@PAGEOFF]");
                        }
                        Some(Global::Func { .. }) => {
                            return Err(format!("can't set! {name}: it was defined as a function"));
                        }
                        None => {
                            return Err(format!("set!: unbound variable: {}", display_name(name)));
                        }
                    }
                }
                self.load_imm("x0", VOID);
                self.ret_if(tail);
            }
            _ => return Err(format!("bad syntax: {head}")),
        }
        Ok(())
    }

    /// Emit the lambda's code as a separate function, and code here that
    /// creates its closure in x0.
    fn compile_lambda(&mut self, params: &Params, body: &[Expr], env: &Env) -> Result<()> {
        let mut referenced = Vec::new();
        body.iter()
            .for_each(|e| referenced_symbols(e, &mut referenced));
        let free: Vec<Local> = referenced
            .iter()
            .filter(|s| !params.all().any(|p| p == *s))
            .filter_map(|s| lookup(env, s).cloned())
            .collect();

        let code = self.fresh("scm_lambda");
        self.compile_function(&code, "lambda", params, &free, body, true)?;

        if free.is_empty() {
            let closure = self.static_closure(&code);
            self.load_address("x0", &closure, CLOSURE_TAG);
            return Ok(());
        }
        // no values are held in registers here, so allocating is safe
        self.alloc((16 + 8 * free.len()).div_ceil(16) * 16);
        emit!(self, "adr x1, {code}");
        emit!(self, "str x1, [x0]");
        self.load_imm("x1", free.len() as i64);
        emit!(self, "str x1, [x0, #8]");
        for (j, local) in free.iter().enumerate() {
            self.load_raw(local, "x1"); // captures the box itself if boxed
            if 16 + 8 * j <= 32760 {
                emit!(self, "str x1, [x0, #{}]", 16 + 8 * j);
            } else {
                self.load_imm("x2", (16 + 8 * j) as i64);
                emit!(self, "str x1, [x0, x2]");
            }
        }
        emit!(self, "add x0, x0, #{CLOSURE_TAG}");
        Ok(())
    }

    /// Emit one complete function into `self.text`. `free` are the variables
    /// it reads from its closure (in field order).
    fn compile_function(
        &mut self,
        label: &str,
        name: &str,
        params: &Params,
        free: &[Local],
        body: &[Expr],
        check_arity: bool,
    ) -> Result<()> {
        let outer = std::mem::take(&mut self.f);
        if let Err(e) = self.compile_function_body(params, free, body) {
            self.f = outer;
            return Err(e);
        }
        let body_asm = std::mem::take(&mut self.f.out);
        let frame = (self.f.max_slots * 8).div_ceil(16) * 16; // sp must stay 16-byte aligned

        writeln!(
            self.f.out,
            "\n    .p2align 2\n{label}:  // {}",
            display_name(name)
        )
        .unwrap();
        if check_arity {
            let n = params.fixed.len();
            let rest = params.rest.is_some();
            let err = self.error_label(
                &format!(
                    "{}: expected {} argument(s), got",
                    display_name(name),
                    arity_text(n, rest)
                ),
                "lsl x1, x9, #2",
            );
            emit!(self, "cmp x9, #{n}");
            emit!(self, "{} {err}", if rest { "b.lo" } else { "b.ne" });
        }
        emit!(self, "stp x29, x30, [sp, #-16]!");
        emit!(self, "mov x29, sp");
        if frame > 0 {
            self.load_imm("x10", frame as i64);
            emit!(self, "sub sp, sp, x10");
            // zero the slots so the GC never sees garbage
            if frame <= 512 {
                for off in (0..frame).step_by(16) {
                    emit!(self, "stp xzr, xzr, [sp, #{off}]");
                }
            } else {
                let l = self.fresh("L");
                emit!(self, "mov x10, sp");
                self.label(&l);
                emit!(self, "stp xzr, xzr, [x10], #16");
                emit!(self, "cmp x10, x29");
                emit!(self, "b.lo {l}");
            }
        }
        self.f.out.push_str(&body_asm);
        let finished = std::mem::replace(&mut self.f, outer);
        self.text.push_str(&finished.out);
        Ok(())
    }

    fn compile_function_body(
        &mut self,
        params: &Params,
        free: &[Local],
        body: &[Expr],
    ) -> Result<()> {
        let n = params.fixed.len();
        let mut env = Env::new();
        for (i, p) in params.all().enumerate() {
            env.push(Local {
                name: p.clone(),
                loc: Loc::Slot(i),
                boxed: is_assigned(p, body),
            });
        }

        // copy the arguments into slots before anything can allocate
        if params.rest.is_some() {
            // spill the register arguments so all of them are in the buffer
            self.load_address("x11", "_scm_argbuf", 0);
            for i in (0..REG_ARGS).step_by(2) {
                emit!(self, "stp x{}, x{}, [x11, #{}]", i, i + 1, 8 * i);
            }
            for i in 0..n {
                emit!(self, "ldr x10, [x11, #{}]", 8 * i);
                self.store_slot("x10", i)?;
            }
        } else {
            for i in 0..n.min(REG_ARGS) {
                self.store_slot(&format!("x{i}"), i)?;
            }
            if n > REG_ARGS {
                self.load_address("x11", "_scm_argbuf", 0);
                for i in REG_ARGS..n {
                    emit!(self, "ldr x10, [x11, #{}]", 8 * i);
                    self.store_slot("x10", i)?;
                }
            }
        }
        let mut si = params.all().count();
        if !free.is_empty() {
            self.store_slot("x8", si)?;
            self.f.closure_slot = Some(si);
            si += 1;
            for (j, local) in free.iter().enumerate() {
                env.push(Local {
                    name: local.name.clone(),
                    loc: Loc::Free(j),
                    boxed: local.boxed,
                });
            }
        }
        if params.rest.is_some() {
            self.collect_rest_args(n)?;
        }
        let boxed: Vec<usize> = (0..params.all().count())
            .filter(|&i| env[i].boxed)
            .collect();
        for i in boxed {
            self.box_slot(i)?;
        }
        self.compile_body(body, &env, si, true)
    }

    /// Turn arguments `n..x9` (in `scm_argbuf`) into a list in slot `n`.
    /// The GC treats `scm_argbuf[0..scm_argbuf_live]` as roots meanwhile.
    fn collect_rest_args(&mut self, n: usize) -> Result<()> {
        let (empty, done, top) = (self.fresh("L"), self.fresh("L"), self.fresh("L"));
        emit!(self, "lsl x10, x9, #{FIXNUM_SHIFT}");
        self.store_slot("x10", n)?; // the count, as a fixnum, survives the allocation
        emit!(self, "cmp x9, #{n}");
        emit!(self, "b.eq {empty}");
        self.load_address("x12", "_scm_argbuf_live", 0);
        emit!(self, "str x9, [x12]");
        emit!(self, "sub x10, x9, #{n}");
        emit!(self, "lsl x10, x10, #4"); // 16 bytes per pair
        emit!(self, "bl scm_alloc");
        self.load_slot("x12", n);
        emit!(self, "asr x12, x12, #{FIXNUM_SHIFT}");
        self.load_address("x14", "_scm_argbuf", 0);
        emit!(self, "mov x13, #{n}");
        emit!(self, "mov x15, x0");
        self.label(&top);
        emit!(self, "ldr x1, [x14, x13, lsl #3]");
        emit!(self, "add x2, x15, #{}", 16 + PAIR_TAG);
        emit!(self, "stp x1, x2, [x15], #16");
        emit!(self, "add x13, x13, #1");
        emit!(self, "cmp x13, x12");
        emit!(self, "b.lo {top}");
        self.load_imm("x2", EMPTY);
        emit!(self, "stur x2, [x15, #-8]"); // the last cdr
        emit!(self, "add x0, x0, #{PAIR_TAG}");
        self.load_address("x12", "_scm_argbuf_live", 0);
        emit!(self, "str xzr, [x12]");
        emit!(self, "b {done}");
        self.label(&empty);
        self.load_imm("x0", EMPTY);
        self.label(&done);
        self.store_slot("x0", n)
    }
}

/// Variadic primitives and `apply` used as values are these library functions.
const VALUE_WRAPPERS: &[&str] = &["+", "-", "*", "list", "vector", "apply"];

/// A program after macro expansion: every definition and expression is in
/// core forms, and the prelude functions it uses are included.
pub struct Program {
    /// (name, definition, source line, comes from the prelude)
    pub defs: Vec<(String, Def, u32, bool)>,
    /// the top-level expressions, in order (`(define x e)` became `(set! x e)`)
    pub main: Vec<Expr>,
}

impl Program {
    /// The expanded program as Scheme source (for `-E` and the playground),
    /// leaving out the prelude.
    pub fn to_source(&self, width: usize) -> String {
        let mut out = String::new();
        for (name, def, _, from_prelude) in &self.defs {
            if *from_prelude {
                continue;
            }
            let form = match def {
                Def::Function { params, body } => {
                    let mut sig = vec![sym(name)];
                    match params.to_expr() {
                        Expr::List(items, _) => sig.extend(items),
                        rest => sig.extend([sym("."), rest]),
                    }
                    let mut items = vec![sym("define"), list(sig)];
                    items.extend(body.iter().cloned());
                    list(items)
                }
                Def::Value(_) => list(vec![sym("define"), sym(name), Expr::Bool(false)]),
            };
            out.push_str(&reader::pretty(&form, width));
            out.push_str("\n\n");
        }
        let is_prelude = |n: &str| self.defs.iter().any(|(d, .., p)| *p && d == n);
        for e in &self.main {
            match e {
                Expr::List(items, _) if items.first() == Some(&sym("%void")) => continue,
                // initializing a library variable
                Expr::List(items, _)
                    if items.first() == Some(&sym("set!"))
                        && matches!(&items[1], Expr::Symbol(n) if is_prelude(n)) =>
                {
                    continue;
                }
                _ => {}
            }
            out.push_str(&reader::pretty(e, width));
            out.push('\n');
        }
        out
    }
}

/// Compile a whole program: top-level definitions and expressions,
/// evaluated in order; the value of the last expression is printed.
/// `file` is used in error messages.
///
/// Produces an assembly file defining
/// `value scheme_entry(void *heap, void *heap_end, void *stack_top)`.
pub fn compile_program(forms: &[Expr], file: &str) -> Result<String> {
    generate(&expand_program(forms)?, file)
}

/// Expand macros and derived syntax, and pull in the prelude functions used.
pub fn expand_program(forms: &[Expr]) -> Result<Program> {
    let mut ex = Expander::default();
    let prelude_forms = reader::read_all(PRELUDE).expect("prelude parses");

    // ── macros first, so they can be used before their definition
    let mut rest = Vec::new();
    for f in forms {
        if !ex.define_macro(f)? {
            rest.push(f.clone());
        }
    }

    // ── top-level forms: expand head macros, splice begins, find definitions
    let mut flat = Vec::new();
    for f in &rest {
        ex.flatten_into(f, &mut flat)?;
    }
    let mut user_defs: Vec<(String, Def, u32)> = Vec::new();
    let mut main_raw: Vec<Expr> = Vec::new();
    for form in &flat {
        if head_is_define_syntax(form) {
            return Err(at_line(
                line_of(form),
                "define-syntax produced by a macro isn't supported".into(),
            ));
        }
        match parse_define(form)? {
            Some((name, def)) => {
                if ex.is_macro(&name) {
                    return Err(at_line(line_of(form), format!("{name} is already a macro")));
                }
                if user_defs.iter().any(|(n, ..)| *n == name) {
                    return Err(at_line(line_of(form), format!("{name} is defined twice")));
                }
                if let Def::Value(e) = &def {
                    main_raw.push(Expr::List(
                        vec![sym("set!"), sym(&name), e.clone()],
                        Line(line_of(form)),
                    ));
                }
                user_defs.push((name, def, line_of(form)));
            }
            None => main_raw.push(form.clone()),
        }
    }
    let prelude_defs: Vec<(String, Def, u32)> = prelude_forms
        .iter()
        .map(|f| {
            let (n, d) = parse_define(f)
                .expect("prelude is valid")
                .expect("prelude has only defines");
            (n, d, line_of(f))
        })
        .collect();

    // ── expand everything into core forms
    let expand_def = |ex: &mut Expander, def: Def| -> Result<Def> {
        Ok(match def {
            Def::Function { params, body } => Def::Function {
                params,
                body: ex.expand_body(&body)?,
            },
            Def::Value(e) => Def::Value(ex.expand(&e)?),
        })
    };
    let mut defs = Vec::new();
    for (name, def, line) in user_defs {
        defs.push((name, expand_def(&mut ex, def)?, line, false));
    }
    let mut main_body = main_raw
        .iter()
        .map(|e| ex.expand(e))
        .collect::<Result<Vec<_>>>()?;
    if main_body.is_empty() {
        main_body.push(list(vec![sym("%void")]));
    }

    // ── pull in the prelude definitions the program uses (transitively)
    let mut used = HashSet::new();
    main_body.iter().for_each(|e| all_symbols(e, &mut used));
    for (_, def, ..) in &defs {
        match def {
            Def::Function { body, .. } => body.iter().for_each(|e| all_symbols(e, &mut used)),
            Def::Value(e) => all_symbols(e, &mut used),
        }
    }
    let mut prelude: Vec<(String, Def, u32)> = Vec::new();
    for (name, def, line) in prelude_defs {
        prelude.push((
            name,
            expand_def(&mut ex, def).expect("prelude expands"),
            line,
        ));
    }
    let mut included = HashSet::new();
    loop {
        let before = included.len();
        for w in VALUE_WRAPPERS {
            if used.contains(*w) {
                used.insert(format!("%{w}"));
            }
        }
        for (name, def, _) in &prelude {
            if used.contains(name)
                && !included.contains(name)
                && !defs.iter().any(|(n, ..)| n == name)
            {
                included.insert(name.clone());
                match def {
                    Def::Function { body, .. } => {
                        body.iter().for_each(|e| all_symbols(e, &mut used))
                    }
                    Def::Value(e) => all_symbols(e, &mut used),
                }
            }
        }
        if included.len() == before {
            break;
        }
    }
    let prelude: Vec<_> = prelude
        .into_iter()
        .filter(|(n, ..)| included.contains(n))
        .collect();
    // prelude variables are initialized before the program runs
    let inits: Vec<Expr> = prelude
        .iter()
        .filter_map(|(n, d, _)| match d {
            Def::Value(e) => Some(list(vec![sym("set!"), sym(n), e.clone()])),
            Def::Function { .. } => None,
        })
        .collect();
    main_body.splice(0..0, inits);
    defs.extend(prelude.into_iter().map(|(n, d, l)| (n, d, l, true)));
    Ok(Program {
        defs,
        main: main_body,
    })
}

/// Generate the assembly for an expanded program.
pub fn generate(program: &Program, file: &str) -> Result<String> {
    let Program {
        defs,
        main: main_body,
    } = program;
    // ── register globals so every function can see every other
    let mut c = Compiler {
        file: file.to_string(),
        ..Default::default()
    };
    let mut labels = Vec::new();
    for (name, def, ..) in defs {
        let global = match def {
            Def::Function { params, .. } => {
                let label = c.fresh("scm_fn");
                let closure = c.static_closure(&label);
                labels.push(label.clone());
                Global::Func {
                    label,
                    closure,
                    arity: params.fixed.len(),
                    rest: params.rest.is_some(),
                }
            }
            Def::Value(_) => {
                let label = c.fresh("scm_g");
                writeln!(c.globals_data, "{label}:\n    .quad {UNBOUND}").unwrap();
                labels.push(label.clone());
                Global::Var { label }
            }
        };
        c.globals.insert(name.clone(), global);
    }

    // `%call/cc` is hand-written assembly (see RUNTIME_GLUE)
    if !c.globals.contains_key("%call/cc") {
        let closure = c.static_closure("scm_callcc");
        c.globals.insert(
            "%call/cc".into(),
            Global::Func {
                label: "scm_callcc".into(),
                closure,
                arity: 1,
                rest: false,
            },
        );
    }

    // ── code
    let no_params = Params {
        fixed: vec![],
        rest: None,
    };
    // the listing reads in order: the program's functions, its top-level
    // code, then the library functions it uses
    c.text.push_str("\n// ───── the program\n");
    let functions = |prelude: bool| {
        defs.iter()
            .zip(&labels)
            .filter(move |((.., from_prelude), _)| *from_prelude == prelude)
    };
    for ((name, def, line, _), label) in functions(false) {
        if let Def::Function { params, body } = def {
            c.line = *line;
            c.compile_function(label, name, params, &[], body, true)
                .map_err(|e| at_line(*line, e))?;
        }
    }
    c.line = 0;
    c.compile_function("scm_main", "program", &no_params, &[], main_body, false)?;
    c.text.push_str("\n// ───── library functions it uses (src/prelude.scm)\n");
    c.file = "prelude".into();
    for ((name, def, line, _), label) in functions(true) {
        if let Def::Function { params, body } = def {
            c.line = *line;
            c.compile_function(label, name, params, &[], body, true)
                .map_err(|e| at_line(*line, e))?;
        }
    }

    Ok(assemble(c))
}

fn head_is_define_syntax(e: &Expr) -> bool {
    matches!(e, Expr::List(items, _) if items.first() == Some(&sym("define-syntax")))
}

fn assemble(c: Compiler) -> String {
    let mut out = String::new();
    out.push_str("    .text\n");
    out.push_str(&c.text);
    out.push_str("\n// ───── runtime: entry point, allocator, continuations\n");
    out.push_str(RUNTIME_GLUE);
    out.push_str("\n// ───── runtime errors\n");
    for (i, e) in c.errors.iter().enumerate() {
        writeln!(out, "{}:", e.label).unwrap();
        if e.culprit.is_empty() {
            writeln!(out, "    mov x1, #{VOID}").unwrap(); // nothing to show
        }
        for line in e.culprit.lines() {
            writeln!(out, "    {line}").unwrap();
        }
        writeln!(
            out,
            "    adrp x0, scm_msg{i}@PAGE\n    add x0, x0, scm_msg{i}@PAGEOFF\n    bl _scheme_error"
        )
        .unwrap();
    }
    out.push_str("\n    .section __TEXT,__cstring\n");
    for (i, e) in c.errors.iter().enumerate() {
        writeln!(
            out,
            "scm_msg{i}:\n    .asciz \"{}\"",
            asm_string(&e.message)
        )
        .unwrap();
    }
    out.push_str("\n    .data\n    .p2align 3\n");
    out.push_str("    .globl _scm_stack_base\n_scm_stack_base:\n    .quad 0\n");
    out.push_str("    .globl _scm_argbuf_live\n_scm_argbuf_live:\n    .quad 0\n");
    out.push_str(
        "    .globl _scm_globals_start\n    .globl _scm_globals_end\n_scm_globals_start:\n",
    );
    out.push_str(&c.globals_data);
    out.push_str("_scm_globals_end:\n");
    // every static symbol, so string->symbol can find them
    out.push_str(
        "    .globl _scm_symbols_start\n    .globl _scm_symbols_end\n_scm_symbols_start:\n",
    );
    let mut syms: Vec<&String> = c.symbols.values().collect();
    syms.sort();
    for l in syms {
        writeln!(out, "    .quad {l}+{SYMBOL_TAG}").unwrap();
    }
    out.push_str("_scm_symbols_end:\n");
    out.push_str(&c.data);
    writeln!(
        out,
        "\n    .globl _scm_argbuf\n    .zerofill __DATA,__bss,_scm_argbuf,{},4",
        ARGBUF_SIZE * 8
    )
    .unwrap();
    out
}

/// The entry point (C calling convention) and the allocator.
const RUNTIME_GLUE: &str = r#"    .text
    .globl _scheme_entry
    .p2align 2
// value scheme_entry(void *heap, void *heap_end, void *stack_top)
// Runs the program on its own (big) stack.
_scheme_entry:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x27, x28, [sp, #-16]!     // callee-saved in C
    mov x28, x0
    mov x27, x1
    mov x9, sp
    mov sp, x2                    // switch to the Scheme stack
    stp x9, xzr, [sp, #-16]!      // remember the C stack
    mov x29, sp
    adrp x10, _scm_stack_base@PAGE
    str x29, [x10, _scm_stack_base@PAGEOFF]   // the GC stops walking frames here
    bl scm_main
    ldr x9, [sp]
    mov sp, x9                    // back to the C stack
    ldp x27, x28, [sp], #16
    ldp x29, x30, [sp], #16
    ret

// x0 = raw pointer to x10 bytes; may run the GC
    .p2align 2
scm_alloc:
    add x11, x28, x10
    cmp x11, x27
    b.hi scm_alloc_slow
    mov x0, x28
    mov x28, x11
    ret
scm_alloc_slow:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    str x10, [sp, #-16]!
    add x0, x29, #16              // the caller's sp (its slots start here)
    ldr x1, [x29]                 // the caller's frame pointer
    mov x2, x10
    bl _scheme_gc                 // returns the new heap pointer and limit
    mov x28, x0
    mov x27, x1
    ldr x10, [sp], #16
    ldp x29, x30, [sp], #16
    b scm_alloc

// ───── first-class continuations
//
// (%call/cc f): capture the continuation by copying the whole live Scheme
// stack, from our caller's sp up to scm_stack_base, into a vector. That is
// GC-safe as an ordinary vector: every word is either a tagged value (a
// slot) or a saved x29/x30, and those are 16- and 4-byte aligned, so they
// look like fixnums. The continuation is a closure
//   [scm_cont_entry, 4, stack-copy, caller sp, caller x29, return address]
// and f is tail-called with it, so f's normal return also goes to our caller.
    .p2align 2
scm_callcc:
    cmp x9, #1
    b.ne Lcallcc_arity
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    sub sp, sp, #16
    stp x0, xzr, [sp]             // slot 0 = f, slot 1 = the stack copy
    adrp x11, _scm_stack_base@PAGE
    ldr x11, [x11, _scm_stack_base@PAGEOFF]
    add x12, x29, #16
    sub x10, x11, x12             // bytes of stack (a multiple of 16)
    add x10, x10, #16             // + the length word, rounded up
    bl scm_alloc
    // copy only now: a GC inside scm_alloc may have updated the stack
    adrp x11, _scm_stack_base@PAGE
    ldr x11, [x11, _scm_stack_base@PAGEOFF]
    add x12, x29, #16
    sub x13, x11, x12
    lsr x14, x13, #1              // word count as a fixnum: (bytes / 8) << 2
    str x14, [x0]
    add x15, x0, #8
Lcallcc_copy:
    cmp x12, x11
    b.hs Lcallcc_copied
    ldr x14, [x12], #8
    str x14, [x15], #8
    b Lcallcc_copy
Lcallcc_copied:
    add x0, x0, #5                // VECTOR_TAG
    str x0, [sp, #8]
    mov x10, #48
    bl scm_alloc
    adr x1, scm_cont_entry
    mov x2, #4
    stp x1, x2, [x0]
    ldr x1, [sp, #8]
    add x2, x29, #16              // caller sp
    stp x1, x2, [x0, #16]
    ldp x1, x2, [x29]             // caller x29, return address
    stp x1, x2, [x0, #32]
    add x0, x0, #2                // CLOSURE_TAG: the continuation is f's argument
    ldr x8, [sp]
    and x10, x8, #7
    cmp x10, #2
    b.ne Lcallcc_notproc
    ldur x10, [x8, #-2]
    mov x9, #1
    mov sp, x29
    ldp x29, x30, [sp], #16
    br x10

// Invoking a continuation: x8 = the continuation, x0 = the value. Put the
// saved stack back (using no stack ourselves: it may be the part being
// overwritten) and return to the saved return address.
    .p2align 2
scm_cont_entry:
    cmp x9, #1
    b.eq Lcont_go
    cbnz x9, Lcont_arity
    mov x0, #0x4F                 // (k) with no value passes void
Lcont_go:
    sub x8, x8, #2
    ldp x1, x2, [x8, #16]         // stack copy, saved sp
    ldp x3, x4, [x8, #32]         // saved x29, return address
    mov sp, x2
    ldur x5, [x1, #-5]            // word count (fixnum)
    add x1, x1, #3                // first word: vector - tag + 8
    add x5, x2, x5, lsl #1        // end address: sp + count * 8
Lcont_copy:
    cmp x2, x5
    b.hs Lcont_copied
    ldr x6, [x1], #8
    str x6, [x2], #8
    b Lcont_copy
Lcont_copied:
    mov x29, x3
    br x4

Lcallcc_arity:
    lsl x1, x9, #2
    adrp x0, Lcallcc_arity_msg@PAGE
    add x0, x0, Lcallcc_arity_msg@PAGEOFF
    bl _scheme_error
Lcallcc_notproc:
    mov x1, x8
    adrp x0, Lcallcc_notproc_msg@PAGE
    add x0, x0, Lcallcc_notproc_msg@PAGEOFF
    bl _scheme_error
Lcont_arity:
    lsl x1, x9, #2
    adrp x0, Lcont_arity_msg@PAGE
    add x0, x0, Lcont_arity_msg@PAGEOFF
    bl _scheme_error

    .section __TEXT,__cstring
Lcallcc_arity_msg:
    .asciz "call/cc: expected 1 argument(s), got"
Lcallcc_notproc_msg:
    .asciz "call/cc: expected a procedure, got"
Lcont_arity_msg:
    .asciz "continuation: expected 0 or 1 argument(s), got"
    .text
"#;
