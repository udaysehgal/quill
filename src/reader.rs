//! Reader: source text -> `Expr` tree.
//!
//! Lists remember the line they start on, for error messages. A dotted pair
//! `(a . b)` reads as a list containing the symbol `.`; the compiler
//! interprets it where it's allowed (quoted data, parameter lists, patterns).

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Bool(bool),
    Char(char),
    Str(String),
    Symbol(String),
    List(Vec<Expr>, Line),
    /// `#(...)` literal (self-evaluating, elements are data)
    Vector(Vec<Expr>),
}

/// A source line number (0 = unknown, e.g. code made by the compiler).
/// Always compares equal so it never affects comparisons between `Expr`s.
#[derive(Debug, Clone, Copy, Default)]
pub struct Line(pub u32);

impl PartialEq for Line {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    LParen,
    VectorOpen,
    RParen,
    /// `'` `` ` `` `,` `,@`, each wrapping the next datum in this form
    Prefix(&'static str),
    Str(String),
    Atom(String),
}

fn tokenize(src: &str) -> Result<Vec<(Token, u32)>, String> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    let mut line = 1;
    while let Some(&c) = chars.peek() {
        let start = line;
        match c {
            '\n' => {
                line += 1;
                chars.next();
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            ';' => {
                // comment to end of line (the newline itself is counted above)
                while chars.peek().is_some_and(|&c| c != '\n') {
                    chars.next();
                }
            }
            '(' | '[' => {
                chars.next();
                tokens.push((Token::LParen, start));
            }
            ')' | ']' => {
                chars.next();
                tokens.push((Token::RParen, start));
            }
            '\'' | '`' => {
                chars.next();
                tokens.push((
                    Token::Prefix(if c == '\'' { "quote" } else { "quasiquote" }),
                    start,
                ));
            }
            ',' => {
                chars.next();
                if chars.peek() == Some(&'@') {
                    chars.next();
                    tokens.push((Token::Prefix("unquote-splicing"), start));
                } else {
                    tokens.push((Token::Prefix("unquote"), start));
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    let err = || format!("line {start}: unterminated string");
                    match chars.next().ok_or_else(err)? {
                        '"' => break,
                        '\\' => s.push(match chars.next().ok_or_else(err)? {
                            'n' => '\n',
                            't' => '\t',
                            '0' => '\0',
                            c @ ('"' | '\\') => c,
                            c => return Err(format!("line {line}: unknown string escape \\{c}")),
                        }),
                        c => {
                            if c == '\n' {
                                line += 1;
                            }
                            s.push(c)
                        }
                    }
                }
                tokens.push((Token::Str(s), start));
            }
            _ => {
                let mut atom = String::new();
                if c == '#' {
                    atom.push(chars.next().unwrap());
                    match chars.peek() {
                        Some('(') => {
                            chars.next();
                            tokens.push((Token::VectorOpen, start));
                            continue;
                        }
                        // `#\(` and `#\ ` are char literals, so the char after `#\` is always taken
                        Some('\\') => {
                            atom.push(chars.next().unwrap());
                            atom.push(chars.next().ok_or_else(|| {
                                format!("line {start}: unterminated character literal")
                            })?);
                        }
                        _ => {}
                    }
                }
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || "()[];'`,\"".contains(c) {
                        break;
                    }
                    atom.push(c);
                    chars.next();
                }
                tokens.push((Token::Atom(atom), start));
            }
        }
    }
    Ok(tokens)
}

fn parse_atom(s: &str, line: u32) -> Result<Expr, String> {
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Expr::Int(n));
    }
    if s.len() > 1
        && s.trim_start_matches(['-', '+'])
            .chars()
            .all(|c| c.is_ascii_digit())
    {
        return Err(format!("line {line}: integer {s} is too large"));
    }
    match s {
        "#t" | "#true" => return Ok(Expr::Bool(true)),
        "#f" | "#false" => return Ok(Expr::Bool(false)),
        _ => {}
    }
    if let Some(name) = s.strip_prefix("#\\") {
        let c = match name {
            "space" => ' ',
            "newline" => '\n',
            "tab" => '\t',
            "nul" => '\0',
            _ if name.chars().count() == 1 => name.chars().next().unwrap(),
            _ => return Err(format!("line {line}: unknown character literal #\\{name}")),
        };
        return Ok(Expr::Char(c));
    }
    if s.starts_with('#') {
        return Err(format!("line {line}: unknown syntax {s}"));
    }
    Ok(Expr::Symbol(s.to_string()))
}

