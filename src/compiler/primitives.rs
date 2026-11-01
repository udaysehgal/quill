//! Built-in operations, compiled inline with runtime type checks.

use super::*;

/// (min, max) argument counts; `None` max means variadic.
fn arity(name: &str) -> Option<(usize, Option<usize>)> {
    Some(match name {
        "newline" => (0, Some(0)),
        "exit" => (0, Some(1)),
        "add1" | "sub1" | "zero?" | "null?" | "not" | "integer?" | "boolean?" | "char?"
        | "pair?" | "procedure?" | "vector?" | "string?" | "symbol?" | "char->integer"
        | "integer->char" | "car" | "cdr" | "vector-length" | "string-length" | "display"
        | "write" | "string->symbol" | "%symbol-chars" => (1, Some(1)),
        "=" | "<" | ">" | "<=" | ">=" | "eq?" | "eqv?" | "cons" | "quotient" | "remainder"
        | "modulo" | "vector-ref" | "string-ref" | "set-car!" | "set-cdr!" | "char=?"
        | "char<?" | "char>?" | "char<=?" | "char>=?" => (2, Some(2)),
        "vector-set!" | "string-set!" => (3, Some(3)),
        "make-vector" | "make-string" | "error" => (1, Some(2)),
        "+" | "*" | "list" | "vector" => (0, None),
        "-" => (1, None),
        _ => return None,
    })
}

pub fn is_primitive(name: &str) -> bool {
    arity(name).is_some()
}

/// Arity of the wrapper function used when a fixed-arity primitive is passed
/// as a value. (Variadic ones use library functions, see `VALUE_WRAPPERS`.)
pub(super) fn value_arity(name: &str) -> Option<usize> {
    match arity(name)? {
        (min, Some(max)) if min == max => Some(min),
        (min, Some(_)) => Some(min),
        _ => None,
    }
}

impl Compiler {
    pub(super) fn compile_primitive(
        &mut self,
        name: &str,
        args: &[Expr],
        env: &Env,
        si: usize,
    ) -> Result<()> {
        let (min, max) = arity(name).unwrap();
        if args.len() < min || max.is_some_and(|max| args.len() > max) {
            let expected = match max {
                Some(max) if max == min => format!("{min}"),
                Some(max) => format!("{min} to {max}"),
                None => format!("at least {min}"),
            };
            return Err(format!(
                "{name} expects {expected} argument(s), got {}",
                args.len()
            ));
        }

        // variadic arithmetic and optional arguments become fixed-arity forms
        match (name, args.len()) {
            ("+" | "*", 0) => {
                return self.compile(&Expr::Int((name == "*") as i64), env, si, false);
            }
            ("+" | "*", 1) => {
                self.compile(&args[0], env, si, false)?;
                self.check_fixnum("x0", name);
                return Ok(());
            }
            ("-", 1) => {
                self.compile(&args[0], env, si, false)?;
                self.check_fixnum("x0", name);
                let err = self.overflow_label(name);
                emit!(self, "negs x0, x0");
                emit!(self, "b.vs {err}");
                return Ok(());
            }
            ("+" | "-" | "*", n) if n > 2 => {
                // (+ a b c) ==> (+ (+ a b) c)
                let folded = args[1..].iter().fold(args[0].clone(), |acc, a| {
                    list(vec![sym(name), acc, a.clone()])
                });
                return self.compile(&folded, env, si, false);
            }
            ("make-vector" | "make-string" | "error", 1) => {
                let default = match name {
                    "make-vector" => Expr::Int(0),
                    "make-string" => Expr::Char(' '),
                    _ => list(vec![sym("%void")]),
                };
                return self.compile_primitive(name, &[args[0].clone(), default], env, si);
            }
            ("exit", 0) => return self.compile_primitive(name, &[Expr::Int(0)], env, si),
            ("list", n) => {
                self.args_to_slots(args, env, si)?;
                return self.list_from_slots(si, n, None);
            }
            ("vector", n) => {
                self.args_to_slots(args, env, si)?;
                return self.vector_from_slots(si, n);
            }
            _ => {}
        }

        if args.len() == 1 {
            self.compile(&args[0], env, si, false)?;
        } else {
            self.args_to_slots(args, env, si)?;
            for i in 0..args.len() {
                self.load_slot(&format!("x{i}"), si + i);
            }
        }
        self.primitive_op(name, si);
        Ok(())
    }

