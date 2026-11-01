//! Code generation: `Expr` -> ARM64 assembly (Apple / Mach-O syntax).
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
//! - `x0..x7`  arguments on a call (so at most 8 parameters)
//! - `x8`      the closure being called;  `x9` the argument count
//! - `x10`     allocation size for `scm_alloc`;  `x1..x4`, `x9..x11` scratch
//! - `x27`     heap limit;  `x28` heap pointer (bump allocator)
//! - `x29`     frame pointer;  `x30` return address
//!
//! # Stack frames
//! ```text
//!   x29 -> | saved x29, x30 |
//!          | slot N-1       |
//!          | ...            |
//!   sp  -> | slot 0         |   slot k lives at [sp, #8*k]
//! ```
//! Parameters (and the closure, if it has free variables) are copied into the
//! first slots. Slots hold only valid values (the prologue zeroes them), and
//! every live value is in a slot whenever allocation can happen, so the
//! garbage collector can treat every slot of every frame as a root and walk
//! frames through the saved-x29 chain.
//!
//! # Variables
//! A variable is a stack slot or a field of the current closure. Variables
//! that are ever `set!` live in a heap "box" (a pair whose car is the value),
//! so closures that capture them share the box.
//!
//! # Tail calls
//! A call in tail position tears down the current frame and branches to the
//! callee, so the callee returns straight to our caller.

use crate::reader::{self, Expr};
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
const MAX_SLOTS: usize = 4095; // limit of `ldr x, [sp, #imm]` (imm/8 <= 4095)
const MAX_ARGS: usize = 8; // passed in x0..x7

const PRELUDE: &str = include_str!("prelude.scm");

/// Keywords. They can't be used as variable names, which keeps desugared
/// code from being captured by user bindings.
const SPECIAL_FORMS: &[&str] = &[
    "quote", "if", "and", "or", "begin", "let", "lambda", "set!", "define", "%void", "cond",
    "else", "let*", "letrec", "letrec*", "when", "unless",
];

type Result<T> = std::result::Result<T, String>;

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
    },
    /// `(define x ...)`: a word in the data section
    Var { label: String },
}

pub(crate) enum Def {
    Function {
        params: Vec<String>,
        body: Vec<Expr>,
    },
    Value(Expr),
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
}

macro_rules! emit {
    ($c:expr, $($arg:tt)*) => {{
        $c.f.out.push_str("    ");
        writeln!($c.f.out, $($arg)*).unwrap();
    }};
}

// after `emit!` so the submodules can use it
mod desugar;
mod primitives;

fn sym(s: &str) -> Expr {
    Expr::Symbol(s.into())
}

fn list(items: Vec<Expr>) -> Expr {
    Expr::List(items)
}

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
        Expr::List(items) if items.is_empty() => EMPTY,
        _ => return Ok(None),
    }))
}

fn check_arity(name: &str, args: &[Expr], n: usize) -> Result<()> {
    if args.len() != n {
        return Err(format!(
            "{name} expects {n} argument(s), got {}",
            args.len()
        ));
    }
    Ok(())
}

fn check_bindable(name: &str) -> Result<()> {
    if SPECIAL_FORMS.contains(&name) {
        return Err(format!(
            "{name} is a keyword and can't be used as a variable name"
        ));
    }
    Ok(())
}

fn parse_binding(b: &Expr) -> Result<(&str, &Expr)> {
    if let Expr::List(pair) = b
        && let [Expr::Symbol(name), rhs] = pair.as_slice()
    {
        check_bindable(name)?;
        return Ok((name, rhs));
    }
    Err("binding must look like [name expr]".into())
}

fn parse_params(params: &[Expr]) -> Result<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    for p in params {
        let Expr::Symbol(name) = p else {
            return Err(format!("parameter must be a name, got {p:?}"));
        };
        check_bindable(name)?;
        if names.contains(name) {
            return Err(format!("duplicate parameter {name}"));
        }
        names.push(name.clone());
    }
    if names.len() > MAX_ARGS {
        return Err(format!("at most {MAX_ARGS} parameters are supported"));
    }
    Ok(names)
}

