//! A bounded UTF-8 S-expression lexer/parser; unknown forms are retained in the AST.
use crate::{Error, MAX_FILE_BYTES, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    Atom(String),
    List(Vec<Expr>),
}
impl Expr {
    pub fn atom(&self) -> Option<&str> {
        if let Self::Atom(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn items(&self) -> &[Expr] {
        if let Self::List(v) = self { v } else { &[] }
    }
    pub fn tag(&self) -> Option<&str> {
        self.items().first()?.atom()
    }
    pub fn value(&self, index: usize) -> Option<&str> {
        self.items().get(index)?.atom()
    }
    pub fn child(&self, tag: &str) -> Option<&Expr> {
        self.items().iter().find(|e| e.tag() == Some(tag))
    }
    pub fn children<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a Expr> {
        self.items().iter().filter(move |e| e.tag() == Some(tag))
    }
    pub fn has_atom(&self, value: &str) -> bool {
        self.items().iter().any(|e| e.atom() == Some(value))
    }
    pub fn number(&self, index: usize) -> Option<f64> {
        self.value(index)?
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite() && n.abs() <= 1e8)
    }
}
pub fn parse(source: &str) -> Result<Expr> {
    if source.len() > MAX_FILE_BYTES {
        return Err(Error::new("Schematic exceeds 32 MB"));
    }
    let mut p = Reader {
        source,
        index: 0,
        nodes: 0,
    };
    p.space();
    let expr = p.expr(0)?;
    p.space();
    if p.index != source.len() {
        return p.fail("Unexpected data after the root expression");
    }
    Ok(expr)
}
struct Reader<'a> {
    source: &'a str,
    index: usize,
    nodes: usize,
}
impl Reader<'_> {
    fn fail<T>(&self, message: &str) -> Result<T> {
        Err(Error::at(self.source, self.index, message))
    }
    fn next(&mut self) -> Option<char> {
        let ch = self.source[self.index..].chars().next()?;
        self.index += ch.len_utf8();
        Some(ch)
    }
    fn peek(&self) -> Option<char> {
        self.source[self.index..].chars().next()
    }
    fn space(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.next();
            }
            if self.peek() == Some(';') {
                while self.next().is_some_and(|c| c != '\n') {}
            } else {
                break;
            }
        }
    }
    fn expr(&mut self, depth: usize) -> Result<Expr> {
        self.nodes += 1;
        if depth > 128 || self.nodes > 2_000_000 {
            return self.fail("S-expression nesting or node limit exceeded");
        }
        self.space();
        match self.next() {
            Some('(') => {
                let mut values = Vec::new();
                loop {
                    self.space();
                    match self.peek() {
                        Some(')') => {
                            self.next();
                            break;
                        }
                        None => return self.fail("Unterminated list"),
                        _ => values.push(self.expr(depth + 1)?),
                    }
                }
                Ok(Expr::List(values))
            }
            Some('"') => {
                let mut value = String::new();
                loop {
                    match self.next() {
                        Some('"') => break,
                        Some('\\') => match self.next() {
                            Some('n') => value.push('\n'),
                            Some('r') => value.push('\r'),
                            Some('t') => value.push('\t'),
                            Some('"') => value.push('"'),
                            Some('\\') => value.push('\\'),
                            Some(c) => {
                                value.push('\\');
                                value.push(c)
                            }
                            None => return self.fail("Unterminated string escape"),
                        },
                        Some(c) => value.push(c),
                        None => return self.fail("Unterminated quoted string"),
                    }
                }
                Ok(Expr::Atom(value))
            }
            Some(')') => self.fail("Unexpected closing parenthesis"),
            Some(c) => {
                let mut atom = String::from(c);
                while let Some(c) = self.peek() {
                    if c.is_whitespace() || matches!(c, '(' | ')' | ';') {
                        break;
                    }
                    atom.push(self.next().unwrap());
                }
                Ok(Expr::Atom(atom))
            }
            None => self.fail("Expected an expression"),
        }
    }
}
