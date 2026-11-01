//! The expander: rewrites a program into core forms before code generation.
//!
//! Core forms: `quote if and or begin let lambda set! %void`, calls,
//! variables and literals. Everything else is rewritten here:
//! `syntax-rules` macros, `cond case when unless let* letrec letrec* do`,
//! named `let`, quasiquote, and internal `define`s (which become `letrec*`).
//!
//! Expansions only introduce keywords, `%`-prefixed internal names and
//! fresh names containing a space (which users can't write), so they can't
//! be captured by user variables.
//!
//! Macros are partly hygienic: a template's own temporaries (names that
//! aren't pattern variables, keywords or globals, like `tmp` in `swap!`) are
//! renamed on every expansion. Macro names act like keywords: they can't be
//! rebound.

use crate::reader::{Expr, Line};
use crate::syntax::*;
use std::collections::{HashMap, HashSet};

const MAX_EXPANSION_DEPTH: usize = 10_000;

struct Macro {
    literals: Vec<String>,
    /// (pattern without the keyword, template)
    rules: Vec<(Expr, Expr)>,
}

#[derive(Clone, Debug)]
enum Binding {
    One(Expr),
    Many(Vec<Binding>),
}

type Bindings = HashMap<String, Binding>;

#[derive(Default)]
pub struct Expander {
    macros: HashMap<String, Macro>,
    counter: usize,
    depth: usize,
}

fn with_line(e: Expr, line: Line) -> Expr {
    match e {
        Expr::List(items, Line(0)) => Expr::List(items, line),
        e => e,
    }
}

fn begin(body: &[Expr]) -> Expr {
    let mut v = vec![sym("begin")];
    v.extend_from_slice(body);
    list(v)
}

fn void() -> Expr {
    list(vec![sym("%void")])
}

fn quote(e: Expr) -> Expr {
    list(vec![sym("quote"), e])
}

fn head_is(e: &Expr, name: &str) -> bool {
    matches!(e, Expr::List(items, _) if items.first() == Some(&sym(name)))
}

impl Expander {
    fn gensym(&mut self, base: &str) -> String {
        self.counter += 1;
        format!("{base} {}", self.counter)
    }

    pub fn is_macro(&self, name: &str) -> bool {
        self.macros.contains_key(name)
    }

    fn check_not_macro(&self, name: &str) -> Result<()> {
        if self.is_macro(name) {
            return Err(format!(
                "{name} is a macro and can't be used as a variable name"
            ));
        }
        Ok(())
    }