    // ───────── type checks: branch to an error stub on failure ─────────

    pub(super) fn check_fixnum(&mut self, reg: &str, who: &str) {
        let err = self.error_label(
            &format!("{who}: expected an integer, got"),
            &format!("mov x1, {reg}"),
        );
        emit!(self, "tst {reg}, #3");
        emit!(self, "b.ne {err}");
    }

    fn check_fixnums(&mut self, who: &str) {
        // blame whichever of x0/x1 isn't a fixnum
        let err = self.error_label(
            &format!("{who}: expected an integer, got"),
            "tst x0, #3\ncsel x1, x1, x0, eq",
        );
        emit!(self, "orr x9, x0, x1");
        emit!(self, "tst x9, #3");
        emit!(self, "b.ne {err}");
    }

    fn overflow_label(&mut self, who: &str) -> String {
        self.error_label(
            &format!("{who}: integer overflow (integers are 62-bit)"),
            "",
        )
    }

    fn check_tag(&mut self, reg: &str, mask: i64, tag: i64, who: &str, what: &str) {
        let err = self.error_label(
            &format!("{who}: expected {what}, got"),
            &format!("mov x1, {reg}"),
        );
        emit!(self, "and x9, {reg}, #{mask}");
        emit!(self, "cmp x9, #{tag}");
        emit!(self, "b.ne {err}");
    }

    fn check_pair(&mut self, reg: &str, who: &str) {
        self.check_tag(reg, 7, PAIR_TAG, who, "a pair");
    }

    fn check_char(&mut self, reg: &str, who: &str) {
        self.check_tag(reg, 0xff, CHAR_TAG, who, "a character");
    }

    /// x0 = object with `tag` and a length field, x1 = index.
    fn check_index(&mut self, tag: i64, who: &str, what: &str) {
        self.check_tag("x0", 7, tag, who, what);
        self.check_fixnum("x1", who);
        let err = self.error_label(&format!("{who}: index out of range:"), "mov x1, x1");
        emit!(self, "ldur x9, [x0, #-{tag}]");
        emit!(self, "cmp x1, x9");
        emit!(self, "b.hs {err}"); // unsigned, so negative indexes fail too
    }

    fn check_length(&mut self, reg: &str, who: &str) {
        let err = self.error_label(
            &format!("{who}: expected a non-negative length, got"),
            &format!("mov x1, {reg}"),
        );
        emit!(self, "tst {reg}, #0x8000000000000003"); // fixnum and not negative
        emit!(self, "b.ne {err}");
    }

    // ───────── constructors ─────────

    /// x0 = a list of the values in slots `base..base+n`, ending in the value
    /// in slot `tail` (or `()`).
    pub(super) fn list_from_slots(
        &mut self,
        base: usize,
        n: usize,
        tail: Option<usize>,
    ) -> Result<()> {
        if n == 0 {
            match tail {
                Some(t) => self.load_slot("x0", t),
                None => self.load_imm("x0", EMPTY),
            }
            return Ok(());
        }
        // one allocation for all n pairs, laid out consecutively
        self.alloc(16 * n);
        emit!(self, "mov x2, x0");
        for i in 0..n {
            self.load_slot("x1", base + i);
            if i + 1 < n {
                emit!(self, "add x3, x2, #{}", 16 + PAIR_TAG);
            } else {
                match tail {
                    Some(t) => self.load_slot("x3", t),
                    None => self.load_imm("x3", EMPTY),
                }
            }
            emit!(self, "stp x1, x3, [x2], #16");
        }
        emit!(self, "add x0, x0, #{PAIR_TAG}");
        Ok(())
    }

