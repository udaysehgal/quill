//! Definitions shared by the expander and the compiler: keywords, and
//! parsers for binding forms.

use crate::reader::{Expr, Line};

pub type Result<T> = std::result::Result<T, String>;

/// Most fixed parameters / literal arguments in one call (the limit of an
/// `[x, #imm]` offset into the argument buffer). `apply` can pass more.
pub const MAX_CALL_ARGS: usize = 4095;

/// Keywords. They can't be used as variable names, which keeps expanded
/// code from being captured by user bindings.
pub const SPECIAL_FORMS: &[&str] = &[
    // core forms the code generator handles
    "quote",
    "if",
    "and",
    "or",
    "begin",
    "let",
    "lambda",
    "set!",
    "%void",
    // derived forms, rewritten by the expander
    "define",
    "define-syntax",
    "syntax-rules",
    "cond",
    "case",
    "else",
    "=>",
    "let*",
    "letrec",
    "letrec*",
    "when",
    "unless",
    "do",
    "quasiquote",
    "unquote",
    "unquote-splicing",
];

pub fn sym(s: &str) -> Expr {
    Expr::Symbol(s.into())
}

pub fn list(items: Vec<Expr>) -> Expr {
    Expr::List(items, Line(0))
}

/// The line a form starts on, if known.
pub fn line_of(e: &Expr) -> u32 {
    match e {
        Expr::List(_, Line(n)) => *n,
        _ => 0,
    }
}

/// Prefix an error with a line number unless it already has one.
pub fn at_line(line: u32, e: String) -> String {
    if line == 0 || e.starts_with("line ") {
        e
    } else {
        format!("line {line}: {e}")
    }
}

pub fn check_bindable(name: &str) -> Result<()> {
    if SPECIAL_FORMS.contains(&name) {
        return Err(format!(
            "{name} is a keyword and can't be used as a variable name"
        ));
    }
    if name == "." {
        return Err("unexpected '.'".into());
    }
    Ok(())
}

pub fn parse_binding(b: &Expr) -> Result<(&str, &Expr)> {
    if let Expr::List(pair, _) = b
        && let [Expr::Symbol(name), rhs] = pair.as_slice()
    {
        check_bindable(name)?;
        return Ok((name, rhs));
    }
    Err("binding must look like [name expr]".into())
}

/// Parameters: `(a b)`, `(a b . rest)`, or a bare `rest` symbol (all arguments as a list).
pub struct Params {
    pub fixed: Vec<String>,
    pub rest: Option<String>,
}

impl Params {
    pub fn all(&self) -> impl Iterator<Item = &String> {
        self.fixed.iter().chain(&self.rest)
    }

    /// Back to syntax, for building a `lambda`.
    pub fn to_expr(&self) -> Expr {
        let mut items: Vec<Expr> = self.fixed.iter().map(|p| sym(p)).collect();
        match &self.rest {
            Some(r) if items.is_empty() => return sym(r),
            Some(r) => items.extend([sym("."), sym(r)]),
            None => {}
        }
        list(items)
    }
}

pub fn parse_params(e: &Expr) -> Result<Params> {
    let items = match e {
        Expr::Symbol(rest) => {
            check_bindable(rest)?;
            return Ok(Params {
                fixed: vec![],
                rest: Some(rest.clone()),
            });
        }
        Expr::List(items, _) => items,
        other => return Err(format!("bad parameter list {other:?}")),
    };
    let (fixed, rest) = match items.iter().position(|i| *i == sym(".")) {
        Some(dot) if dot + 2 == items.len() => (&items[..dot], Some(&items[dot + 1])),
        Some(_) => return Err("'.' must come just before the last parameter".into()),
        None => (&items[..], None),
    };
    let mut names: Vec<String> = Vec::new();
    for p in fixed.iter().chain(rest) {
        let Expr::Symbol(name) = p else {
            return Err(format!("parameter must be a name, got {p:?}"));
        };
        check_bindable(name)?;
        if names.contains(name) {
            return Err(format!("duplicate parameter {name}"));
        }
        names.push(name.clone());
    }
    let rest = rest.map(|_| names.pop().unwrap());
    if names.len() > MAX_CALL_ARGS {
        return Err(format!(
            "at most {MAX_CALL_ARGS} parameters are supported (use a rest parameter)"
        ));
    }
    Ok(Params { fixed: names, rest })
}

pub enum Def {
    Function { params: Params, body: Vec<Expr> },
    Value(Expr),
}

/// Recognize `(define (f params...) body...)` and `(define x expr)`.
pub fn parse_define(form: &Expr) -> Result<Option<(String, Def)>> {
    let Expr::List(items, line) = form else {
        return Ok(None);
    };
    if items.first() != Some(&sym("define")) {
        return Ok(None);
    }
    let located = |e: String| at_line(line.0, e);
    match &items[1..] {
        [Expr::List(sig, l), body @ ..] => {
            let Some((Expr::Symbol(name), params)) = sig.split_first() else {
                return Err(located("define needs a function name".into()));
            };
            check_bindable(name).map_err(located)?;
            let params = parse_params(&Expr::List(params.to_vec(), *l))
                .map_err(|e| located(format!("{name}: {e}")))?;
            if body.is_empty() {
                return Err(located(format!("{name}: empty body")));
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
            check_bindable(name).map_err(located)?;
            Ok(Some((name.clone(), Def::Value(value.clone()))))
        }
        _ => Err(located(
            "define must look like (define (name args...) body...) or (define name expr)".into(),
        )),
    }
}