    /// Register `(define-syntax name (syntax-rules (literals...) [pattern template]...))`.
    /// Returns false if `form` isn't a `define-syntax`.
    pub fn define_macro(&mut self, form: &Expr) -> Result<bool> {
        if !head_is(form, "define-syntax") {
            return Ok(false);
        }
        let Expr::List(items, line) = form else {
            unreachable!()
        };
        let located = |e: String| at_line(line.0, e);
        let [_, Expr::Symbol(name), Expr::List(spec, _)] = items.as_slice() else {
            return Err(located(
                "define-syntax expects a name and a (syntax-rules ...) form".into(),
            ));
        };
        check_bindable(name).map_err(located)?;
        let [Expr::Symbol(kind), Expr::List(lits, _), rules @ ..] = spec.as_slice() else {
            return Err(located(
                "expected (syntax-rules (literals...) [pattern template]...)".into(),
            ));
        };
        if kind != "syntax-rules" {
            return Err(located(format!(
                "only syntax-rules macros are supported, not {kind}"
            )));
        }
        let literals = lits
            .iter()
            .map(|l| match l {
                Expr::Symbol(s) => Ok(s.clone()),
                _ => Err(located("syntax-rules literals must be names".into())),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut parsed = Vec::new();
        for rule in rules {
            let Expr::List(parts, l) = rule else {
                return Err(located(
                    "a syntax-rules rule must be [pattern template]".into(),
                ));
            };
            let [Expr::List(pattern, pl), template] = parts.as_slice() else {
                return Err(at_line(
                    l.0,
                    format!("{name}: a rule must be [(pattern...) template]"),
                ));
            };
            // the keyword position of the pattern is ignored
            let pattern = Expr::List(pattern.iter().skip(1).cloned().collect(), *pl);
            parsed.push((pattern, template.clone()));
        }
        self.macros.insert(
            name.clone(),
            Macro {
                literals,
                rules: parsed,
            },
        );
        Ok(true)
    }

    /// Expand macro uses at the head of `e` only (to see whether a top-level
    /// or body form is a `define` or `begin`).
    pub fn expand_head(&mut self, e: &Expr) -> Result<Expr> {
        let mut e = e.clone();
        let mut steps = 0;
        while let Expr::List(items, line) = &e
            && let Some(Expr::Symbol(head)) = items.first()
            && self.is_macro(head)
        {
            let line = *line;
            e = self
                .apply_macro(head, &items[1..], line)
                .map_err(|err| at_line(line.0, err))?;
            steps += 1;
            if steps > MAX_EXPANSION_DEPTH {
                return Err(at_line(line.0, "macro expansion doesn't terminate".into()));
            }
        }
        Ok(e)
    }

    /// Fully expand an expression.
    pub fn expand(&mut self, e: &Expr) -> Result<Expr> {
        match e {
            Expr::List(items, line) if !items.is_empty() => {
                self.depth += 1;
                if self.depth > MAX_EXPANSION_DEPTH {
                    return Err(at_line(line.0, "macro expansion is too deep".into()));
                }
                let r = self
                    .expand_list(items, *line)
                    .map_err(|err| at_line(line.0, err));
                self.depth -= 1;
                r
            }
            _ => Ok(e.clone()),
        }
    }

    /// Expand a body: leading `define`s (possibly produced by macros, or
    /// inside a `begin`) become a `letrec*`.
    pub fn expand_body(&mut self, body: &[Expr]) -> Result<Vec<Expr>> {
        let mut forms = Vec::new();
        for f in body {
            self.flatten_into(f, &mut forms)?;
        }
        let mut bindings = Vec::new();
        let mut rest = forms.as_slice();
        while let Some((first, tail)) = rest.split_first() {
            let Some((name, def)) = parse_define(first)? else {
                break;
            };
            self.check_not_macro(&name)?;
            bindings.push(list(vec![sym(&name), def_value(def)]));
            rest = tail;
        }
        if bindings.is_empty() {
            return forms.iter().map(|f| self.expand(f)).collect();
        }
        if rest.is_empty() {
            return Err("a body needs at least one expression after its definitions".into());
        }
        let letrec = self.letrec(&bindings, rest)?;
        Ok(vec![self.expand(&letrec)?])
    }

    /// Expand head macros and splice `begin`s (at the top level or start of a body).
    pub fn flatten_into(&mut self, f: &Expr, out: &mut Vec<Expr>) -> Result<()> {
        let f = self.expand_head(f)?;
        if let Expr::List(items, _) = &f
            && items.first() == Some(&sym("begin"))
            && items.len() > 1
        {
            for child in &items[1..] {
                self.flatten_into(child, out)?;
            }
        } else {
            out.push(f);
        }
        Ok(())
    }

    fn expand_all(&mut self, items: &[Expr]) -> Result<Vec<Expr>> {
        items.iter().map(|i| self.expand(i)).collect()
    }

    fn expand_list(&mut self, items: &[Expr], line: Line) -> Result<Expr> {
        let Expr::Symbol(head) = &items[0] else {
            return Ok(Expr::List(self.expand_all(items)?, line));
        };
        let args = &items[1..];
        if self.is_macro(head) {
            let expanded = self.apply_macro(head, args, line)?;
            return self.expand(&expanded);
        }
        if let Some(derived) = self.derived(head, args)? {
            return self.expand(&with_line(derived, line));
        }
        match head.as_str() {
            "quote" => Ok(Expr::List(items.to_vec(), line)),
            "lambda" => {
                let [params, body @ ..] = args else {
                    return Err("lambda expects a parameter list and a body".into());
                };
                let parsed = parse_params(params)?;
                for p in parsed.all() {
                    self.check_not_macro(p)?;
                }
                if body.is_empty() {
                    return Err("lambda: empty body".into());
                }
                let mut out = vec![sym("lambda"), params.clone()];
                out.extend(self.expand_body(body)?);
                Ok(Expr::List(out, line))
            }
            "let" => {
                let [Expr::List(bindings, bl), body @ ..] = args else {
                    return Err("let expects a binding list and a body".into());
                };
                if body.is_empty() {
                    return Err("let: empty body".into());
                }
                let mut new_bindings = Vec::new();
                for b in bindings {
                    let (name, rhs) = parse_binding(b)?;
                    self.check_not_macro(name)?;
                    new_bindings.push(list(vec![sym(name), self.expand(rhs)?]));
                }
                let mut out = vec![sym("let"), Expr::List(new_bindings, *bl)];
                out.extend(self.expand_body(body)?);
                Ok(Expr::List(out, line))
            }
            "define" => {
                Err("define is only allowed at the top level or the start of a body".into())
            }
            "define-syntax" => Err("define-syntax is only supported at the top level".into()),
            "unquote" | "unquote-splicing" => Err(format!("{head} outside of quasiquote")),
            "else" | "=>" | "syntax-rules" => Err(format!("bad syntax: {head}")),
            _ => Ok(Expr::List(self.expand_all(items)?, line)),
        }
    }

    // ───────────────────────────── derived forms ─────────────────────────────

    /// Rewrite `(head args...)` one step if `head` is derived syntax.
    fn derived(&mut self, head: &str, args: &[Expr]) -> Result<Option<Expr>> {
        Ok(Some(match head {
            "cond" => cond(args)?,
            "case" => self.case(args)?,
            "let*" => let_star(args)?,
            "let" if matches!(args.first(), Some(Expr::Symbol(_))) => named_let(args)?,
            "letrec" | "letrec*" => {
                let [Expr::List(bindings, _), body @ ..] = args else {
                    return Err(format!("{head} expects a binding list and a body"));
                };
                self.letrec(bindings, body)?
            }
            "when" | "unless" => {
                let [test, body @ ..] = args else {
                    return Err(format!("{head} expects a test and a body"));
                };
                if body.is_empty() {
                    return Err(format!("{head}: empty body"));
                }
                if head == "when" {
                    list(vec![sym("if"), test.clone(), begin(body)])
                } else {
                    list(vec![sym("if"), test.clone(), void(), begin(body)])
                }
            }
            "do" => self.do_loop(args)?,
            "quasiquote" => {
                let [template] = args else {
                    return Err("quasiquote expects one template".into());
                };
                self.quasi(template, 1)?
            }
            _ => return Ok(None),
        }))
    }

    /// `(letrec ([x e]...) body...)` ==> `(let ([x (%void)]...) (set! x e)... (let () body...))`
    ///
    /// The `set!`s make every `x` a boxed variable, so closures created in the
    /// `e`s share the box and see the final value.
    fn letrec(&mut self, bindings: &[Expr], body: &[Expr]) -> Result<Expr> {
        let mut slots = Vec::new();
        let mut sets = Vec::new();
        for b in bindings {
            let (name, rhs) = parse_binding(b)?;
            slots.push(list(vec![sym(name), void()]));
            sets.push(list(vec![sym("set!"), sym(name), rhs.clone()]));
        }
        let mut inner = vec![sym("let"), list(vec![])];
        inner.extend_from_slice(body);
        let mut out = vec![sym("let"), list(slots)];
        out.extend(sets);
        out.push(list(inner));
        Ok(list(out))
    }

    /// `(case key [(d...) e...]... [else e...])` ==> `(let ([k key]) (cond [(or (eq? k 'd)...) e...]...))`
    fn case(&mut self, args: &[Expr]) -> Result<Expr> {
        let [key, clauses @ ..] = args else {
            return Err("case expects a key and clauses".into());
        };
        let k = self.gensym("case-key");
        let mut cond_clauses = vec![sym("cond")];
        for clause in clauses {
            let Expr::List(parts, _) = clause else {
                return Err("case clause must be a list".into());
            };
            let [data, body @ ..] = parts.as_slice() else {
                return Err("empty case clause".into());
            };
            if body.is_empty() {
                return Err("case clause needs a body".into());
            }
            let test = match data {
                Expr::Symbol(s) if s == "else" => sym("else"),
                Expr::List(data, _) => {
                    let mut or = vec![sym("or")];
                    or.extend(
                        data.iter()
                            .map(|d| list(vec![sym("%eq?"), sym(&k), quote(d.clone())])),
                    );
                    list(or)
                }
                _ => return Err("case clause must start with a list of data or else".into()),
            };
            let mut c = vec![test];
            c.extend_from_slice(body);
            cond_clauses.push(list(c));
        }
        Ok(list(vec![
            sym("let"),
            list(vec![list(vec![sym(&k), key.clone()])]),
            list(cond_clauses),
        ]))
    }

    /// `(do ([var init step]...) (test result...) body...)` ==> a named-let loop
    fn do_loop(&mut self, args: &[Expr]) -> Result<Expr> {
        let [Expr::List(specs, _), Expr::List(exit, _), body @ ..] = args else {
            return Err("do expects ([var init step]...) (test result...) body...".into());
        };
        let Some((test, results)) = exit.split_first() else {
            return Err("do needs an exit test".into());
        };
        let name = self.gensym("do-loop");
        let mut bindings = Vec::new();
        let mut steps = vec![sym(&name)];
        for spec in specs {
            let Expr::List(parts, _) = spec else {
                return Err("do variable must look like [var init step]".into());
            };
            let (var, init, step) = match parts.as_slice() {
                [Expr::Symbol(v), init] => (v, init, sym(v)),
                [Expr::Symbol(v), init, step] => (v, init, step.clone()),
                _ => return Err("do variable must look like [var init step]".into()),
            };
            bindings.push(list(vec![sym(var), init.clone()]));
            steps.push(step);
        }
        let mut done = vec![void()];
        done.extend_from_slice(results);
        let mut again = body.to_vec();
        again.push(list(steps));
        Ok(list(vec![
            sym("let"),
            sym(&name),
            list(bindings),
            list(vec![sym("if"), test.clone(), begin(&done), begin(&again)]),
        ]))
    }

    /// Quasiquote at nesting `depth`. Unquoted parts are spliced in with
    /// `%cons` / `%append2`; parts without unquotes stay quoted.
    fn quasi(&mut self, x: &Expr, depth: usize) -> Result<Expr> {
        if !has_unquote(x) {
            return Ok(quote(x.clone()));
        }
        match x {
            Expr::List(items, _) if items.len() == 2 && items[0] == sym("unquote") => {
                if depth == 1 {
                    Ok(items[1].clone())
                } else {
                    Ok(list(vec![
                        sym("%list"),
                        quote(sym("unquote")),
                        self.quasi(&items[1], depth - 1)?,
                    ]))
                }
            }
            Expr::List(items, _) if items.len() == 2 && items[0] == sym("quasiquote") => {
                Ok(list(vec![
                    sym("%list"),
                    quote(sym("quasiquote")),
                    self.quasi(&items[1], depth + 1)?,
                ]))
            }
            Expr::List(items, _) => {
                let n = items.len();
                let (elems, mut result) = if n >= 3 && items[n - 2] == sym(".") {
                    (&items[..n - 2], self.quasi(&items[n - 1], depth)?)
                } else {
                    (&items[..], quote(list(vec![])))
                };
                for item in elems.iter().rev() {
                    result = match item {
                        Expr::List(inner, _)
                            if depth == 1
                                && inner.len() == 2
                                && inner[0] == sym("unquote-splicing") =>
                        {
                            list(vec![sym("%append2"), inner[1].clone(), result])
                        }
                        _ => list(vec![sym("%cons"), self.quasi(item, depth)?, result]),
                    };
                }
                Ok(result)
            }
            Expr::Vector(items) => Ok(list(vec![
                sym("%list->vector"),
                self.quasi(&list(items.clone()), depth)?,
            ])),
            _ => Ok(quote(x.clone())),
        }
    }

    // ───────────────────────────── syntax-rules ─────────────────────────────

    /// Expand one use of macro `name`. Code from the template gets the use's line.
    fn apply_macro(&mut self, name: &str, args: &[Expr], line: Line) -> Result<Expr> {
        let m = &self.macros[name];
        let form = list(args.to_vec());
        for (pattern, template) in &m.rules {
            let mut b = Bindings::new();
            if match_pattern(pattern, &form, &m.literals, &mut b) {
                let template = template.clone();
                let mut binders = HashSet::new();
                template_binders(&template, &b, &mut binders);
                let mut cx = Instantiation {
                    b: &b,
                    binders: &binders,
                    renames: HashMap::new(),
                    line,
                };
                return self.instantiate(&template, &mut cx);
            }
        }
        Err(format!("no pattern of macro {name} matches this use"))
    }

    fn instantiate(&mut self, t: &Expr, cx: &mut Instantiation) -> Result<Expr> {
        let b = cx.b;
        match t {
            Expr::Symbol(s) => match b.get(s) {
                Some(Binding::One(e)) => Ok(e.clone()),
                Some(Binding::Many(_)) => Err(format!(
                    "pattern variable {s} must be followed by ... in the template"
                )),
                None => Ok(Expr::Symbol(self.rename(s, cx.binders, &mut cx.renames))),
            },
            Expr::List(items, _) => {
                let mut out = Vec::new();
                let mut i = 0;
                while i < items.len() {
                    if items.get(i + 1) == Some(&sym("...")) {
                        let sub = &items[i];
                        let mut vars = Vec::new();
                        symbols_in(sub, &mut vars);
                        let repeated: Vec<&String> = vars
                            .iter()
                            .filter(|v| matches!(b.get(*v), Some(Binding::Many(_))))
                            .collect();
                        let Some(first) = repeated.first() else {
                            return Err(
                                "... follows a template with no repeating pattern variables".into(),
                            );
                        };
                        let Binding::Many(ms) = &b[*first] else {
                            unreachable!()
                        };
                        let n = ms.len();
                        for v in &repeated {
                            let Binding::Many(ms) = &b[*v] else {
                                unreachable!()
                            };
                            if ms.len() != n {
                                return Err(format!(
                                    "pattern variables {first} and {v} repeat a different number of times"
                                ));
                            }
                        }
                        for j in 0..n {
                            let mut bj = b.clone();
                            for v in &repeated {
                                let Binding::Many(ms) = &b[*v] else {
                                    unreachable!()
                                };
                                bj.insert((*v).clone(), ms[j].clone());
                            }
                            let mut inner = Instantiation {
                                b: &bj,
                                binders: cx.binders,
                                renames: std::mem::take(&mut cx.renames),
                                line: cx.line,
                            };
                            let r = self.instantiate(sub, &mut inner);
                            cx.renames = inner.renames;
                            out.push(r?);
                        }
                        i += 2;
                    } else {
                        out.push(self.instantiate(&items[i], cx)?);
                        i += 1;
                    }
                }
                // a dotted tail that became a list is spliced in
                let n = out.len();
                if n >= 2
                    && out[n - 2] == sym(".")
                    && let Expr::List(tail, _) = &out[n - 1]
                {
                    let tail = tail.clone();
                    out.truncate(n - 2);
                    out.extend(tail);
                }
                Ok(Expr::List(out, cx.line))
            }
            Expr::Vector(items) => {
                let Expr::List(items, _) = self.instantiate(&list(items.clone()), cx)? else {
                    unreachable!()
                };
                Ok(Expr::Vector(items))
            }
            _ => Ok(t.clone()),
        }
    }

    /// Hygiene: names the template itself binds (like `tmp` in
    /// `(let ([tmp a]) ...)`) get a fresh name on every expansion; free
    /// names (globals like `for-each`) are left alone.
    fn rename(
        &mut self,
        s: &str,
        binders: &HashSet<String>,
        renames: &mut HashMap<String, String>,
    ) -> String {
        if !binders.contains(s) {
            return s.to_string();
        }
        if let Some(r) = renames.get(s) {
            return r.clone();
        }
        let r = self.gensym(s);
        renames.insert(s.to_string(), r.clone());
        r
    }
}

struct Instantiation<'a> {
    b: &'a Bindings,
    /// names the template binds itself, renamed for hygiene
    binders: &'a HashSet<String>,
    renames: HashMap<String, String>,
    /// the macro use's line, given to code that comes from the template
    line: Line,
}

/// Names a template binds with `let`/`lambda`/`do`/`define`... that aren't
/// pattern variables (those come from the user and keep their names).
fn template_binders(t: &Expr, b: &Bindings, out: &mut HashSet<String>) {
    let Expr::List(items, _) = t else { return };
    let mut add = |e: &Expr| {
        if let Expr::Symbol(s) = e
            && !b.contains_key(s)
            && ![".", "...", "_"].contains(&s.as_str())
        {
            out.insert(s.clone());
        }
    };
    let binding_names = |e: &Expr, add: &mut dyn FnMut(&Expr)| {
        if let Expr::List(bs, _) = e {
            for binding in bs {
                match binding {
                    Expr::List(parts, _) => {
                        if let Some(first) = parts.first() {
                            add(first)
                        }
                    }
                    other => add(other), // (lambda (a b) ...) parameter lists
                }
            }
        }
    };
    match items.first() {
        Some(Expr::Symbol(h)) if h == "quote" => return,
        Some(Expr::Symbol(h))
            if ["let", "let*", "letrec", "letrec*", "do"].contains(&h.as_str()) =>
        {
            match items.get(1) {
                Some(name @ Expr::Symbol(_)) => {
                    add(name); // named let
                    if let Some(bs) = items.get(2) {
                        binding_names(bs, &mut add);
                    }
                }
                Some(bs) => binding_names(bs, &mut add),
                None => {}
            }
        }
        Some(Expr::Symbol(h)) if h == "lambda" => match items.get(1) {
            Some(rest @ Expr::Symbol(_)) => add(rest),
            Some(ps) => binding_names(ps, &mut add),
            None => {}
        },
        Some(Expr::Symbol(h)) if h == "define" => match items.get(1) {
            Some(Expr::List(sig, _)) => sig.iter().for_each(&mut add),
            Some(name) => add(name),
            None => {}
        },
        _ => {}
    }
    items.iter().for_each(|i| template_binders(i, b, out));
}

fn def_value(def: Def) -> Expr {
    match def {
        Def::Function { params, body } => {
            let mut lambda = vec![sym("lambda"), params.to_expr()];
            lambda.extend(body);
            list(lambda)
        }
        Def::Value(e) => e,
    }
}

fn has_unquote(x: &Expr) -> bool {
    match x {
        Expr::List(items, _) => {
            items
                .first()
                .is_some_and(|h| *h == sym("unquote") || *h == sym("unquote-splicing"))
                || items.iter().any(has_unquote)
        }
        Expr::Vector(items) => items.iter().any(has_unquote),
        _ => false,
    }
}

fn symbols_in(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Symbol(s) if !out.contains(s) => out.push(s.clone()),
        Expr::List(items, _) | Expr::Vector(items) => items.iter().for_each(|i| symbols_in(i, out)),
        _ => {}
    }
}