    pub(super) fn vector_from_slots(&mut self, base: usize, n: usize) -> Result<()> {
        self.alloc((8 + 8 * n).div_ceil(16) * 16);
        self.load_imm("x1", (n as i64) << FIXNUM_SHIFT);
        emit!(self, "mov x2, x0");
        emit!(self, "str x1, [x2], #8");
        for i in 0..n {
            self.load_slot("x1", base + i);
            emit!(self, "str x1, [x2], #8");
        }
        emit!(self, "add x0, x0, #{VECTOR_TAG}");
        Ok(())
    }

    /// Call a C runtime function with the arguments in x0 (and x1).
    fn call_runtime(&mut self, func: &str, returns_void: bool) {
        emit!(self, "bl _{func}");
        if returns_void {
            self.load_imm("x0", VOID);
        }
    }

    /// Operands are in x0 (and x1, x2), and also in slots `si..` when there's
    /// more than one, so they can be reloaded after an allocation.
    fn primitive_op(&mut self, name: &str, si: usize) {
        match name {
            // ── fixnums (all arithmetic is checked for overflow)
            "add1" | "sub1" => {
                self.check_fixnum("x0", name);
                let err = self.overflow_label(name);
                let op = if name == "add1" { "adds" } else { "subs" };
                emit!(self, "{op} x0, x0, #{}", 1 << FIXNUM_SHIFT);
                emit!(self, "b.vs {err}");
            }
            "zero?" => {
                self.check_fixnum("x0", name);
                emit!(self, "cmp x0, #0");
                self.emit_bool("eq");
            }
            "+" | "-" => {
                self.check_fixnums(name);
                let err = self.overflow_label(name);
                let op = if name == "+" { "adds" } else { "subs" };
                emit!(self, "{op} x0, x0, x1");
                emit!(self, "b.vs {err}");
            }
            "*" => {
                self.check_fixnums(name);
                let err = self.overflow_label(name);
                // (a<<2) * (b<<2) would be ab<<4, so untag one side first;
                // it overflowed unless the high half is just the sign of the low half
                emit!(self, "asr x2, x0, #{FIXNUM_SHIFT}");
                emit!(self, "mul x0, x2, x1");
                emit!(self, "smulh x3, x2, x1");
                emit!(self, "cmp x3, x0, asr #63");
                emit!(self, "b.ne {err}");
            }
            "quotient" | "remainder" | "modulo" => {
                self.check_fixnums(name);
                let err = self.error_label(&format!("{name}: division by zero"), "");
                emit!(self, "cbz x1, {err}");
                emit!(self, "sdiv x2, x0, x1"); // (a<<2)/(b<<2) = a/b, untagged
                if name == "quotient" {
                    // only the most negative fixnum divided by -1 can overflow
                    let err = self.overflow_label(name);
                    emit!(self, "lsl x0, x2, #{FIXNUM_SHIFT}");
                    emit!(self, "cmp x2, x0, asr #{FIXNUM_SHIFT}");
                    emit!(self, "b.ne {err}");
                } else {
                    emit!(self, "msub x0, x2, x1, x0"); // a - (a/b)*b, still tagged
                    if name == "modulo" {
                        // the result takes the sign of the divisor
                        let done = self.fresh("L");
                        emit!(self, "cbz x0, {done}");
                        emit!(self, "eor x3, x0, x1");
                        emit!(self, "tbz x3, #63, {done}");
                        emit!(self, "add x0, x0, x1");
                        self.label(&done);
                    }
                }
            }
            "=" | "<" | ">" | "<=" | ">=" => {
                self.check_fixnums(name);
                let cond = match name {
                    "=" => "eq",
                    "<" => "lt",
                    ">" => "gt",
                    "<=" => "le",
                    _ => "ge",
                };
                emit!(self, "cmp x0, x1");
                self.emit_bool(cond);
            }
            "char=?" | "char<?" | "char>?" | "char<=?" | "char>=?" => {
                self.check_char("x0", name);
                self.check_char("x1", name);
                let cond = match name {
                    "char=?" => "eq",
                    "char<?" => "lo",
                    "char>?" => "hi",
                    "char<=?" => "ls",
                    _ => "hs",
                };
                emit!(self, "cmp x0, x1");
                self.emit_bool(cond);
            }
            "eq?" | "eqv?" => {
                emit!(self, "cmp x0, x1");
                self.emit_bool("eq");
            }

            // ── type predicates
            "null?" | "not" => {
                emit!(
                    self,
                    "cmp x0, #{}",
                    if name == "null?" { EMPTY } else { FALSE }
                );
                self.emit_bool("eq");
            }
            "integer?" => {
                emit!(self, "tst x0, #3");
                self.emit_bool("eq");
            }
            "boolean?" => {
                // #f and #t differ only in bit 6; clear it and compare to #f
                emit!(self, "and x0, x0, #0xffffffffffffffbf");
                emit!(self, "cmp x0, #{FALSE}");
                self.emit_bool("eq");
            }
            "char?" | "pair?" | "procedure?" | "vector?" | "string?" | "symbol?" => {
                let (mask, tag) = match name {
                    "char?" => (0xff, CHAR_TAG),
                    "pair?" => (7, PAIR_TAG),
                    "procedure?" => (7, CLOSURE_TAG),
                    "vector?" => (7, VECTOR_TAG),
                    "string?" => (7, STRING_TAG),
                    _ => (7, SYMBOL_TAG),
                };
                emit!(self, "and x0, x0, #{mask}");
                emit!(self, "cmp x0, #{tag}");
                self.emit_bool("eq");
            }

            // ── chars
            "char->integer" => {
                self.check_char("x0", name);
                emit!(self, "lsr x0, x0, #{}", CHAR_SHIFT - FIXNUM_SHIFT);
            }
            "integer->char" => {
                self.check_fixnum("x0", name);
                let err = self.error_label(&format!("{name}: not a character code:"), "mov x1, x0");
                self.load_imm("x9", 0x10FFFF << FIXNUM_SHIFT);
                emit!(self, "cmp x0, x9");
                emit!(self, "b.hi {err}"); // unsigned: negative codes fail too
                emit!(self, "lsl x0, x0, #{}", CHAR_SHIFT - FIXNUM_SHIFT);
                emit!(self, "add x0, x0, #{CHAR_TAG}");
            }

            // ── pairs
            "cons" => {
                self.alloc(16);
                self.load_slot("x1", si);
                self.load_slot("x2", si + 1);
                emit!(self, "stp x1, x2, [x0]");
                emit!(self, "add x0, x0, #{PAIR_TAG}");
            }
            // the pointer is tagged with +1, so the fields are at -1 and +7
            "car" | "cdr" => {
                self.check_pair("x0", name);
                emit!(
                    self,
                    "ldur x0, [x0, #{}]",
                    if name == "car" {
                        -PAIR_TAG
                    } else {
                        8 - PAIR_TAG
                    }
                );
            }
            "set-car!" | "set-cdr!" => {
                self.check_pair("x0", name);
                emit!(
                    self,
                    "stur x1, [x0, #{}]",
                    if name == "set-car!" {
                        -PAIR_TAG
                    } else {
                        8 - PAIR_TAG
                    }
                );
                self.load_imm("x0", VOID);
            }

            // ── vectors: [len, elems...]
            "make-vector" => {
                self.check_length("x0", name);
                emit!(self, "lsl x10, x0, #1"); // n*8 (x0 is n<<2)
                emit!(self, "add x10, x10, #23"); // + 8 for the length, rounded up to 16
                emit!(self, "and x10, x10, #0xfffffffffffffff0");
                emit!(self, "bl scm_alloc");
                self.load_slot("x1", si);
                self.load_slot("x2", si + 1);
                emit!(self, "str x1, [x0]");
                self.fill_loop("str x2, [x3], #8", "add x4, x3, x1, lsl #1");
                emit!(self, "add x0, x0, #{VECTOR_TAG}");
            }
            "vector-length" => {
                self.check_tag("x0", 7, VECTOR_TAG, name, "a vector");
                emit!(self, "ldur x0, [x0, #-{VECTOR_TAG}]");
            }
            "vector-ref" => {
                self.check_index(VECTOR_TAG, name, "a vector");
                emit!(self, "add x3, x0, x1, lsl #1"); // + i*8
                emit!(self, "ldur x0, [x3, #{}]", 8 - VECTOR_TAG);
            }
            "vector-set!" => {
                self.check_index(VECTOR_TAG, name, "a vector");
                emit!(self, "add x3, x0, x1, lsl #1");
                emit!(self, "stur x2, [x3, #{}]", 8 - VECTOR_TAG);
                self.load_imm("x0", VOID);
            }

            // ── strings: [len, bytes...]
            "make-string" => {
                self.check_length("x0", name);
                self.check_char("x1", name);
                emit!(self, "asr x10, x0, #{FIXNUM_SHIFT}");
                emit!(self, "add x10, x10, #23");
                emit!(self, "and x10, x10, #0xfffffffffffffff0");
                emit!(self, "bl scm_alloc");
                self.load_slot("x1", si);
                self.load_slot("x2", si + 1);
                emit!(self, "lsr x2, x2, #{CHAR_SHIFT}");
                emit!(self, "str x1, [x0]");
                self.fill_loop("strb w2, [x3], #1", "add x4, x3, x1, asr #2");
                emit!(self, "add x0, x0, #{STRING_TAG}");
            }
            "string-length" => {
                self.check_tag("x0", 7, STRING_TAG, name, "a string");
                emit!(self, "ldur x0, [x0, #-{STRING_TAG}]");
            }
            "string-ref" => {
                self.check_index(STRING_TAG, name, "a string");
                emit!(self, "add x3, x0, x1, asr #2"); // + i
                emit!(self, "ldurb w0, [x3, #{}]", 8 - STRING_TAG);
                emit!(self, "lsl x0, x0, #{CHAR_SHIFT}");
                emit!(self, "add x0, x0, #{CHAR_TAG}");
            }
            "string-set!" => {
                self.check_index(STRING_TAG, name, "a string");
                self.check_char("x2", name);
                emit!(self, "lsr x2, x2, #{CHAR_SHIFT}");
                emit!(self, "add x3, x0, x1, asr #2");
                emit!(self, "sturb w2, [x3, #{}]", 8 - STRING_TAG);
                self.load_imm("x0", VOID);
            }

            // ── symbols
            "%symbol-chars" => {
                // a symbol has the same layout as a string; just retag it
                self.check_tag("x0", 7, SYMBOL_TAG, "symbol->string", "a symbol");
                emit!(self, "add x0, x0, #{}", STRING_TAG - SYMBOL_TAG);
            }
            "string->symbol" => {
                // interned by the runtime (symbols live outside the GC heap)
                self.check_tag("x0", 7, STRING_TAG, name, "a string");
                self.call_runtime("scheme_string_to_symbol", false);
            }

            // ── I/O and control
            "display" => self.call_runtime("scheme_display", true),
            "write" => self.call_runtime("scheme_write", true),
            "newline" => {
                self.load_imm("x0", (('\n' as i64) << CHAR_SHIFT) | CHAR_TAG);
                self.call_runtime("scheme_display", true);
            }
            "error" => emit!(self, "bl _scheme_user_error"), // doesn't return
            "exit" => {
                self.check_fixnum("x0", name);
                emit!(self, "asr x0, x0, #{FIXNUM_SHIFT}");
                emit!(self, "bl _scheme_exit"); // doesn't return
            }

            _ => unreachable!("unhandled primitive {name}"),
        }
    }

    /// x0 = raw object; fill from x0+8 to x4 using `store` (which advances x3).
    /// `end` computes x4 from x3 and the tagged length in x1.
    fn fill_loop(&mut self, store: &str, end: &str) {
        let (top, done) = (self.fresh("L"), self.fresh("L"));
        emit!(self, "add x3, x0, #8");
        emit!(self, "{end}");
        self.label(&top);
        emit!(self, "cmp x3, x4");
        emit!(self, "b.hs {done}");
        emit!(self, "{store}");
        emit!(self, "b {top}");
        self.label(&done);
    }
}
