//! Hand-rolled tokenizer for the Cypher subset. Small enough that a char-scan
//! beats pulling in a lexer-generator dependency.

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // punctuation / structure
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Colon,
    Comma,
    Dot,
    Pipe,
    Star, // * (count(*))
    // path arrows
    ArrowRight, // ->
    ArrowLeft,  // <-
    Dash,       // - (undirected segment / part of arrow)
    // operators
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    RegexMatch, // =~ (regex predicate, Neo4j-compatible)
    // keywords (case-insensitive) and identifiers
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
}

pub fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Read the char at byte i WITHOUT a lossy `bytes[i] as char` cast: a
        // multibyte UTF-8 lead byte (e.g. CJK '日' = 0xE6) would otherwise cast
        // to a Latin-1 char that `is_alphabetic()` accepts, push the scanner
        // into the identifier branch, and slice on a non-char boundary → panic.
        // `chars().next()` yields the true Unicode scalar; `len_utf8()` its byte
        // width, so the multibyte fallthrough advances by whole chars.
        let c = input[i..].chars().next().unwrap();
        match c {
            c if c.is_whitespace() => i += c.len_utf8(),
            '(' => {
                out.push(Token::LParen);
                i += 1;
            }
            ')' => {
                out.push(Token::RParen);
                i += 1;
            }
            '[' => {
                out.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Token::RBracket);
                i += 1;
            }
            '{' => {
                out.push(Token::LBrace);
                i += 1;
            }
            '}' => {
                out.push(Token::RBrace);
                i += 1;
            }
            '*' => {
                out.push(Token::Star);
                i += 1;
            }
            ':' => {
                out.push(Token::Colon);
                i += 1;
            }
            ',' => {
                out.push(Token::Comma);
                i += 1;
            }
            '.' => {
                out.push(Token::Dot);
                i += 1;
            }
            '|' => {
                out.push(Token::Pipe);
                i += 1;
            }
            '-' => {
                if input[i..].starts_with("->") {
                    out.push(Token::ArrowRight);
                    i += 2;
                } else {
                    out.push(Token::Dash);
                    i += 1;
                }
            }
            '<' => {
                if input[i..].starts_with("<-") {
                    out.push(Token::ArrowLeft);
                    i += 2;
                } else if input[i..].starts_with("<=") {
                    out.push(Token::Le);
                    i += 2;
                } else if input[i..].starts_with("<>") {
                    out.push(Token::Ne);
                    i += 2;
                } else {
                    out.push(Token::Lt);
                    i += 1;
                }
            }
            '>' => {
                if input[i..].starts_with(">=") {
                    out.push(Token::Ge);
                    i += 2;
                } else {
                    out.push(Token::Gt);
                    i += 1;
                }
            }
            '=' => {
                if input[i..].starts_with("=~") {
                    out.push(Token::RegexMatch);
                    i += 2;
                } else {
                    out.push(Token::Eq);
                    i += 1;
                }
            }
            '!' if input[i..].starts_with("!=") => {
                out.push(Token::Ne);
                i += 2;
            }
            '\'' | '"' => {
                let quote = c;
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] as char != quote {
                    j += 1;
                }
                if j >= bytes.len() {
                    return Err(format!("unterminated string at {i}"));
                }
                out.push(Token::Str(input[start..j].to_string()));
                i = j + 1;
            }
            // Backtick-quoted identifier — Neo4j-standard escape for identifiers
            // containing dots, spaces, or other non-ident chars (e.g. `p.name`).
            // Emitted as a normal Ident token so downstream parsing code
            // (alias slot in parse_return_item, future label/property names)
            // sees no difference between `foo` and just foo. The inner bytes
            // are taken verbatim — no `\` escape processing, matching Neo4j.
            '`' => {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && bytes[j] != b'`' {
                    j += 1;
                }
                if j >= bytes.len() {
                    return Err(format!("unterminated backtick identifier at {i}"));
                }
                if j == start {
                    return Err(format!("empty backtick identifier at {i}"));
                }
                out.push(Token::Ident(input[start..j].to_string()));
                i = j + 1;
            }
            c if c.is_ascii_digit() => {
                let start = i;
                let mut seen_dot = false;
                while i < bytes.len() {
                    let d = bytes[i] as char;
                    if d.is_ascii_digit() {
                        i += 1;
                    } else if d == '.' && !seen_dot {
                        seen_dot = true;
                        i += 1;
                    } else {
                        break;
                    }
                }
                let text = &input[start..i];
                if seen_dot {
                    out.push(Token::Float(text.parse().map_err(|_| "bad float")?));
                } else {
                    out.push(Token::Int(text.parse().map_err(|_| "bad int")?));
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                // Unicode-aware identifier scan: accepts CJK / accented letters
                // (`名稱`, `café`) so a non-ASCII property/label name lexes as a
                // normal Ident instead of panicking. Advances by whole chars.
                let start = i;
                while i < bytes.len() {
                    let d = input[i..].chars().next().unwrap();
                    if d.is_alphanumeric() || d == '_' {
                        i += d.len_utf8();
                    } else {
                        break;
                    }
                }
                out.push(Token::Ident(input[start..i].to_string()));
            }
            other => return Err(format!("unexpected char {other:?} at {i}")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a non-ASCII char in identifier position (CJK property name,
    /// emoji, accented letter) used to panic — `bytes[i] as char` mis-classified
    /// a multibyte lead byte as alphabetic, then the byte-by-byte slice landed on
    /// a non-char boundary. It must now lex without panicking.
    #[test]
    fn tokenize_non_ascii_identifier_does_not_panic() {
        // CJK identifier lexes as a single Ident token, byte-exact.
        assert_eq!(tokenize("名稱").unwrap(), vec![Token::Ident("名稱".into())]);
        // accented Latin letters likewise.
        assert_eq!(tokenize("café").unwrap(), vec![Token::Ident("café".into())]);
        // CJK after a dot (e.g. `RETURN p.名稱`) tokenizes cleanly.
        assert_eq!(
            tokenize("p.名稱").unwrap(),
            vec![Token::Ident("p".into()), Token::Dot, Token::Ident("名稱".into())]
        );
    }

    /// An emoji (non-alphabetic, non-ASCII) is not a valid token, but must yield
    /// an Err — never a panic.
    #[test]
    fn tokenize_emoji_is_error_not_panic() {
        assert!(tokenize("🎉").is_err());
    }

    /// CJK inside a string literal was already safe; pin it so the boundary fix
    /// doesn't regress quoted multibyte content.
    #[test]
    fn tokenize_cjk_in_string_literal() {
        assert_eq!(tokenize("'咖啡'").unwrap(), vec![Token::Str("咖啡".into())]);
    }
}
