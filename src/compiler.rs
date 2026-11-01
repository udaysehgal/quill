//! Code generation: `Expr` -> ARM64 assembly (Apple / Mach-O syntax).
//!
//! # Value representation (64-bit tagged words)
//!
//! | type        | encoding                         | low bits      |
//! |-------------|----------------------------------|---------------|
//! | fixnum      | `n << 2`                         | `..00`        |
//! | pair        | pointer to 16-byte cell, `+ 1`   | `..001`       |
//! | char        | `c << 8 \| 0x0F`                 | `0000_1111`   |
//! | `#f` / `#t` | `0x2F` / `0x6F`                  | `0x2F`, `0x6F`|
//! | `()`        | `0x3F`                           | `0011_1111`   |
//!
//! # Registers
//! - `x0`  accumulator: every expression leaves its result here
//! - `x1`  scratch for the second operand of binary primitives
//! - `x28` heap pointer (bump allocator; the runtime passes the heap in)
//! - `sp`  base of this frame's slots; slot `k` lives at `[sp, #8*k]`

use crate::reader::Expr;
use std::fmt::Write as _;

pub const FIXNUM_SHIFT: u32 = 2;
pub const CHAR_SHIFT: u32 = 8;
pub const CHAR_TAG: i64 = 0x0F;
pub const FALSE: i64 = 0x2F;
pub const TRUE: i64 = 0x6F;
pub const EMPTY: i64 = 0x3F;
pub const PAIR_TAG: i64 = 0x1;

const FIXNUM_MAX: i64 = (1 << 61) - 1;
const FIXNUM_MIN: i64 = -(1 << 61);
const MAX_SLOTS: usize = 4095; // limit of `ldr x, [sp, #imm]` (imm/8 <= 4095)

/// Lexical environment: variable name -> stack slot. Searched from the end so
/// inner bindings shadow outer ones.
type Env = Vec<(String, usize)>;

type Result<T> = std::result::Result<T, String>;

struct Compiler {
    out: String,
    labels: usize,
    max_slots: usize,
}

macro_rules! emit {
    ($c:expr, $($arg:tt)*) => {{
        $c.out.push_str("    ");
        writeln!($c.out, $($arg)*).unwrap();
    }};
}

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
        return Err(format!("{name} expects {n} argument(s), got {}", args.len()));
    }
    Ok(())
}

impl Compiler {
    fn new_label(&mut self) -> String {
        self.labels += 1;
        format!("L{}", self.labels)
    }

    fn label(&mut self, l: &str) {
        writeln!(self.out, "{l}:").unwrap();
    }

