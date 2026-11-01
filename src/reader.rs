//! Reader: source text -> `Expr` tree.

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Bool(bool),
    Char(char),
    Symbol(String),
    List(Vec<Expr>),
}

#[derive(Debug, PartialEq)]
enum Token {
    LParen,
    RParen,
    Quote,
    Atom(String),
}

fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            ';' => {
                // comment to end of line
                while chars.next().is_some_and(|c| c != '\n') {}
            }
            '(' | '[' => {
                chars.next();
                tokens.push(Token::LParen);
            }
            ')' | ']' => {
                chars.next();
                tokens.push(Token::RParen);
            }
            '\'' => {
                chars.next();
                tokens.push(Token::Quote);
            }
            _ => {
                let mut atom = String::new();
                // `#\(` and `#\ ` are char literals, so the char after `#\` is always taken
                if c == '#' {
                    atom.push(chars.next().unwrap());
                    if chars.peek() == Some(&'\\') {
                        atom.push(chars.next().unwrap());
                        atom.push(chars.next().ok_or("unterminated character literal")?);
                    }
                }
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || "()[];'".contains(c) {
                        break;
                    }
                    atom.push(c);
                    chars.next();
                }
                tokens.push(Token::Atom(atom));
            }
        }
    }
    Ok(tokens)
}

fn parse_atom(s: &str) -> Result<Expr, String> {
    if let Ok(n) = s.parse::<i64>() {
        return Ok(Expr::Int(n));
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
            _ => return Err(format!("unknown character literal #\\{name}")),
        };
        return Ok(Expr::Char(c));
    }
    if s.starts_with('#') {
        return Err(format!("unknown syntax {s}"));
    }
    Ok(Expr::Symbol(s.to_string()))
}

fn parse(tokens: &[Token], pos: &mut usize) -> Result<Expr, String> {
    let tok = tokens.get(*pos).ok_or("unexpected end of input")?;
    *pos += 1;
    match tok {
        Token::LParen => {
            let mut items = Vec::new();
            loop {
                match tokens.get(*pos) {
                    None => return Err("missing ')'".into()),
                    Some(Token::RParen) => break,
                    Some(_) => items.push(parse(tokens, pos)?),
                }
            }
            *pos += 1; // consume ')'
            Ok(Expr::List(items))
        }
        Token::RParen => Err("unexpected ')'".into()),
        Token::Quote => {
            let quoted = parse(tokens, pos)?;
            Ok(Expr::List(vec![Expr::Symbol("quote".into()), quoted]))
        }
        Token::Atom(s) => parse_atom(s),
    }
}

/// Read exactly one expression from `src`.
#[cfg(test)]
pub fn read(src: &str) -> Result<Expr, String> {
    let tokens = tokenize(src)?;
    let mut pos = 0;
    let expr = parse(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err("extra input after expression".into());
    }
    Ok(expr)
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

    #[test]
    fn reads_nested_lists() {
        assert_eq!(
            read("(add1 (add1 41))").unwrap(),
            Expr::List(vec![sym("add1"), Expr::List(vec![sym("add1"), Expr::Int(41)])])
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
    }

    #[test]
    fn reads_quote_and_comments() {
        assert_eq!(
            read("; hi\n'()").unwrap(),
            Expr::List(vec![sym("quote"), Expr::List(vec![])])
        );
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
