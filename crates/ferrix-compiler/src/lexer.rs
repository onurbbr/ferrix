//! Lexer for Ferrix source code.
//!
//! The lexer turns raw source text into tokens with byte-based source spans.
//! It handles keywords, identifiers, integer literals, strings with escapes,
//! punctuation, and reports invalid characters as compile errors.

use ferrix_core::diagnostics::{FileId, SourceSpan};

use crate::error::{CompileError, CompileErrorKind};

/// One lexical token with its original source span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    /// Token category and any literal payload.
    pub kind: TokenKind,
    /// Byte span in the source file.
    pub span: SourceSpan,
}

/// Ferrix token categories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Identifier(String),
    Integer(i64),
    String(String),
    Let,
    Fn,
    Export,
    Import,
    If,
    Else,
    While,
    Return,
    Throw,
    Try,
    Catch,
    True,
    False,
    Nil,
    Plus,
    Minus,
    Star,
    Slash,
    Equal,
    EqualEqual,
    BangEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Colon,
    Dot,
    Comma,
    Semicolon,
    Eof,
}

/// Tokenizes a source string and appends an EOF token.
pub fn lex(source: &str, file_id: FileId) -> Result<Vec<Token>, CompileError> {
    Lexer::new(source, file_id).lex()
}

struct Lexer<'a> {
    source: &'a str,
    file_id: FileId,
    bytes: &'a [u8],
    current: usize,
}

impl<'a> Lexer<'a> {
    fn new(source: &'a str, file_id: FileId) -> Self {
        Self {
            source,
            file_id,
            bytes: source.as_bytes(),
            current: 0,
        }
    }

    fn lex(mut self) -> Result<Vec<Token>, CompileError> {
        let mut tokens = Vec::new();

        while let Some(byte) = self.peek() {
            let start = self.current;
            match byte {
                b' ' | b'\r' | b'\t' | b'\n' => {
                    self.advance();
                }
                b'0'..=b'9' => tokens.push(self.integer(start)?),
                b'"' => tokens.push(self.string(start)?),
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => tokens.push(self.identifier(start)),
                b'+' => tokens.push(self.single(start, TokenKind::Plus)),
                b'-' => tokens.push(self.single(start, TokenKind::Minus)),
                b'*' => tokens.push(self.single(start, TokenKind::Star)),
                b'/' => tokens.push(self.single(start, TokenKind::Slash)),
                b'(' => tokens.push(self.single(start, TokenKind::LeftParen)),
                b')' => tokens.push(self.single(start, TokenKind::RightParen)),
                b'{' => tokens.push(self.single(start, TokenKind::LeftBrace)),
                b'}' => tokens.push(self.single(start, TokenKind::RightBrace)),
                b'[' => tokens.push(self.single(start, TokenKind::LeftBracket)),
                b']' => tokens.push(self.single(start, TokenKind::RightBracket)),
                b':' => tokens.push(self.single(start, TokenKind::Colon)),
                b'.' => tokens.push(self.single(start, TokenKind::Dot)),
                b',' => tokens.push(self.single(start, TokenKind::Comma)),
                b';' => tokens.push(self.single(start, TokenKind::Semicolon)),
                b'=' => tokens.push(self.double_or_single(
                    start,
                    b'=',
                    TokenKind::EqualEqual,
                    TokenKind::Equal,
                )),
                b'!' => {
                    self.advance();
                    if self.match_byte(b'=') {
                        tokens.push(Token {
                            kind: TokenKind::BangEqual,
                            span: self.span(start, self.current),
                        });
                    } else {
                        return Err(CompileError::new(
                            CompileErrorKind::UnexpectedCharacter { character: '!' },
                            Some(self.span(start, self.current)),
                        ));
                    }
                }
                b'<' => tokens.push(self.double_or_single(
                    start,
                    b'=',
                    TokenKind::LessEqual,
                    TokenKind::Less,
                )),
                b'>' => tokens.push(self.double_or_single(
                    start,
                    b'=',
                    TokenKind::GreaterEqual,
                    TokenKind::Greater,
                )),
                _ => {
                    self.advance();
                    return Err(CompileError::new(
                        CompileErrorKind::UnexpectedCharacter {
                            character: byte as char,
                        },
                        Some(self.span(start, self.current)),
                    ));
                }
            }
        }

        tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.span(self.current, self.current),
        });
        Ok(tokens)
    }

    fn integer(&mut self, start: usize) -> Result<Token, CompileError> {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.advance();
        }

        let literal = &self.source[start..self.current];
        let value = literal.parse::<i64>().map_err(|_| {
            CompileError::new(
                CompileErrorKind::InvalidInteger {
                    literal: literal.to_string(),
                },
                Some(self.span(start, self.current)),
            )
        })?;

        Ok(Token {
            kind: TokenKind::Integer(value),
            span: self.span(start, self.current),
        })
    }

    fn string(&mut self, start: usize) -> Result<Token, CompileError> {
        self.advance();
        let mut value = String::new();

        while let Some(ch) = self.peek_char() {
            match ch {
                '"' => {
                    self.advance_char(ch);
                    return Ok(Token {
                        kind: TokenKind::String(value),
                        span: self.span(start, self.current),
                    });
                }
                '\n' | '\r' => {
                    return Err(CompileError::new(
                        CompileErrorKind::UnterminatedString,
                        Some(self.span(start, self.current)),
                    ));
                }
                '\\' => {
                    self.advance_char(ch);
                    let Some(escape) = self.peek_char() else {
                        return Err(CompileError::new(
                            CompileErrorKind::UnterminatedString,
                            Some(self.span(start, self.current)),
                        ));
                    };
                    self.advance_char(escape);
                    match escape {
                        '"' => value.push('"'),
                        '\\' => value.push('\\'),
                        'n' => value.push('\n'),
                        'r' => value.push('\r'),
                        't' => value.push('\t'),
                        _ => {
                            return Err(CompileError::new(
                                CompileErrorKind::InvalidStringEscape { escape },
                                Some(self.span(start, self.current)),
                            ));
                        }
                    }
                }
                _ => {
                    self.advance_char(ch);
                    value.push(ch);
                }
            }
        }

        Err(CompileError::new(
            CompileErrorKind::UnterminatedString,
            Some(self.span(start, self.current)),
        ))
    }

    fn identifier(&mut self, start: usize) -> Token {
        while matches!(
            self.peek(),
            Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
        ) {
            self.advance();
        }

        let text = &self.source[start..self.current];
        let kind = match text {
            "let" => TokenKind::Let,
            "fn" => TokenKind::Fn,
            "export" => TokenKind::Export,
            "import" => TokenKind::Import,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "return" => TokenKind::Return,
            "throw" => TokenKind::Throw,
            "try" => TokenKind::Try,
            "catch" => TokenKind::Catch,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            _ => TokenKind::Identifier(text.to_string()),
        };

        Token {
            kind,
            span: self.span(start, self.current),
        }
    }

    fn single(&mut self, start: usize, kind: TokenKind) -> Token {
        self.advance();
        Token {
            kind,
            span: self.span(start, self.current),
        }
    }

    fn double_or_single(
        &mut self,
        start: usize,
        second: u8,
        double: TokenKind,
        single: TokenKind,
    ) -> Token {
        self.advance();
        let kind = if self.match_byte(second) {
            double
        } else {
            single
        };
        Token {
            kind,
            span: self.span(start, self.current),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.current).copied()
    }

    fn peek_char(&self) -> Option<char> {
        self.source.get(self.current..)?.chars().next()
    }

    fn advance(&mut self) {
        self.current += 1;
    }

    fn advance_char(&mut self, ch: char) {
        self.current += ch.len_utf8();
    }

    fn match_byte(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn span(&self, start: usize, end: usize) -> SourceSpan {
        SourceSpan::new(self.file_id, start, end)
    }
}

