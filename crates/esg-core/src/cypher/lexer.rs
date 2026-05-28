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
        let c = bytes[i] as char;
        match c {
            c if c.is_whitespace() => i += 1,
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
                let start = i;
                while i < bytes.len() {
                    let d = bytes[i] as char;
                    if d.is_alphanumeric() || d == '_' {
                        i += 1;
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