fn parse(tokens: &[(Token, u32)], pos: &mut usize) -> Result<Expr, String> {
    let Some((tok, line)) = tokens.get(*pos) else {
        return Err("unexpected end of input".into());
    };
    let line = *line;
    *pos += 1;
    match tok {
        Token::LParen | Token::VectorOpen => {
            let mut items = Vec::new();
            loop {
                match tokens.get(*pos) {
                    None => return Err(format!("line {line}: missing ')' for the '(' here")),
                    Some((Token::RParen, _)) => break,
                    Some(_) => items.push(parse(tokens, pos)?),
                }
            }
            *pos += 1; // consume ')'
            if *tok == Token::VectorOpen {
                Ok(Expr::Vector(items))
            } else {
                Ok(Expr::List(items, Line(line)))
            }
        }
        Token::RParen => Err(format!("line {line}: unexpected ')'")),
        Token::Prefix(form) => {
            let datum = parse(tokens, pos).map_err(|e| {
                if e.starts_with("line ") {
                    e
                } else {
                    format!("line {line}: {e} after {form}")
                }
            })?;
            Ok(Expr::List(
                vec![Expr::Symbol(form.to_string()), datum],
                Line(line),
            ))
        }
        Token::Str(s) => Ok(Expr::Str(s.clone())),
        Token::Atom(s) => parse_atom(s, line),
    }
}

/// Read exactly one expression from `src`.
#[cfg(test)]
pub fn read(src: &str) -> Result<Expr, String> {
    let exprs = read_all(src)?;
    match exprs.len() {
        1 => Ok(exprs.into_iter().next().unwrap()),
        0 => Err("unexpected end of input".into()),
        _ => Err("extra input after expression".into()),
    }
}

/// Read every expression in `src` (a whole program).
pub fn read_all(src: &str) -> Result<Vec<Expr>, String> {
    let tokens = tokenize(src)?;
    let mut pos = 0;
    let mut exprs = Vec::new();
    while pos < tokens.len() {
        exprs.push(parse(&tokens, &mut pos)?);
    }
    Ok(exprs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sym(s: &str) -> Expr {
        Expr::Symbol(s.into())
    }

    fn list(items: Vec<Expr>) -> Expr {
        Expr::List(items, Line(0))
    }

    #[test]
    fn reads_nested_lists() {
        assert_eq!(
            read("(add1 (add1 41))").unwrap(),
            list(vec![sym("add1"), list(vec![sym("add1"), Expr::Int(41)])])
        );
    }

    #[test]
    fn reads_literals() {
        assert_eq!(read("#t").unwrap(), Expr::Bool(true));
        assert_eq!(read("#f").unwrap(), Expr::Bool(false));
        assert_eq!(read("#\\a").unwrap(), Expr::Char('a'));
        assert_eq!(read("#\\(").unwrap(), Expr::Char('('));
        assert_eq!(read("#\\space").unwrap(), Expr::Char(' '));
        assert_eq!(read("-12").unwrap(), Expr::Int(-12));
        assert_eq!(
            read("#(1 a)").unwrap(),
            Expr::Vector(vec![Expr::Int(1), sym("a")])
        );
        assert!(
            read("99999999999999999999")
                .unwrap_err()
                .contains("too large")
        );
    }

    #[test]
    fn reads_strings() {
        assert_eq!(read(r#""hi there""#).unwrap(), Expr::Str("hi there".into()));
        assert_eq!(
            read(r#""a\"b\\c\n""#).unwrap(),
            Expr::Str("a\"b\\c\n".into())
        );
        assert_eq!(
            read(r#""(not a list)""#).unwrap(),
            Expr::Str("(not a list)".into())
        );
        assert!(read(r#""open"#).is_err());
    }

    #[test]
    fn reads_quote_forms_and_comments() {
        assert_eq!(
            read("; hi\n'()").unwrap(),
            list(vec![sym("quote"), list(vec![])])
        );
        assert_eq!(
            read("`(a ,b ,@c)").unwrap(),
            list(vec![
                sym("quasiquote"),
                list(vec![
                    sym("a"),
                    list(vec![sym("unquote"), sym("b")]),
                    list(vec![sym("unquote-splicing"), sym("c")]),
                ])
            ])
        );
        assert_eq!(
            read("(a . b)").unwrap(),
            list(vec![sym("a"), sym("."), sym("b")])
        );
    }

    #[test]
    fn tracks_lines() {
        let forms = read_all("1\n\n(f\n (g))").unwrap();
        let Expr::List(items, Line(3)) = &forms[1] else {
            panic!("expected line 3: {forms:?}")
        };
        assert!(matches!(items[1], Expr::List(_, Line(4))));
        assert!(read_all("(a\n(b)").unwrap_err().starts_with("line 1:"));
        assert!(read_all("\"a\nb\" )").unwrap_err().starts_with("line 2:"));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(read("(1 2").is_err());
        assert!(read(")").is_err());
        assert!(read("1 2").is_err());
    }

    #[test]
    fn reads_whole_program() {
        let forms = read_all("(define (f x) x) ; comment\n(f 1)").unwrap();
        assert_eq!(forms.len(), 2);
        assert!(read_all("(f 1))").is_err());
    }
}
