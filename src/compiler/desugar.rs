//! Derived syntax, rewritten into core forms before compiling.
//!
//! Core forms the code generator knows: `quote if and or begin let lambda set! %void`.
//! Everything here expands into those (only special-form names and the
//! user's own variables appear in the output, so expansions can't be captured
//! by user bindings: special-form names can't be rebound).

use super::{Def, Result, list, parse_binding, parse_define, sym};
use crate::reader::Expr;

/// Expand `(head args...)` if `head` is derived syntax.
pub(super) fn derived(head: &str, args: &[Expr]) -> Result<Option<Expr>> {
    Ok(Some(match head {
        "cond" => cond(args)?,
        "let*" => let_star(args)?,
        "let" if matches!(args.first(), Some(Expr::Symbol(_))) => named_let(args)?,
        "letrec" | "letrec*" => {
            let [Expr::List(bindings), body @ ..] = args else {
                return Err(format!("{head} expects a binding list and a body"));
            };
            letrec(bindings, body)?
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
        _ => return Ok(None),
    }))
}

/// A body that starts with `define`s becomes a `letrec*`:
/// `((define (f x) ...) (define y 1) expr...)` ==> `(letrec* ([f (lambda (x) ...)] [y 1]) expr...)`
pub(super) fn internal_defines(body: &[Expr]) -> Result<Option<Expr>> {
    let mut bindings = Vec::new();
    let mut rest = body;
    while let Some((first, tail)) = rest.split_first() {
        let Some((name, def)) = parse_define(first)? else {
            break;
        };
        let value = match def {
            Def::Function { params, body } => {
                let mut lambda = vec![sym("lambda"), list(params.iter().map(|p| sym(p)).collect())];
                lambda.extend(body);
                list(lambda)
            }
            Def::Value(e) => e,
        };
        bindings.push(list(vec![sym(&name), value]));
        rest = tail;
    }
    if bindings.is_empty() {
        return Ok(None);
    }
    if rest.is_empty() {
        return Err("a body needs at least one expression after its definitions".into());
    }
    letrec(&bindings, rest).map(Some)
}

fn begin(body: &[Expr]) -> Expr {
    let mut v = vec![sym("begin")];
    v.extend_from_slice(body);
    list(v)
}

fn void() -> Expr {
    list(vec![sym("%void")])
}

/// `(cond [t1 e1...] [t2] [else e...])` ==> nested `if`s
fn cond(clauses: &[Expr]) -> Result<Expr> {
    let mut expanded = void();
    for clause in clauses.iter().rev() {
        let Expr::List(parts) = clause else {
            return Err("cond clause must be a list".into());
        };
        let [test, body @ ..] = parts.as_slice() else {
            return Err("empty cond clause".into());
        };
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
    let [Expr::List(bindings), body @ ..] = args else {
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
    let [Expr::Symbol(name), Expr::List(bindings), body @ ..] = args else {
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

/// `(letrec ([x e]...) body...)` ==> `(let ([x (%void)]...) (set! x e)... (let () body...))`
///
/// The `set!`s make every `x` a boxed variable, so closures created in the `e`s
/// share the box and see the final value.
fn letrec(bindings: &[Expr], body: &[Expr]) -> Result<Expr> {
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