/// Recognize `(define (f params...) body...)` and `(define x expr)`.
fn parse_define(form: &Expr) -> Result<Option<(String, Def)>> {
    let Expr::List(items) = form else {
        return Ok(None);
    };
    if items.first() != Some(&sym("define")) {
        return Ok(None);
    }
    match &items[1..] {
        [Expr::List(sig), body @ ..] => {
            let Some((Expr::Symbol(name), params)) = sig.split_first() else {
                return Err("define needs a function name".into());
            };
            check_bindable(name)?;
            let params = parse_params(params).map_err(|e| format!("{name}: {e}"))?;
            if body.is_empty() {
                return Err(format!("{name}: empty body"));
            }
            Ok(Some((
                name.clone(),
                Def::Function {
                    params,
                    body: body.to_vec(),
                },
            )))
        }
        [Expr::Symbol(name), value] => {
            check_bindable(name)?;
            Ok(Some((name.clone(), Def::Value(value.clone()))))
        }
        _ => Err(
            "define must look like (define (name args...) body...) or (define name expr)".into(),
        ),
    }
}

/// Every symbol in `e`, including quoted ones (for deciding what prelude code to include).
fn all_symbols(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Symbol(s) => {
            out.insert(s.clone());
        }
        Expr::List(items) => items.iter().for_each(|i| all_symbols(i, out)),
        _ => {}
    }
}

/// Symbols that may refer to variables, in first-use order (skips quoted data).
fn referenced_symbols(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Symbol(s) if !out.contains(s) => out.push(s.clone()),
        Expr::List(items) if items.first() == Some(&sym("quote")) => {}
        Expr::List(items) => items.iter().for_each(|i| referenced_symbols(i, out)),
        _ => {}
    }
}

