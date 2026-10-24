//! Structured fields: callers pass a FLAT JSON object
//! (`{"host":"example.com","attempt":2,"ok":true}`) and records
//! render it as `host=example.com attempt=2 ok=true`.
//!
//! Flat only, by design: a log line is not a document store.
//! Nested objects/arrays are rejected with LK_ERR_PARSE so the
//! mistake is loud at the call site, not silent in the file.

use crate::{LkResult, ERR_PARSE};

#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Str(String),
    I64(i64),
    F64(f64),
    Bool(bool),
    Null,
}

fn perr<T>(msg: impl Into<String>) -> LkResult<T> {
    Err((ERR_PARSE, msg.into()))
}

/// Parse a flat JSON object into ordered fields. An empty or
/// whitespace-only string yields no fields. Duplicate keys are
/// rejected (same ambiguity rule as SettingsKit files).
pub fn parse_fields(text: &str) -> LkResult<Vec<(String, FieldValue)>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut p = P {
        b: trimmed.as_bytes(),
        i: 0,
    };
    p.ws();
    p.expect(b'{')?;
    let mut out: Vec<(String, FieldValue)> = Vec::new();
    p.ws();
    if p.peek() == Some(b'}') {
        p.i += 1;
        p.ws();
        p.end()?;
        return Ok(out);
    }
    loop {
        p.ws();
        let key = p.string()?;
        if out.iter().any(|(k, _)| k == &key) {
            return perr(format!("duplicate field key \"{key}\""));
        }
        p.ws();
        p.expect(b':')?;
        p.ws();
        let value = p.value()?;
        out.push((key, value));
        p.ws();
        match p.peek() {
            Some(b',') => p.i += 1,
            Some(b'}') => {
                p.i += 1;
                break;
            }
            _ => return perr(format!("expected ',' or '}}' at byte {}", p.i)),
        }
    }
    p.ws();
    p.end()?;
    Ok(out)
}

/// Render one field value for a log line: bare when it needs no
/// protection, double-quoted with escapes otherwise. A string is
/// bare when it is non-empty and contains no space, quote,
/// backslash, `=`, or control character — so `host=example.com`
/// stays clean and `msg="a b"` stays parseable.
pub fn render_value(v: &FieldValue) -> String {
    match v {
        FieldValue::Str(s) => {
            if bare_safe(s) {
                s.clone()
            } else {
                let mut out = String::with_capacity(s.len() + 2);
                out.push('"');
                for c in s.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                        c => out.push(c),
                    }
                }
                out.push('"');
                out
            }
        }
        FieldValue::I64(n) => n.to_string(),
        FieldValue::F64(f) => {
            if f == &f.trunc() && f.abs() < 1e15 {
                format!("{f:.1}")
            } else {
                format!("{f}")
            }
        }
        FieldValue::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        FieldValue::Null => "null".to_string(),
    }
}

