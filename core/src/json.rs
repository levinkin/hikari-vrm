const MAX_DEPTH: usize = 64;

pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

pub static NULL: Value = Value::Null;

impl Value {
    pub fn get(&self, key: &str) -> &Value {
        match self {
            Value::Object(entries) => entries.iter().find(|entry| entry.0 == key).map_or(&NULL, |entry| &entry.1),
            _ => &NULL,
        }
    }

    pub fn is_null(&self) -> bool {
        match self {
            Value::Null => true,
            _ => false,
        }
    }

    pub fn number(&self) -> Option<f64> {
        match self {
            Value::Number(number) => Some(*number),
            _ => None,
        }
    }

    pub fn boolean(&self) -> Option<bool> {
        match self {
            Value::Bool(flag) => Some(*flag),
            _ => None,
        }
    }

    pub fn string(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn array(&self) -> &[Value] {
        match self {
            Value::Array(items) => items,
            _ => &[],
        }
    }

    pub fn entries(&self) -> &[(String, Value)] {
        match self {
            Value::Object(entries) => entries,
            _ => &[],
        }
    }
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

pub fn parse(bytes: &[u8]) -> Result<Value, String> {
    let mut parser = Parser { bytes, position: 0 };
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.position != bytes.len() {
        return Err(parser.error("trailing data"));
    }
    Ok(value)
}

impl Parser<'_> {
    fn error(&self, message: &str) -> String {
        format!("json: {} at byte {}", message, self.position)
    }

    fn skip_whitespace(&mut self) {
        while self.position < self.bytes.len() && matches!(self.bytes[self.position], b' ' | b'\t' | b'\n' | b'\r') {
            self.position += 1;
        }
    }

    fn expect_literal(&mut self, literal: &[u8], value: Value) -> Result<Value, String> {
        if self.bytes[self.position..].starts_with(literal) {
            self.position += literal.len();
            Ok(value)
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth >= MAX_DEPTH {
            return Err(self.error("nesting deeper than 64"));
        }
        self.skip_whitespace();
        match self.bytes.get(self.position).copied() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.expect_literal(b"true", Value::Bool(true)),
            Some(b'f') => self.expect_literal(b"false", Value::Bool(false)),
            Some(b'n') => self.expect_literal(b"null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, String> {
        self.position += 1;
        let mut entries = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.position).copied() == Some(b'}') {
            self.position += 1;
            return Ok(Value::Object(entries));
        }
        loop {
            self.skip_whitespace();
            if self.bytes.get(self.position).copied() != Some(b'"') {
                return Err(self.error("expected object key"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            if self.bytes.get(self.position).copied() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.position += 1;
            let value = self.value(depth + 1)?;
            entries.push((key, value));
            self.skip_whitespace();
            match self.bytes.get(self.position).copied() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Value::Object(entries));
                }
                _ => return Err(self.error("expected ',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, String> {
        self.position += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.bytes.get(self.position).copied() == Some(b']') {
            self.position += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.bytes.get(self.position).copied() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.error("expected ',' or ']'")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.bytes.get(self.position..self.position + 4).ok_or_else(|| self.error("short \\u escape"))?;
        let mut code = 0u32;
        for digit in digits {
            let nibble = (*digit as char).to_digit(16).ok_or_else(|| self.error("bad \\u escape"))?;
            code = code * 16 + nibble;
        }
        self.position += 4;
        Ok(code)
    }

    fn string(&mut self) -> Result<String, String> {
        self.position += 1;
        let mut text = Vec::new();
        loop {
            let byte = self.bytes.get(self.position).copied().ok_or_else(|| self.error("unterminated string"))?;
            self.position += 1;
            match byte {
                b'"' => break,
                b'\\' => {
                    let escape = self.bytes.get(self.position).copied().ok_or_else(|| self.error("unterminated escape"))?;
                    self.position += 1;
                    let decoded = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let high = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&high) {
                                if !self.bytes[self.position..].starts_with(b"\\u") {
                                    return Err(self.error("lone high surrogate"));
                                }
                                self.position += 2;
                                let low = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&low) {
                                    return Err(self.error("bad low surrogate"));
                                }
                                0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
                            } else {
                                high
                            };
                            char::from_u32(code).ok_or_else(|| self.error("lone low surrogate"))?
                        }
                        _ => return Err(self.error("bad escape")),
                    };
                    let mut buffer = [0u8; 4];
                    text.extend_from_slice(decoded.encode_utf8(&mut buffer).as_bytes());
                }
                0..0x20 => return Err(self.error("control character in string")),
                _ => text.push(byte),
            }
        }
        String::from_utf8(text).map_err(|_| self.error("invalid utf-8 in string"))
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.position;
        if self.bytes.get(self.position).copied() == Some(b'-') {
            self.position += 1;
        }
        let integer_start = self.position;
        while matches!(self.bytes.get(self.position).copied(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        let integer_length = self.position - integer_start;
        if integer_length == 0 || (integer_length > 1 && self.bytes[integer_start] == b'0') {
            return Err(self.error("bad number"));
        }
        if self.bytes.get(self.position).copied() == Some(b'.') {
            self.position += 1;
            let fraction_start = self.position;
            while matches!(self.bytes.get(self.position).copied(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
            if self.position == fraction_start {
                return Err(self.error("bad number fraction"));
            }
        }
        if matches!(self.bytes.get(self.position).copied(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.bytes.get(self.position).copied(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            let exponent_start = self.position;
            while matches!(self.bytes.get(self.position).copied(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
            if self.position == exponent_start {
                return Err(self.error("bad number exponent"));
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position]).map_err(|_| self.error("bad number"))?;
        let number = text.parse::<f64>().map_err(|_| self.error("bad number"))?;
        if !(number as f32).is_finite() {
            return Err(self.error("number is out of the 32-bit float range"));
        }
        Ok(Value::Number(number))
    }
}
