//! Small handwritten JSON parser used for .kicad_pro settings (including Unicode escapes).
use crate::{Error, MAX_FILE_BYTES, Result};
use std::collections::BTreeMap;
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(BTreeMap<String, Value>),
}
impl Value {
    pub fn get(&self, name: &str) -> Option<&Value> {
        if let Self::Object(map) = self {
            map.get(name)
        } else {
            None
        }
    }
    pub fn string(&self) -> Option<&str> {
        if let Self::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn array(&self) -> &[Value] {
        if let Self::Array(v) = self { v } else { &[] }
    }
    pub fn object(&self) -> Option<&BTreeMap<String, Value>> {
        if let Self::Object(map) = self {
            Some(map)
        } else {
            None
        }
    }
}
pub fn parse(source: &str) -> Result<Value> {
    if source.len() > MAX_FILE_BYTES {
        return Err(Error::new("Project file exceeds 32 MB"));
    }
    let mut p = Reader {
        source,
        index: 0,
        nodes: 0,
    };
    let value = p.value(0)?;
    p.space();
    if p.index != source.len() {
        return p.fail("Unexpected trailing JSON data");
    }
    Ok(value)
}
struct Reader<'a> {
    source: &'a str,
    index: usize,
    nodes: usize,
}
impl Reader<'_> {
    fn fail<T>(&self, msg: &str) -> Result<T> {
        Err(Error::at(self.source, self.index, msg))
    }
    fn peek(&self) -> Option<char> {
        self.source[self.index..].chars().next()
    }
    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.index += c.len_utf8();
        Some(c)
    }
    fn space(&mut self) {
        while self
            .peek()
            .is_some_and(|c| matches!(c, ' ' | '\n' | '\r' | '\t'))
        {
            self.next();
        }
    }
    fn expect(&mut self, c: char) -> Result<()> {
        self.space();
        if self.next() == Some(c) {
            Ok(())
        } else {
            self.fail("Unexpected JSON delimiter")
        }
    }
    fn string(&mut self) -> Result<String> {
        self.expect('"')?;
        let mut text = String::new();
        loop {
            match self.next() {
                Some('"') => return Ok(text),
                Some('\\') => match self.next() {
                    Some('"') => text.push('"'),
                    Some('\\') => text.push('\\'),
                    Some('/') => text.push('/'),
                    Some('b') => text.push('\u{8}'),
                    Some('f') => text.push('\u{c}'),
                    Some('n') => text.push('\n'),
                    Some('r') => text.push('\r'),
                    Some('t') => text.push('\t'),
                    Some('u') => {
                        let first = self.hex()?;
                        let value = if (0xd800..=0xdbff).contains(&first) {
                            if self.next() != Some('\\') || self.next() != Some('u') {
                                return self.fail("Missing low Unicode surrogate");
                            }
                            let second = self.hex()?;
                            if !(0xdc00..=0xdfff).contains(&second) {
                                return self.fail("Invalid low Unicode surrogate");
                            }
                            0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
                        } else {
                            first
                        };
                        text.push(char::from_u32(value).ok_or_else(|| {
                            Error::at(self.source, self.index, "Invalid Unicode code point")
                        })?);
                    }
                    _ => return self.fail("Invalid JSON string escape"),
                },
                Some(c) if c >= ' ' => text.push(c),
                _ => return self.fail("Unterminated or invalid JSON string"),
            }
        }
    }
    fn hex(&mut self) -> Result<u32> {
        let mut n = 0;
        for _ in 0..4 {
            n = n * 16
                + self
                    .next()
                    .and_then(|c| c.to_digit(16))
                    .ok_or_else(|| Error::at(self.source, self.index, "Invalid Unicode escape"))?;
        }
        Ok(n)
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        self.space();
        self.nodes += 1;
        if depth > 128 || self.nodes > 2_000_000 {
            return self.fail("JSON nesting or node limit exceeded");
        }
        match self.peek() {
            Some('{') => {
                self.next();
                let mut map = BTreeMap::new();
                self.space();
                if self.peek() == Some('}') {
                    self.next();
                    return Ok(Value::Object(map));
                }
                loop {
                    let key = self.string()?;
                    self.expect(':')?;
                    if map.insert(key, self.value(depth + 1)?).is_some() {
                        return self.fail("Duplicate JSON property");
                    }
                    self.space();
                    match self.next() {
                        Some('}') => break,
                        Some(',') => {}
                        _ => return self.fail("Expected comma or closing brace"),
                    }
                }
                Ok(Value::Object(map))
            }
            Some('[') => {
                self.next();
                let mut values = Vec::new();
                self.space();
                if self.peek() == Some(']') {
                    self.next();
                    return Ok(Value::Array(values));
                }
                loop {
                    values.push(self.value(depth + 1)?);
                    self.space();
                    match self.next() {
                        Some(']') => break,
                        Some(',') => {}
                        _ => return self.fail("Expected comma or closing bracket"),
                    }
                }
                Ok(Value::Array(values))
            }
            Some('"') => Ok(Value::String(self.string()?)),
            Some('n' | 't' | 'f') => {
                for (token, value) in [
                    ("null", Value::Null),
                    ("true", Value::Bool(true)),
                    ("false", Value::Bool(false)),
                ] {
                    if self.source[self.index..].starts_with(token) {
                        self.index += token.len();
                        return Ok(value);
                    }
                }
                self.fail("Invalid JSON literal")
            }
            Some('-' | '0'..='9') => {
                let start = self.index;
                if self.peek() == Some('-') {
                    self.next();
                }
                match self.next() {
                    Some('0') => {}
                    Some('1'..='9') => {
                        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                            self.next();
                        }
                    }
                    _ => return self.fail("Invalid JSON number"),
                }
                if self.peek() == Some('.') {
                    self.next();
                    let before = self.index;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.next();
                    }
                    if self.index == before {
                        return self.fail("Missing fraction digits");
                    }
                }
                if matches!(self.peek(), Some('e' | 'E')) {
                    self.next();
                    if matches!(self.peek(), Some('+' | '-')) {
                        self.next();
                    }
                    let before = self.index;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.next();
                    }
                    if self.index == before {
                        return self.fail("Missing exponent digits");
                    }
                }
                let n = self.source[start..self.index]
                    .parse::<f64>()
                    .map_err(|_| Error::at(self.source, start, "Invalid JSON number"))?;
                if !n.is_finite() {
                    return self.fail("JSON number is too large");
                }
                Ok(Value::Number(n))
            }
            _ => self.fail("Expected a JSON value"),
        }
    }
}