fn bare_safe(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| !c.is_whitespace() && c != '"' && c != '\\' && c != '=' && (c as u32) >= 0x20)
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn end(&self) -> LkResult<()> {
        if self.i == self.b.len() {
            Ok(())
        } else {
            perr(format!("trailing characters at byte {}", self.i))
        }
    }

    fn expect(&mut self, byte: u8) -> LkResult<()> {
        if self.peek() == Some(byte) {
            self.i += 1;
            Ok(())
        } else {
            perr(format!("expected '{}' at byte {}", byte as char, self.i))
        }
    }

    fn string(&mut self) -> LkResult<String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self
                .peek()
                .ok_or((ERR_PARSE, "unterminated string".to_string()))?;
            self.i += 1;
            match byte {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self
                        .peek()
                        .ok_or((ERR_PARSE, "unterminated escape".to_string()))?;
                    self.i += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) {
                                if self.peek() == Some(b'\\') {
                                    self.i += 1;
                                    self.expect(b'u')?;
                                    let lo = self.hex4()?;
                                    if !(0xDC00..0xE000).contains(&lo) {
                                        return perr("invalid low surrogate");
                                    }
                                    let c = 0x1_0000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                    out.push(char::from_u32(c).ok_or((
                                        ERR_PARSE,
                                        "invalid unicode escape".to_string(),
                                    ))?);
                                } else {
                                    return perr("unpaired high surrogate");
                                }
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return perr("unpaired low surrogate");
                            } else {
                                out.push(
                                    char::from_u32(hi)
                                        .ok_or((ERR_PARSE, "invalid unicode escape".to_string()))?,
                                );
                            }
                        }
                        _ => return perr(format!("invalid escape at byte {}", self.i - 1)),
                    }
                }
                0x00..=0x1F => {
                    return perr(format!(
                        "unescaped control character at byte {}",
                        self.i - 1
                    ))
                }
                _ => {
                    let start = self.i - 1;
                    let mut end = self.i;
                    while end < self.b.len() && (self.b[end] & 0xC0) == 0x80 {
                        end += 1;
                    }
                    out.push_str(
                        std::str::from_utf8(&self.b[start..end])
                            .map_err(|_| (ERR_PARSE, "invalid UTF-8".to_string()))?,
                    );
                    self.i = end;
                }
            }
        }
    }

    fn hex4(&mut self) -> LkResult<u32> {
        let mut v: u32 = 0;
        for _ in 0..4 {
            let byte = self
                .peek()
                .ok_or((ERR_PARSE, "truncated \\u escape".to_string()))?;
            self.i += 1;
            let digit = (byte as char)
                .to_digit(16)
                .ok_or((ERR_PARSE, "invalid hex in \\u escape".to_string()))?;
            v = v * 16 + digit;
        }
        Ok(v)
    }

    fn value(&mut self) -> LkResult<FieldValue> {
        match self.peek() {
            Some(b'"') => Ok(FieldValue::Str(self.string()?)),
            Some(b't') => self.literal("true", FieldValue::Bool(true)),
            Some(b'f') => self.literal("false", FieldValue::Bool(false)),
            Some(b'n') => self.literal("null", FieldValue::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b'{' | b'[') => {
                perr("nested objects/arrays are not allowed in log fields (flat object only)")
            }
            _ => perr(format!("unexpected character at byte {}", self.i)),
        }
    }

    fn literal(&mut self, word: &str, v: FieldValue) -> LkResult<FieldValue> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            perr(format!("invalid literal at byte {}", self.i))
        }
    }

    fn number(&mut self) -> LkResult<FieldValue> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        match self.peek() {
            Some(b'0') => self.i += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.i += 1;
                }
            }
            _ => return perr(format!("invalid number at byte {start}")),
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.i += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return perr(format!("invalid fraction at byte {}", self.i));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return perr(format!("invalid exponent at byte {}", self.i));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.i += 1;
            }
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).unwrap();
        if !is_float {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(FieldValue::I64(n));
            }
        }
        let f: f64 = text
            .parse()
            .map_err(|_| (ERR_PARSE, format!("invalid number '{text}'")))?;
        if !f.is_finite() {
            return perr(format!("number out of range '{text}'"));
        }
        Ok(FieldValue::F64(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flat_object() {
        let f =
            parse_fields(r#"{"host":"example.com","attempt":2,"ok":true,"ratio":0.5,"note":null}"#)
                .unwrap();
        assert_eq!(f.len(), 5);
        assert_eq!(
            f[0],
            ("host".to_string(), FieldValue::Str("example.com".into()))
        );
        assert_eq!(f[1].1, FieldValue::I64(2));
        assert_eq!(f[2].1, FieldValue::Bool(true));
        assert_eq!(f[3].1, FieldValue::F64(0.5));
        assert_eq!(f[4].1, FieldValue::Null);
        assert!(parse_fields("").unwrap().is_empty());
        assert!(parse_fields("  {} ").unwrap().is_empty());
    }

    #[test]
    fn rejects_nested_and_duplicates() {
        assert_eq!(parse_fields(r#"{"a":{"b":1}}"#).unwrap_err().0, ERR_PARSE);
        assert_eq!(parse_fields(r#"{"a":[1]}"#).unwrap_err().0, ERR_PARSE);
        assert_eq!(parse_fields(r#"{"a":1,"a":2}"#).unwrap_err().0, ERR_PARSE);
        assert_eq!(parse_fields("not json").unwrap_err().0, ERR_PARSE);
        assert_eq!(parse_fields(r#"{"a":}"#).unwrap_err().0, ERR_PARSE);
    }

    #[test]
    fn renders_values() {
        assert_eq!(
            render_value(&FieldValue::Str("example.com".into())),
            "example.com"
        );
        assert_eq!(render_value(&FieldValue::Str("a b".into())), "\"a b\"");
        assert_eq!(
            render_value(&FieldValue::Str("say \"hi\"\n".into())),
            "\"say \\\"hi\\\"\\n\""
        );
        assert_eq!(render_value(&FieldValue::Str(String::new())), "\"\"");
        assert_eq!(render_value(&FieldValue::I64(-3)), "-3");
        assert_eq!(render_value(&FieldValue::F64(2.0)), "2.0");
        assert_eq!(render_value(&FieldValue::Bool(false)), "false");
        assert_eq!(render_value(&FieldValue::Null), "null");
    }
}
