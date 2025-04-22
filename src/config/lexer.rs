use std::fmt::{Debug, Display};
use std::iter::{self, from_fn};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LexerError {
    #[error("Unexpected symbol `{ch}`")]
    UnexpectedSymbol { ch: char },

    #[error("Unexpected symbol in embed tag `{ch}`")]
    UnexpectedSymbolInTag { ch: char },

    #[error("Embed not closed")]
    UnclosedEmbed,

    #[error("String does not terminate")]
    UnclosedString,
}

#[derive(Debug)]
pub enum Token {
    Identifier(String),
    Symbol(char),
    String(String),
    CodeBlock { lang: String, code: String },
}

impl Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            Self::Identifier(str) => write!(f, "Identifier({})", str),
            Self::Symbol(char) => write!(f, "Symbol({})", char),
            Self::String(str) => write!(f, "String({})", str),
            Self::CodeBlock { lang, code: _ } => write!(f, "CodeBlock({})", lang),
        }
    }
}

pub fn tokenize(input: &str) -> Result<Vec<Token>, LexerError> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut iter = input.chars().peekable();

    while let Some(ch) = iter.next() {
        match ch {
            ch if ch.is_whitespace() => continue,
            '{' | '}' | ':' | '[' | ']' | ',' | '*' | '#' | '!' | '@' | '=' => tokens.push(Token::Symbol(ch)),
            ch if ch.is_alphabetic() => {
                let str: String = iter::once(ch)
                    .chain(from_fn(|| iter.by_ref().next_if(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '-' || *ch == '.')))
                    .collect::<String>();

                tokens.push(Token::Identifier(str))
            }
            '/' => match iter.peek() {
                Some('/') => loop {
                    iter.next();
                    if iter.peek().is_none_or(|c| *c == '\n') {
                        break;
                    }
                },
                Some('*') => {
                    iter.next();
                    'parent: loop {
                        loop {
                            match iter.next() {
                                None => break 'parent,
                                Some('*') => break,
                                _ => continue,
                            }
                        }

                        if iter.next().is_none_or(|c| c == '/') {
                            break;
                        }
                    }
                }
                _ => tokens.push(Token::Symbol('/')),
            },
            '"' => {
                let str = match iter.next() {
                    None => return Err(LexerError::UnclosedString),
                    Some(first_ch) => {
                        let str: String = iter::once(first_ch).chain(from_fn(|| iter.by_ref().next_if(|ch| *ch != '"'))).collect::<String>();
                        if iter.next() == None {
                            return Err(LexerError::UnclosedString);
                        }
                        str
                    }
                };

                tokens.push(Token::String(str));
            }
            '<' => {
                let mut lang = String::new();
                loop {
                    match iter.next() {
                        Some(ch) if ch.is_alphabetic() => lang.push(ch),
                        Some('>') => break,
                        Some(ch) => return Err(LexerError::UnexpectedSymbolInTag { ch }),
                        None => return Err(LexerError::UnclosedEmbed),
                    }
                }

                let mut code = String::new();
                loop {
                    match iter.next() {
                        Some('<') => {
                            match iter.next() {
                                Some('/') => {}
                                Some(ch) => {
                                    code.push('<');
                                    code.push(ch);
                                    break;
                                }
                                None => return Err(LexerError::UnclosedEmbed),
                            }

                            let mut matching = false;
                            let mut close_lang = String::new();
                            loop {
                                match iter.next() {
                                    Some('>') => {
                                        if lang == close_lang {
                                            matching = true;
                                        } else {
                                            close_lang.push('>');
                                        }
                                        break;
                                    }
                                    Some(ch) => {
                                        close_lang.push(ch);
                                        if !ch.is_alphabetic() {
                                            break;
                                        }
                                    }
                                    None => return Err(LexerError::UnclosedEmbed),
                                }
                            }

                            if matching {
                                break;
                            }

                            code.push('<');
                            code.push('/');
                            code.push_str(close_lang.as_str());
                        }
                        Some(ch) => code.push(ch),
                        None => return Err(LexerError::UnclosedEmbed),
                    }
                }

                tokens.push(Token::CodeBlock { lang, code });
            }
            _ => return Err(LexerError::UnexpectedSymbol { ch }),
        }
    }

    tokens.reverse();

    Ok(tokens)
}