impl TokenKind {
    /// Returns a human-readable token description for parser diagnostics.
    pub fn describe(&self) -> String {
        match self {
            Self::Identifier(name) => format!("identifier `{name}`"),
            Self::Integer(value) => format!("integer `{value}`"),
            Self::String(_) => "string literal".to_string(),
            Self::Let => "`let`".to_string(),
            Self::Fn => "`fn`".to_string(),
            Self::Export => "`export`".to_string(),
            Self::Import => "`import`".to_string(),
            Self::If => "`if`".to_string(),
            Self::Else => "`else`".to_string(),
            Self::While => "`while`".to_string(),
            Self::Return => "`return`".to_string(),
            Self::Throw => "`throw`".to_string(),
            Self::Try => "`try`".to_string(),
            Self::Catch => "`catch`".to_string(),
            Self::True => "`true`".to_string(),
            Self::False => "`false`".to_string(),
            Self::Nil => "`nil`".to_string(),
            Self::Plus => "`+`".to_string(),
            Self::Minus => "`-`".to_string(),
            Self::Star => "`*`".to_string(),
            Self::Slash => "`/`".to_string(),
            Self::Equal => "`=`".to_string(),
            Self::EqualEqual => "`==`".to_string(),
            Self::BangEqual => "`!=`".to_string(),
            Self::Less => "`<`".to_string(),
            Self::LessEqual => "`<=`".to_string(),
            Self::Greater => "`>`".to_string(),
            Self::GreaterEqual => "`>=`".to_string(),
            Self::LeftParen => "`(`".to_string(),
            Self::RightParen => "`)`".to_string(),
            Self::LeftBrace => "`{`".to_string(),
            Self::RightBrace => "`}`".to_string(),
            Self::LeftBracket => "`[`".to_string(),
            Self::RightBracket => "`]`".to_string(),
            Self::Colon => "`:`".to_string(),
            Self::Dot => "`.`".to_string(),
            Self::Comma => "`,`".to_string(),
            Self::Semicolon => "`;`".to_string(),
            Self::Eof => "end of file".to_string(),
        }
    }
}