fn pattern_vars(p: &Expr, literals: &[String], out: &mut Vec<String>) {
    match p {
        Expr::Symbol(s) if ![".", "...", "_"].contains(&s.as_str()) && !literals.contains(s) => {
            out.push(s.clone())
        }
        Expr::List(items, _) | Expr::Vector(items) => {
            items.iter().for_each(|i| pattern_vars(i, literals, out))
        }
        _ => {}
    }
}

fn match_pattern(pat: &Expr, form: &Expr, literals: &[String], b: &mut Bindings) -> bool {
    match pat {
        Expr::Symbol(s) if s == "_" => true,
        Expr::Symbol(s) if literals.contains(s) => form == pat,
        Expr::Symbol(s) => {
            b.insert(s.clone(), Binding::One(form.clone()));
            true
        }
        Expr::List(pats, _) => {
            let Expr::List(items, _) = form else {
                return false;
            };
            match_sequence(pats, items, literals, b)
        }
        Expr::Vector(pats) => {
            let Expr::Vector(items) = form else {
                return false;
            };
            match_sequence(pats, items, literals, b)
        }
        literal => literal == form,
    }
}

/// Match `(p1 p2 rep ... q1 . tail)` style patterns against a list's items.
fn match_sequence(pats: &[Expr], items: &[Expr], literals: &[String], b: &mut Bindings) -> bool {
    let n = pats.len();
    let (pats, tail) = if n >= 2 && pats[n - 2] == sym(".") {
        (&pats[..n - 2], Some(&pats[n - 1]))
    } else {
        (pats, None)
    };

    let Some(ell) = pats
        .iter()
        .position(|p| *p == sym("..."))
        .filter(|&i| i > 0)
    else {
        return match tail {
            None => {
                items.len() == pats.len()
                    && pats
                        .iter()
                        .zip(items)
                        .all(|(p, i)| match_pattern(p, i, literals, b))
            }
            Some(t) => {
                items.len() >= pats.len()
                    && pats
                        .iter()
                        .zip(items)
                        .all(|(p, i)| match_pattern(p, i, literals, b))
                    && match_pattern(t, &list(items[pats.len()..].to_vec()), literals, b)
            }
        };
    };
    if tail.is_some() {
        return false; // ... together with a dotted tail isn't supported
    }
    let (before, rep, after) = (&pats[..ell - 1], &pats[ell - 1], &pats[ell + 1..]);
    if items.len() < before.len() + after.len() {
        return false;
    }
    let reps = items.len() - before.len() - after.len();
    if !before
        .iter()
        .zip(items)
        .all(|(p, i)| match_pattern(p, i, literals, b))
    {
        return false;
    }
    let mut matches = Vec::new();
    for item in &items[before.len()..before.len() + reps] {
        let mut mb = Bindings::new();
        if !match_pattern(rep, item, literals, &mut mb) {
            return false;
        }
        matches.push(mb);
    }
    let mut vars = Vec::new();
    pattern_vars(rep, literals, &mut vars);
    for v in vars {
        let each = matches.iter().map(|m| m[&v].clone()).collect();
        b.insert(v, Binding::Many(each));
    }
    after
        .iter()
        .zip(&items[before.len() + reps..])
        .all(|(p, i)| match_pattern(p, i, literals, b))
}

