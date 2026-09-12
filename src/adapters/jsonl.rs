use serde::de::IgnoredAny;
use std::io::{self, BufRead};
use std::str;

pub(super) struct JsonlReader<'a> {
    input: &'a mut dyn BufRead,
    buffer: Vec<u8>,
    line: usize,
}

pub(super) enum JsonlLine<'a> {
    Complete { number: usize, text: &'a str },
    Incomplete { number: usize },
    Eof,
}

#[derive(Debug)]
pub(super) enum JsonlError {
    MalformedLine { line: usize },
    InvalidUtf8 { line: usize },
    Io { line: usize, source: io::Error },
}

impl<'a> JsonlReader<'a> {
    pub(super) fn new(input: &'a mut dyn BufRead) -> Self {
        Self {
            input,
            buffer: Vec::new(),
            line: 0,
        }
    }

    pub(super) fn next_line(&mut self) -> Result<JsonlLine<'_>, JsonlError> {
        self.buffer.clear();
        self.line += 1;
        if self
            .input
            .read_until(b'\n', &mut self.buffer)
            .map_err(|source| JsonlError::Io {
                line: self.line,
                source,
            })?
            == 0
        {
            return Ok(JsonlLine::Eof);
        }
        Ok(match complete_line(&self.buffer, self.line)? {
            Some(text) => JsonlLine::Complete {
                number: self.line,
                text,
            },
            None => JsonlLine::Incomplete { number: self.line },
        })
    }
}

fn complete_line(bytes: &[u8], line: usize) -> Result<Option<&str>, JsonlError> {
    let terminated = bytes.ends_with(b"\n");
    let text = match str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if !terminated && error.error_len().is_none() => {
            return match serde_json::from_slice::<IgnoredAny>(bytes) {
                Err(error) if error.is_eof() && valid_unicode_escape_prefixes(bytes) => Ok(None),
                _ => Err(JsonlError::MalformedLine { line }),
            };
        }
        Err(_) => return Err(JsonlError::InvalidUtf8 { line }),
    };

    // Check syntax independently of field types, including ignored content.
    match serde_json::from_str::<IgnoredAny>(text) {
        Ok(_) => Ok(Some(text)),
        Err(error) if !terminated && error.is_eof() && valid_unicode_escape_prefixes(bytes) => {
            Ok(None)
        }
        Err(_) if !terminated && text.ends_with(['.', 'e', 'E', '+', '-']) => {
            // Appending a digit distinguishes a truncated number from invalid syntax.
            match serde_json::from_str::<IgnoredAny>(&format!("{text}0")) {
                Ok(_) => Ok(None),
                Err(error) if error.is_eof() && valid_unicode_escape_prefixes(bytes) => Ok(None),
                Err(_) => Err(JsonlError::MalformedLine { line }),
            }
        }
        Err(_) => Err(JsonlError::MalformedLine { line }),
    }
}

fn valid_unicode_escape_prefixes(bytes: &[u8]) -> bool {
    // serde_json reports EOF before validating partial Unicode escapes.
    let mut bytes = bytes.iter();
    let mut in_string = false;
    while let Some(&byte) = bytes.next() {
        if byte == b'"' {
            in_string = !in_string;
        } else if in_string
            && byte == b'\\'
            && bytes.next() == Some(&b'u')
            && !bytes.by_ref().take(4).all(u8::is_ascii_hexdigit)
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_unterminated_valid_json_prefixes_are_retryable() {
        for tail in [
            "{",
            r#"{"text":"unfinished"#,
            r#"{"items":[1,"#,
            r#"{"n":-"#,
            r#"{"n":1."#,
            r#"{"n":1e+"#,
            r#"{"text":"\u0a"#,
            r#"{"text":"\\uX"#,
        ] {
            assert_eq!(complete_line(tail.as_bytes(), 7).unwrap(), None, "{tail}");
            assert!(
                matches!(
                    complete_line(format!("{tail}\n").as_bytes(), 7),
                    Err(JsonlError::MalformedLine { line: 7 })
                ),
                "{tail}"
            );
        }
        for tail in [
            r#"{"n":1e++"#,
            r#"{"n":01."#,
            r#"{"items":[1 2."#,
            r#"{"text":"\uX"#,
            r#"{"text":"\u00X"#,
            "{} trailing",
        ] {
            assert!(
                matches!(
                    complete_line(tail.as_bytes(), 7),
                    Err(JsonlError::MalformedLine { line: 7 })
                ),
                "{tail}"
            );
        }
        for ending in ["", "\n", "\r\n"] {
            let line = format!("{{}}{ending}");
            assert_eq!(
                complete_line(line.as_bytes(), 7).unwrap(),
                Some(line.as_str())
            );
        }
    }

    #[test]
    fn split_utf8_is_retryable_only_inside_a_valid_final_string() {
        let mut bytes = b"{\"text\":\"".to_vec();
        bytes.extend_from_slice(&[0xf0, 0x9f]);
        assert_eq!(complete_line(&bytes, 2).unwrap(), None);
        bytes.push(b'\n');
        assert!(matches!(
            complete_line(&bytes, 2),
            Err(JsonlError::InvalidUtf8 { line: 2 })
        ));
        for prefix in ["{}", r#"{"text": "#, r#"{"text":"\u"#] {
            let mut bytes = prefix.as_bytes().to_vec();
            bytes.extend_from_slice(&[0xf0, 0x9f]);
            assert!(
                matches!(
                    complete_line(&bytes, 2),
                    Err(JsonlError::MalformedLine { line: 2 })
                ),
                "{prefix}"
            );
        }
        assert!(matches!(
            complete_line(b"\xff", 2),
            Err(JsonlError::InvalidUtf8 { line: 2 })
        ));
    }
}