/// Could `name` be the target of a `set!` in `body`? (Conservative: ignores
/// shadowing, which can only cause an unnecessary box.)
fn is_assigned(name: &str, body: &[Expr]) -> bool {
    body.iter().any(|e| match e {
        Expr::List(items) => match items.as_slice() {
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

impl Compiler {
    fn fresh(&mut self, prefix: &str) -> String {
        self.labels += 1;
        format!("{prefix}{}", self.labels)
    }

    fn label(&mut self, l: &str) {
        writeln!(self.f.out, "{l}:").unwrap();
    }

    fn slot(&mut self, si: usize) -> Result<String> {
        if si >= MAX_SLOTS {
            return Err("expression too deeply nested (out of stack slots)".into());
        }
        self.f.max_slots = self.f.max_slots.max(si + 1);
        Ok(format!("[sp, #{}]", si * 8))
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

    /// Label of a stub that reports `message` plus the value `culprit` puts in x1, then exits.
    fn error_label(&mut self, message: &str, culprit: &str) -> String {
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
            message: message.into(),
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
    fn box_slot(&mut self, k: usize) {
        self.alloc(16);
        emit!(self, "ldr x1, [sp, #{}]", k * 8);
        emit!(self, "str x1, [x0]");
        self.load_imm("x1", EMPTY);
        emit!(self, "str x1, [x0, #8]");
        emit!(self, "add x0, x0, #{PAIR_TAG}");
        emit!(self, "str x0, [sp, #{}]", k * 8);
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
        if let Some(imm) = immediate(e)? {
            self.load_imm("x0", imm);
            self.ret_if(tail);
            return Ok(());
        }
        match e {
            Expr::Symbol(name) => {
                self.compile_variable(name, env)?;
                self.ret_if(tail);
                Ok(())
            }
            Expr::Str(s) => {
                let label = self.static_bytes("scm_str", s.as_bytes());
                self.load_address("x0", &label, STRING_TAG);
                {
                    self.ret_if(tail);
                    Ok(())
                }
            }
            Expr::List(items) => {
                let (op, args) = items.split_first().unwrap(); // `()` is an immediate
                if let Expr::Symbol(head) = op {
                    if SPECIAL_FORMS.contains(&head.as_str()) {
                        return self.compile_special(head, args, env, si, tail);
                    }
                    if lookup(env, head).is_none() {
                        match self.globals.get(head) {
                            Some(Global::Func { label, arity, .. }) => {
                                let (label, arity) = (label.clone(), *arity);
                                return self
                                    .compile_direct_call(head, &label, arity, args, env, si, tail);
                            }
                            None if primitives::is_primitive(head) => {
                                self.compile_primitive(head, args, env, si)?;
                                self.ret_if(tail);
                                return Ok(());
                            }
                            _ => {}
                        }
                    }
                }
                self.compile_call(op, args, env, si, tail)
            }
            _ => unreachable!("immediates handled above"),
        }
    }

    /// Load a local's slot or closure field into `reg`, without unboxing.
    fn load_raw(&mut self, local: &Local, reg: &str) {
        match local.loc {
            Loc::Slot(k) => emit!(self, "ldr {reg}, [sp, #{}]", k * 8),
            Loc::Free(i) => {
                let cs = self
                    .f
                    .closure_slot
                    .expect("free variable without a closure");
                emit!(self, "ldr {reg}, [sp, #{}]", cs * 8);
                emit!(
                    self,
                    "ldr {reg}, [{reg}, #{}]",
                    16 + 8 * i - CLOSURE_TAG as usize
                );
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
            None if primitives::is_primitive(name) => {
                let closure = self.primitive_closure(name)?;
                self.load_address("x0", &closure, CLOSURE_TAG);
                Ok(())
            }
            None if SPECIAL_FORMS.contains(&name) => {
                Err(format!("bad syntax: {name} used as a value"))
            }
            None => Err(format!("unbound variable: {name}")),
        }
    }

    /// Using a primitive like `car` as a value: wrap it in a function once.
    fn primitive_closure(&mut self, name: &str) -> Result<String> {
        if let Some(l) = self.prim_closures.get(name) {
            return Ok(l.clone());
        }
        let arity = primitives::value_arity(name)
            .ok_or_else(|| format!("{name} can't be used as a value yet"))?;
        let params: Vec<String> = (0..arity).map(|i| format!("arg {i}")).collect();
        let mut call = vec![sym(name)];
        call.extend(params.iter().map(|p| sym(p)));
        let code = self.fresh("scm_prim");
        self.compile_function(&code, name, &params, &[], &[list(call)], true)?;
        let closure = self.static_closure(&code);
        self.prim_closures.insert(name.to_string(), closure.clone());
        Ok(closure)
    }

    /// Evaluate `args` into slots `base..`, left to right.
    fn args_to_slots(&mut self, args: &[Expr], env: &Env, base: usize) -> Result<()> {
        for (i, a) in args.iter().enumerate() {
            self.compile(a, env, base + i, false)?;
            let slot = self.slot(base + i)?;
            emit!(self, "str x0, {slot}");
        }
        Ok(())
    }

    /// Load slots `base..base+n` into x0..x(n-1) and the argument count into x9.
    fn load_args(&mut self, base: usize, n: usize) {
        for i in 0..n {
            emit!(self, "ldr x{i}, [sp, #{}]", (base + i) * 8);
        }
        emit!(self, "mov x9, #{n}");
    }

    /// Call a top-level `(define (f ...))` by label (no closure needed).
    #[allow(clippy::too_many_arguments)]
    fn compile_direct_call(
        &mut self,
        name: &str,
        label: &str,
        arity: usize,
        args: &[Expr],
        env: &Env,
        si: usize,
        tail: bool,
    ) -> Result<()> {
        check_arity(name, args, arity)?;
        self.args_to_slots(args, env, si)?;
        self.load_args(si, args.len());
        if tail {
            self.emit_teardown();
            emit!(self, "b {label}");
        } else {
            emit!(self, "bl {label}");
        }
        Ok(())
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
        if args.len() > MAX_ARGS {
            return Err(format!("at most {MAX_ARGS} arguments are supported"));
        }
        self.compile(op, env, si, false)?;
        let op_slot = self.slot(si)?;
        emit!(self, "str x0, {op_slot}");
        self.args_to_slots(args, env, si + 1)?;
        self.load_args(si + 1, args.len());
        emit!(self, "ldr x8, {op_slot}");
        let err = self.error_label("application: not a procedure:", "mov x1, x8");
        emit!(self, "and x10, x8, #7");
        emit!(self, "cmp x10, #{CLOSURE_TAG}");
        emit!(self, "b.ne {err}");
        emit!(self, "ldur x10, [x8, #-{CLOSURE_TAG}]");
        if tail {
            self.emit_teardown();
            emit!(self, "br x10");
        } else {
            emit!(self, "blr x10");
        }
        Ok(())
    }

    fn compile_body(&mut self, body: &[Expr], env: &Env, si: usize, tail: bool) -> Result<()> {
        if let Some(e) = desugar::internal_defines(body)? {
            return self.compile(&e, env, si, tail);
        }
        let Some((last, init)) = body.split_last() else {
            return Err("empty body".into());
        };
        for e in init {
            self.compile(e, env, si, false)?;
        }
        self.compile(last, env, si, tail)
    }

    /// A quoted datum. Lists are built fresh each time they're evaluated.
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
            Expr::List(items) => {
                for (i, item) in items.iter().enumerate() {
                    self.compile_quoted(item, si + i)?;
                    let slot = self.slot(si + i)?;
                    emit!(self, "str x0, {slot}");
                }
                self.list_from_slots(si, items.len());
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
        if let Some(expanded) = desugar::derived(head, args)? {
            return self.compile(&expanded, env, si, tail);
        }
        match head {
            "quote" => {
                check_arity("quote", args, 1)?;
                self.compile_quoted(&args[0], si)?;
                {
                    self.ret_if(tail);
                    Ok(())
                }
            }
            "%void" => {
                self.load_imm("x0", VOID);
                {
                    self.ret_if(tail);
                    Ok(())
                }
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
                Ok(())
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
                Ok(())
            }
            "begin" => self.compile_body(args, env, si, tail),
            "let" => {
                let [Expr::List(bindings), body @ ..] = args else {
                    return Err("let expects a binding list and a body".into());
                };
                // all right-hand sides see the *outer* env; each result gets its own slot
                let mut new_env = env.clone();
                let mut names: Vec<&str> = Vec::new();
                for (i, b) in bindings.iter().enumerate() {
                    let (name, rhs) = parse_binding(b)?;
                    if names.contains(&name) {
                        return Err(format!("let: duplicate binding {name}"));
                    }
                    names.push(name);
                    self.compile(rhs, env, si + i, false)?;
                    let slot = self.slot(si + i)?;
                    emit!(self, "str x0, {slot}");
                    let boxed = is_assigned(name, body);
                    if boxed {
                        self.box_slot(si + i);
                    }
                    new_env.push(Local {
                        name: name.to_string(),
                        loc: Loc::Slot(si + i),
                        boxed,
                    });
                }
                self.compile_body(body, &new_env, si + bindings.len(), tail)
            }
            "lambda" => {
                let [Expr::List(params), body @ ..] = args else {
                    return Err("lambda expects a parameter list and a body (rest parameters aren't supported yet)".into());
                };
                if body.is_empty() {
                    return Err("lambda: empty body".into());
                }
                let params = parse_params(params)?;
                self.compile_lambda(&params, body, env)?;
                {
                    self.ret_if(tail);
                    Ok(())
                }
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
                        None => return Err(format!("set!: unbound variable: {name}")),
                    }
                }
                self.load_imm("x0", VOID);
                {
                    self.ret_if(tail);
                    Ok(())
                }
            }
            "define" => {
                Err("define is only allowed at the top level or the start of a body".into())
            }
            _ => Err(format!("bad syntax: {head}")),
        }
    }

    /// Emit the lambda's code as a separate function, and code here that
    /// creates its closure in x0.
    fn compile_lambda(&mut self, params: &[String], body: &[Expr], env: &Env) -> Result<()> {
        let mut referenced = Vec::new();
        body.iter()
            .for_each(|e| referenced_symbols(e, &mut referenced));
        let free: Vec<Local> = referenced
            .iter()
            .filter(|s| !params.contains(s))
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
        emit!(self, "mov x1, #{}", free.len());
        emit!(self, "str x1, [x0, #8]");
        for (j, local) in free.iter().enumerate() {
            self.load_raw(local, "x1"); // captures the box itself if boxed
            emit!(self, "str x1, [x0, #{}]", 16 + 8 * j);
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
        params: &[String],
        free: &[Local],
        body: &[Expr],
        check_arity: bool,
    ) -> Result<()> {
        let outer = std::mem::take(&mut self.f);
        let result = self.compile_function_body(params, free, body);
        if let Err(e) = result {
            self.f = outer;
            return Err(e);
        }
        let body_asm = std::mem::take(&mut self.f.out);
        let frame = (self.f.max_slots * 8).div_ceil(16) * 16; // sp must stay 16-byte aligned

        writeln!(self.f.out, "\n    .p2align 2\n{label}:  // {name}").unwrap();
        if check_arity {
            let n = params.len();
            let err = self.error_label(
                &format!("{name}: expected {n} argument(s), got"),
                "lsl x1, x9, #2",
            );
            emit!(self, "cmp x9, #{n}");
            emit!(self, "b.ne {err}");
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
        params: &[String],
        free: &[Local],
        body: &[Expr],
    ) -> Result<()> {
        let mut env = Env::new();
        for (i, p) in params.iter().enumerate() {
            let slot = self.slot(i)?;
            emit!(self, "str x{i}, {slot}");
            env.push(Local {
                name: p.clone(),
                loc: Loc::Slot(i),
                boxed: is_assigned(p, body),
            });
        }
        let mut si = params.len();
        if !free.is_empty() {
            let slot = self.slot(si)?;
            emit!(self, "str x8, {slot}");
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
        let boxed: Vec<usize> = (0..params.len()).filter(|&i| env[i].boxed).collect();
        for i in boxed {
            self.box_slot(i);
        }
        self.compile_body(body, &env, si, true)
    }
}

/// Compile a whole program: top-level `define`s and expressions, evaluated in
/// order; the value of the last expression is printed.
///
/// Produces an assembly file defining `value scheme_entry(void *heap, void *heap_end)`.
pub fn compile_program(forms: &[Expr]) -> Result<String> {
    let mut c = Compiler::default();

    // ── classify top-level forms
    let mut defs: Vec<(String, Def)> = Vec::new();
    let mut main_body = Vec::new();
    for form in forms {
        match parse_define(form)? {
            Some((name, def)) => {
                if defs.iter().any(|(n, _)| *n == name) {
                    return Err(format!("{name} is defined twice"));
                }
                if let Def::Value(e) = &def {
                    main_body.push(list(vec![sym("set!"), sym(&name), e.clone()]));
                }
                defs.push((name, def));
            }
            None => main_body.push(form.clone()),
        }
    }
    if main_body.is_empty() {
        main_body.push(list(vec![sym("%void")]));
    }

    // ── pull in the prelude definitions the program uses (transitively)
    let prelude: Vec<(String, Def)> = reader::read_all(PRELUDE)
        .expect("prelude parses")
        .iter()
        .map(|f| {
            parse_define(f)
                .expect("prelude is valid")
                .expect("prelude has only defines")
        })
        .collect();
    let mut used = HashSet::new();
    forms.iter().for_each(|f| all_symbols(f, &mut used));
    let mut included = HashSet::new();
    loop {
        let before = included.len();
        for (name, def) in &prelude {
            if used.contains(name)
                && !included.contains(name)
                && !defs.iter().any(|(n, _)| n == name)
            {
                included.insert(name.clone());
                if let Def::Function { body, .. } = def {
                    body.iter().for_each(|e| all_symbols(e, &mut used));
                }
            }
        }
        if included.len() == before {
            break;
        }
    }
    defs.extend(prelude.into_iter().filter(|(n, _)| included.contains(n)));

    // ── register globals so every function can see every other
    let mut labels = Vec::new();
    for (name, def) in &defs {
        let global = match def {
            Def::Function { params, .. } => {
                let label = c.fresh("scm_fn");
                let closure = c.static_closure(&label);
                labels.push(label.clone());
                Global::Func {
                    label,
                    closure,
                    arity: params.len(),
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

    // ── code
    c.compile_function("scm_main", "program", &[], &[], &main_body, false)?;
    for ((name, def), label) in defs.iter().zip(&labels) {
        if let Def::Function { params, body } = def {
            c.compile_function(label, name, params, &[], body, true)
                .map_err(|e| format!("in {name}: {e}"))?;
        }
    }

    // ── assemble the file
    let mut out = String::new();
    out.push_str(RUNTIME_GLUE);
    out.push_str(&c.text);
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
    out.push_str(
        "\n    .data\n    .p2align 3\n    .globl _scm_stack_base\n_scm_stack_base:\n    .quad 0\n",
    );
    out.push_str(
        "    .globl _scm_globals_start\n    .globl _scm_globals_end\n_scm_globals_start:\n",
    );
    out.push_str(&c.globals_data);
    out.push_str("_scm_globals_end:\n");
    out.push_str(&c.data);
    Ok(out)
}

/// The entry point (C calling convention) and the allocator.
const RUNTIME_GLUE: &str = r#"    .text
    .globl _scheme_entry
    .p2align 2
// value scheme_entry(void *heap, void *heap_end)
_scheme_entry:
    stp x29, x30, [sp, #-16]!
    mov x29, sp
    stp x27, x28, [sp, #-16]!     // callee-saved in C
    adrp x9, _scm_stack_base@PAGE
    str x29, [x9, _scm_stack_base@PAGEOFF]   // the GC stops walking frames here
    mov x28, x0
    mov x27, x1
    bl scm_main
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
"#;