/// `(cond [t1 e1...] [t2] [else e...])` ==> nested `if`s
fn cond(clauses: &[Expr]) -> Result<Expr> {
    let mut expanded = void();
    for clause in clauses.iter().rev() {
        let Expr::List(parts, _) = clause else {
            return Err("cond clause must be a list".into());
        };
        let [test, body @ ..] = parts.as_slice() else {
            return Err("empty cond clause".into());
        };
        if body.first() == Some(&sym("=>")) {
            return Err("cond clauses with => aren't supported".into());
        }
        expanded = if *test == sym("else") {
            if body.is_empty() {
                return Err("empty else clause".into());
            }
            begin(body)
        } else if body.is_empty() {
            list(vec![sym("or"), test.clone(), expanded]) // [test] yields test's value
        } else {
            list(vec![sym("if"), test.clone(), begin(body), expanded])
        };
    }
    Ok(expanded)
}

/// `(let* (a b c) body)` ==> `(let (a) (let (b) (let (c) body)))`
fn let_star(args: &[Expr]) -> Result<Expr> {
    let [Expr::List(bindings, _), body @ ..] = args else {
        return Err("let* expects a binding list and a body".into());
    };
    let mut expanded = vec![sym("let"), list(vec![])];
    expanded.extend_from_slice(body);
    let mut expanded = list(expanded);
    for b in bindings.iter().rev() {
        expanded = list(vec![sym("let"), list(vec![b.clone()]), expanded]);
    }
    Ok(expanded)
}

/// `(let loop ([v init]...) body...)` ==> `((letrec ([loop (lambda (v...) body...)]) loop) init...)`
fn named_let(args: &[Expr]) -> Result<Expr> {
    let [Expr::Symbol(name), Expr::List(bindings, _), body @ ..] = args else {
        return Err("named let expects a name, a binding list and a body".into());
    };
    let mut params = Vec::new();
    let mut inits = Vec::new();
    for b in bindings {
        let (p, init) = parse_binding(b)?;
        params.push(sym(p));
        inits.push(init.clone());
    }
    let mut lambda = vec![sym("lambda"), list(params)];
    lambda.extend_from_slice(body);
    let proc = list(vec![
        sym("letrec"),
        list(vec![list(vec![sym(name), list(lambda)])]),
        sym(name),
    ]);
    let mut call = vec![proc];
    call.extend(inits);
    Ok(list(call))
}
