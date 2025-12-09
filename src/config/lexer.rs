use std::fmt::{Debug, Display};

use thiserror::Error;

#[derive(Error, Debug)]
pub enum LexerError {
    #[error("Unexpected symbol `{ch}`")]
    UnexpectedSymbol { offset: usize, ch: char },

    #[error("Unexpected EOF")]
    UnexpectedEOF,
}

#[derive(Debug)]
pub enum Token {
    Identifier(String),
    Symbol(char),
    String(String),
    Directive(String),
    CodeBlock { lang: String, code: String },
}

impl Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            Self::Identifier(id) => write!(f, "Identifier({})", id),
            Self::Symbol(char) => write!(f, "Symbol({})", char),
            Self::String(str) => write!(f, "String({})", str),
            Self::Directive(id) => write!(f, "Directive({})", id),
            Self::CodeBlock { lang, code: _ } => write!(f, "CodeBlock({})", lang),
        }
    }
}

pub fn lex(input: &str) -> Result<Vec<Token>, LexerError> {
    let mut iter = input.char_indices().peekable();
    let mut tokens: Vec<Token> = Vec::new();

    enum LexState {
        Initial,
        CommentPossible,
        CommentLine,
        CommentMulti { end: bool },
        Identifier(Vec<char>),
        String(Vec<char>),
        Directive(Vec<char>),
        CodeBlockLang(Vec<char>),
        CodeBlock { lang: String, code: Vec<char> },
        CodeBlockEnd { lang: String, code: Vec<char>, endtag: Option<Vec<char>> },
    }

    let mut state: LexState = LexState::Initial;

    let mut current_value = iter.next();
    while let Some((offset, ch)) = current_value {
        match &mut state {
            LexState::Initial => match ch {
                ch if ch.is_whitespace() => {}
                '{' | '}' | ':' | '[' | ']' | ',' | '*' | '%' | '!' | '=' => tokens.push(Token::Symbol(ch)),
                ch if ch.is_alphabetic() => state = LexState::Identifier(vec![ch]),
                '/' => state = LexState::CommentPossible,
                '"' => state = LexState::String(Vec::new()),
                '<' => state = LexState::CodeBlockLang(Vec::new()),
                '@' => state = LexState::Directive(Vec::new()),
                _ => return Err(LexerError::UnexpectedSymbol { offset, ch }),
            },
            LexState::CommentPossible => match ch {
                '/' => state = LexState::CommentLine,
                '*' => state = LexState::CommentMulti { end: false },
                _ => {
                    tokens.push(Token::Symbol('/'));
                    state = LexState::Initial;
                    continue;
                }
            },
            LexState::CommentLine => match ch {
                '\n' => state = LexState::Initial,
                _ => {}
            },
            LexState::CommentMulti { end: false } => match ch {
                '*' => state = LexState::CommentMulti { end: true },
                _ => {}
            },
            LexState::CommentMulti { end: true } => match ch {
                '/' => state = LexState::Initial,
                '*' => state = LexState::CommentMulti { end: true },
                _ => state = LexState::CommentMulti { end: false },
            },
            LexState::Identifier(id) => match ch {
                ch if ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '+' || ch == '.' => id.push(ch),
                _ => {
                    tokens.push(Token::Identifier(id.iter().collect::<String>()));
                    state = LexState::Initial;
                    continue;
                }
            },
            LexState::String(str) => match ch {
                '"' => {
                    tokens.push(Token::String(str.iter().collect::<String>()));
                    state = LexState::Initial;
                }
                _ => str.push(ch),
            },
            LexState::Directive(id) => match ch {
                ch if ch.is_alphanumeric() || ch == '_' || ch == '-' => id.push(ch),
                _ => {
                    tokens.push(Token::Directive(id.iter().collect::<String>()));
                    state = LexState::Initial;
                    continue;
                }
            },
            LexState::CodeBlockLang(lang) => match ch {
                ch if ch.is_alphabetic() => lang.push(ch),
                '>' => {
                    state = LexState::CodeBlock {
                        lang: lang.iter().collect::<String>(),
                        code: Vec::new(),
                    }
                }
                _ => return Err(LexerError::UnexpectedSymbol { offset, ch }),
            },
            LexState::CodeBlock { lang, code } => match ch {
                '<' => {
                    state = LexState::CodeBlockEnd {
                        lang: lang.clone(),
                        code: code.clone(),
                        endtag: None,
                    };
                }
                _ => code.push(ch),
            },
            LexState::CodeBlockEnd { lang, code, endtag: None } => match ch {
                '/' => {
                    state = LexState::CodeBlockEnd {
                        lang: lang.clone(),
                        code: code.clone(),
                        endtag: Some(Vec::new()),
                    }
                }
                _ => {
                    code.push('<');
                    state = LexState::CodeBlock {
                        lang: lang.clone(),
                        code: code.clone(),
                    };
                }
            },
            LexState::CodeBlockEnd { lang, code, endtag: Some(endtag) } => {
                if lang.len() == endtag.len() && ch == '>' {
                    tokens.push(Token::CodeBlock {
                        lang: lang.clone(),
                        code: code.iter().collect::<String>(),
                    });
                    state = LexState::Initial;
                } else {
                    endtag.push(ch);
                    if !lang.starts_with(endtag.iter().collect::<String>().as_str()) {
                        code.push('<');
                        code.push('/');
                        code.append(endtag);
                        state = LexState::CodeBlock {
                            lang: lang.clone(),
                            code: code.clone(),
                        };
                    }
                }
            }
        }
        current_value = iter.next();
    }

    if !matches!(state, LexState::Initial) {
        return Err(LexerError::UnexpectedEOF);
    }

    Ok(tokens)
}