    fn slot(&mut self, si: usize) -> Result<String> {
        if si >= MAX_SLOTS {
            return Err("expression too deeply nested (out of stack slots)".into());
        }
        self.max_slots = self.max_slots.max(si + 1);
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

    /// After a `cmp`, turn condition `cond` into `#t`/`#f` in x0.
    fn emit_bool(&mut self, cond: &str) {
        emit!(self, "cset x0, {cond}"); // x0 = 0 or 1
        emit!(self, "lsl x0, x0, #6"); //  x0 = 0 or 0x40
        emit!(self, "add x0, x0, #{FALSE}"); // x0 = 0x2F or 0x6F
    }

    /// Compile `e` so its value ends up in x0. `si` is the next free stack slot.
    fn compile(&mut self, e: &Expr, env: &Env, si: usize) -> Result<()> {
        if let Some(imm) = immediate(e)? {
            self.load_imm("x0", imm);
            return Ok(());
        }
        match e {
            Expr::Symbol(name) => {
                let (_, slot) = env
                    .iter()
                    .rev()
                    .find(|(n, _)| n == name)
                    .ok_or_else(|| format!("unbound variable: {name}"))?;
                emit!(self, "ldr x0, [sp, #{}]", slot * 8);
                Ok(())
            }
            Expr::List(items) => {
                let Expr::Symbol(head) = &items[0] else {
                    return Err(format!("can't apply {:?} (no first-class functions yet)", items[0]));
                };
                let args = &items[1..];
                // a local variable named like a primitive would be a function call,
                // which isn't supported yet
                if env.iter().any(|(n, _)| n == head) {
                    return Err(format!("{head} is a variable, not a function (no lambda yet)"));
                }
                self.compile_form(head, args, env, si)
            }
            _ => unreachable!("immediates handled above"),
        }
    }

    fn compile_body(&mut self, body: &[Expr], env: &Env, si: usize) -> Result<()> {
        if body.is_empty() {
            return Err("empty body".into());
        }
        // no side effects yet, but evaluate everything for when there are
        for e in body {
            self.compile(e, env, si)?;
        }
        Ok(())
    }

    fn compile_form(&mut self, head: &str, args: &[Expr], env: &Env, si: usize) -> Result<()> {
        match head {
            // ───────── special forms ─────────
            "quote" => {
                check_arity("quote", args, 1)?;
                match &args[0] {
                    // '(a b c)  ==>  (cons 'a (cons 'b (cons 'c '())))
                    Expr::List(items) if !items.is_empty() => {
                        let expanded = items.iter().rev().fold(list(vec![]), |acc, item| {
                            list(vec![sym("cons"), list(vec![sym("quote"), item.clone()]), acc])
                        });
                        self.compile(&expanded, env, si)
                    }
                    Expr::Symbol(s) => Err(format!("quoted symbols ('{s}) aren't supported yet")),
                    lit => self.compile(lit, env, si),
                }
            }
            "if" => {
                if !(2..=3).contains(&args.len()) {
                    return Err("if expects 2 or 3 arguments".into());
                }
                let (else_l, end_l) = (self.new_label(), self.new_label());
                self.compile(&args[0], env, si)?;
                emit!(self, "cmp x0, #{FALSE}");
                emit!(self, "b.eq {else_l}");
                self.compile(&args[1], env, si)?;
                emit!(self, "b {end_l}");
                self.label(&else_l);
                self.compile(args.get(2).unwrap_or(&Expr::Bool(false)), env, si)?;
                self.label(&end_l);
                Ok(())
            }
            "and" | "or" => {
                if args.is_empty() {
                    return self.compile(&Expr::Bool(head == "and"), env, si);
                }
                // stop early when a value is #f (and) or not #f (or); x0 already holds the answer
                let end_l = self.new_label();
                let branch = if head == "and" { "b.eq" } else { "b.ne" };
                for (i, a) in args.iter().enumerate() {
                    self.compile(a, env, si)?;
                    if i + 1 < args.len() {
                        emit!(self, "cmp x0, #{FALSE}");
                        emit!(self, "{branch} {end_l}");
                    }
                }
                self.label(&end_l);
                Ok(())
            }
            "cond" => {
                // (cond [t1 e1...] [t2 e2...] [else e...])  ==>  nested ifs
                let mut expanded = Expr::Bool(false);
                for clause in args.iter().rev() {
                    let Expr::List(parts) = clause else {
                        return Err("cond clause must be a list".into());
                    };
                    let [test, body @ ..] = parts.as_slice() else {
                        return Err("empty cond clause".into());
                    };
                    let mut body_expr = vec![sym("begin")];
                    body_expr.extend_from_slice(body);
                    expanded = if *test == sym("else") {
                        list(body_expr)
                    } else if body.is_empty() {
                        list(vec![sym("or"), test.clone(), expanded])
                    } else {
                        list(vec![sym("if"), test.clone(), list(body_expr), expanded])
                    };
                }
                self.compile(&expanded, env, si)
            }
            "begin" => self.compile_body(args, env, si),
            "let" => {
                let [Expr::List(bindings), body @ ..] = args else {
                    return Err("let expects a binding list and a body".into());
                };
                // all right-hand sides see the *outer* env; each result gets its own slot
                let mut new_env = env.clone();
                for (i, b) in bindings.iter().enumerate() {
                    let (name, rhs) = parse_binding(b)?;
                    self.compile(rhs, env, si + i)?;
                    let slot = self.slot(si + i)?;
                    emit!(self, "str x0, {slot}");
                    new_env.push((name.to_string(), si + i));
                }
                self.compile_body(body, &new_env, si + bindings.len())
            }
            "let*" => {
                // (let* (a b c) body)  ==>  (let (a) (let* (b c) body))
                let [Expr::List(bindings), body @ ..] = args else {
                    return Err("let* expects a binding list and a body".into());
                };
                let Some((first, rest)) = bindings.split_first() else {
                    let mut e = vec![sym("let"), list(vec![])];
                    e.extend_from_slice(body);
                    return self.compile(&list(e), env, si);
                };
                let mut inner = vec![sym("let*"), list(rest.to_vec())];
                inner.extend_from_slice(body);
                let outer = list(vec![sym("let"), list(vec![first.clone()]), list(inner)]);
                self.compile(&outer, env, si)
            }
            "list" => {
                // (list a b)  ==>  (cons a (cons b '()))
                let expanded = args
                    .iter()
                    .rev()
                    .fold(list(vec![]), |acc, a| list(vec![sym("cons"), a.clone(), acc]));
                self.compile(&expanded, env, si)
            }

            // ───────── unary primitives ─────────
            "add1" | "sub1" | "zero?" | "null?" | "not" | "integer?" | "boolean?" | "char?"
            | "pair?" | "char->integer" | "integer->char" | "car" | "cdr" => {
                check_arity(head, args, 1)?;
                self.compile(&args[0], env, si)?;
                self.unary(head);
                Ok(())
            }

            // ───────── variadic arithmetic ─────────
            "+" | "*" if args.is_empty() => {
                self.compile(&Expr::Int(if head == "+" { 0 } else { 1 }), env, si)
            }
            "-" if args.len() == 1 => {
                self.compile(&args[0], env, si)?;
                emit!(self, "neg x0, x0");
                Ok(())
            }
            "+" | "-" | "*" if args.len() > 2 => {
                // (+ a b c)  ==>  (+ (+ a b) c)
                let folded = args[1..].iter().fold(args[0].clone(), |acc, a| {
                    list(vec![sym(head), acc, a.clone()])
                });
                self.compile(&folded, env, si)
            }
            "+" | "-" | "*" if args.len() == 1 => self.compile(&args[0], env, si),

            // ───────── binary primitives ─────────
            "+" | "-" | "*" | "=" | "<" | ">" | "<=" | ">=" | "eq?" | "cons" => {
                check_arity(head, args, 2)?;
                // evaluate b first and park it in a slot, then a into x0, b into x1
                self.compile(&args[1], env, si)?;
                let slot = self.slot(si)?;
                emit!(self, "str x0, {slot}");
                self.compile(&args[0], env, si + 1)?;
                emit!(self, "ldr x1, {slot}");
                self.binary(head);
                Ok(())
            }

            _ => Err(format!("unknown function or form: {head}")),
        }
    }

    /// x0 = op(x0)
    fn unary(&mut self, op: &str) {
        match op {
            "add1" => emit!(self, "add x0, x0, #{}", 1 << FIXNUM_SHIFT),
            "sub1" => emit!(self, "sub x0, x0, #{}", 1 << FIXNUM_SHIFT),
            "zero?" => {
                emit!(self, "cmp x0, #0");
                self.emit_bool("eq");
            }
            "null?" => {
                emit!(self, "cmp x0, #{EMPTY}");
                self.emit_bool("eq");
            }
            "not" => {
                emit!(self, "cmp x0, #{FALSE}");
                self.emit_bool("eq");
            }
            "integer?" => {
                emit!(self, "and x0, x0, #3");
                emit!(self, "cmp x0, #0");
                self.emit_bool("eq");
            }
            "char?" => {
                emit!(self, "and x0, x0, #0xff");
                emit!(self, "cmp x0, #{CHAR_TAG}");
                self.emit_bool("eq");
            }
            "boolean?" => {
                // #f and #t differ only in bit 6; clear it and compare to #f
                emit!(self, "and x0, x0, #0xffffffffffffffbf");
                emit!(self, "cmp x0, #{FALSE}");
                self.emit_bool("eq");
            }
            "pair?" => {
                emit!(self, "and x0, x0, #7");
                emit!(self, "cmp x0, #{PAIR_TAG}");
                self.emit_bool("eq");
            }
            "char->integer" => emit!(self, "lsr x0, x0, #{}", CHAR_SHIFT - FIXNUM_SHIFT),
            "integer->char" => {
                emit!(self, "lsl x0, x0, #{}", CHAR_SHIFT - FIXNUM_SHIFT);
                emit!(self, "add x0, x0, #{CHAR_TAG}");
            }
            // pointer is tagged with +1, so the fields are at -1 and +7
            "car" => emit!(self, "ldur x0, [x0, #-1]"),
            "cdr" => emit!(self, "ldur x0, [x0, #7]"),
            _ => unreachable!(),
        }
    }

    /// x0 = op(x0, x1)
    fn binary(&mut self, op: &str) {
        let cmp = |c: &mut Self, cond: &str| {
            emit!(c, "cmp x0, x1");
            c.emit_bool(cond);
        };
        match op {
            "+" => emit!(self, "add x0, x0, x1"),
            "-" => emit!(self, "sub x0, x0, x1"),
            "*" => {
                // (a<<2) * (b<<2) would be ab<<4, so untag one side first
                emit!(self, "asr x0, x0, #{FIXNUM_SHIFT}");
                emit!(self, "mul x0, x0, x1");
            }
            "=" | "eq?" => cmp(self, "eq"),
            "<" => cmp(self, "lt"),
            ">" => cmp(self, "gt"),
            "<=" => cmp(self, "le"),
            ">=" => cmp(self, "ge"),
            "cons" => {
                emit!(self, "str x0, [x28]");
                emit!(self, "str x1, [x28, #8]");
                emit!(self, "add x0, x28, #{PAIR_TAG}");
                emit!(self, "add x28, x28, #16");
            }
            _ => unreachable!(),
        }
    }
}

fn parse_binding(b: &Expr) -> Result<(&str, &Expr)> {
    match b {
        Expr::List(pair) => match pair.as_slice() {
            [Expr::Symbol(name), rhs] => Ok((name, rhs)),
            _ => Err("let binding must look like (name expr)".into()),
        },
        _ => Err("let binding must look like (name expr)".into()),
    }
}

/// Compile a whole program into an assembly file defining
/// `int64_t scheme_entry(void *heap)`.
pub fn compile_program(expr: &Expr) -> Result<String> {
    let mut c = Compiler { out: String::new(), labels: 0, max_slots: 0 };
    c.compile(expr, &Vec::new(), 0)?;
    let body = std::mem::take(&mut c.out);
    let frame = (c.max_slots * 8).div_ceil(16) * 16; // sp must stay 16-byte aligned

    c.out.push_str("    .text\n    .globl _scheme_entry\n    .p2align 2\n_scheme_entry:\n");
    emit!(c, "stp x29, x30, [sp, #-16]!");
    emit!(c, "mov x29, sp");
    emit!(c, "str x28, [sp, #-16]!"); // x28 is callee-saved
    emit!(c, "mov x28, x0"); // heap pointer from the runtime
    c.load_imm("x9", frame as i64);
    emit!(c, "sub sp, sp, x9");
    c.out.push_str(&body);
    c.load_imm("x9", frame as i64);
    emit!(c, "add sp, sp, x9");
    emit!(c, "ldr x28, [sp], #16");
    emit!(c, "ldp x29, x30, [sp], #16");
    emit!(c, "ret");
    Ok(c.out)
}
