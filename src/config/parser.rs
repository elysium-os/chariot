use std::{collections::HashMap, fmt::Display};
use thiserror::Error;

use super::lexer::Token;

#[derive(Error, Debug)]
pub enum ParserError {
    #[error("Unexpected end of file")]
    UnexpectedEOF,

    #[error("Unexpected token `{0}`")]
    UnexpectedToken(Token),

    #[error("Redefined object key `{0}`")]
    DuplicateObjectKey(String),
}

#[derive(Debug)]
pub enum ConfigFragment {
    Directive { name: String, value: Box<ConfigFragment> },
    Definition { key: Box<ConfigFragment>, value: Box<ConfigFragment> },
    Object(HashMap<String, Box<ConfigFragment>>),
    String(String),
    List(Vec<ConfigFragment>),
    RecipeRef { namespace: String, name: String },
    CodeBlock { lang: String, code: String },
    Unary { operation: char, value: Box<ConfigFragment> },
}

impl Display for ConfigFragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            Self::Directive { name, value: _ } => write!(f, "Directive({})", name),
            Self::Definition { key: _, value: _ } => write!(f, "Definition(...)"),
            Self::Object(_) => write!(f, "Object(...)"),
            Self::String(str) => write!(f, "String({})", str),
            Self::List(_) => write!(f, "List(...)"),
            Self::RecipeRef { namespace, name } => write!(f, "RecipeRef({}/{})", namespace, name),
            Self::CodeBlock { lang, code: _ } => write!(f, "CodeBlock({})", lang),
            Self::Unary { operation, value: _ } => write!(f, "Unary({})", operation),
        }
    }
}

macro_rules! expect {
    ($vec:expr, $pat:pat => $val:expr) => {
        match $vec.pop() {
            Some($pat) => $val,
            Some(frag) => return Err(ParserError::UnexpectedToken(frag)),
            None => return Err(ParserError::UnexpectedEOF),
        }
    };
}

macro_rules! try_expect {
    ($vec:expr, $pat:pat => $val:expr) => {
        match $vec.last() {
            Some($pat) => $vec.pop(),
            _ => None,
        }
    };
}

pub fn parse_config(tokens: &mut Vec<Token>) -> Result<Vec<ConfigFragment>, ParserError> {
    let mut top_level_fragments: Vec<ConfigFragment> = Vec::new();
    while !tokens.is_empty() {
        match tokens.last() {
            Some(Token::Symbol('@')) => top_level_fragments.push(parse_directive(tokens)?),
            _ => top_level_fragments.push(parse_definition(tokens)?),
        }
    }
    Ok(top_level_fragments)
}

fn parse_definition(tokens: &mut Vec<Token>) -> Result<ConfigFragment, ParserError> {
    Ok(ConfigFragment::Definition {
        key: Box::new(parse_recipe_ref(tokens)?),
        value: Box::new(parse_object(tokens)?),
    })
}

fn parse_directive(tokens: &mut Vec<Token>) -> Result<ConfigFragment, ParserError> {
    expect!(tokens, Token::Symbol('@') => ());
    let name = expect!(tokens, Token::Identifier(v) => v);
    Ok(ConfigFragment::Directive {
        name,
        value: Box::new(parse_value(tokens)?),
    })
}

fn parse_recipe_ref(tokens: &mut Vec<Token>) -> Result<ConfigFragment, ParserError> {
    let namespace = expect!(tokens, Token::Identifier(v) => v);
    expect!(tokens, Token::Symbol('/') => ());
    let recipe = expect!(tokens, Token::Identifier(v) => v);

    Ok(ConfigFragment::RecipeRef { namespace, name: recipe })
}

fn parse_value(tokens: &mut Vec<Token>) -> Result<ConfigFragment, ParserError> {
    match tokens.last() {
        Some(Token::Symbol('[')) => parse_list(tokens),
        Some(Token::Symbol('{')) => parse_object(tokens),
        Some(Token::Symbol('*')) => parse_unary(tokens),
        Some(Token::Identifier(_)) => parse_recipe_ref(tokens),
        Some(Token::String(_)) => Ok(ConfigFragment::String(expect!(tokens, Token::String(v) => v))),
        Some(Token::CodeBlock { code: _, lang: _ }) => Ok(expect!(tokens, Token::CodeBlock{lang, code} => ConfigFragment::CodeBlock { lang, code })),
        Some(_) => Err(ParserError::UnexpectedToken(tokens.pop().unwrap())),
        None => Err(ParserError::UnexpectedEOF),
    }
}

fn parse_object(tokens: &mut Vec<Token>) -> Result<ConfigFragment, ParserError> {
    expect!(tokens, Token::Symbol('{') => ());

    let mut values = HashMap::<String, Box<ConfigFragment>>::new();
    while try_expect!(tokens, Token::Symbol('}') => ()).is_none() {
        let key = expect!(tokens, Token::Identifier(v) => v);
        expect!(tokens, Token::Symbol(':') => ());

        if values.insert(key.clone(), Box::new(parse_value(tokens)?)).is_some() && key != "dependencies" {
            return Err(ParserError::DuplicateObjectKey(key));
        }

        try_expect!(tokens, Token::Symbol(',') => ());
    }

    Ok(ConfigFragment::Object(values))
}

fn parse_unary(tokens: &mut Vec<Token>) -> Result<ConfigFragment, ParserError> {
    expect!(tokens, Token::Symbol('*') => ());
    return Ok(ConfigFragment::Unary {
        operation: '*',
        value: Box::new(parse_value(tokens)?),
    });
}

fn parse_list(tokens: &mut Vec<Token>) -> Result<ConfigFragment, ParserError> {
    expect!(tokens, Token::Symbol('[') => ());

    let mut values = Vec::<ConfigFragment>::new();
    while try_expect!(tokens, Token::Symbol(']') => ()).is_none() {
        values.push(parse_value(tokens)?);
        try_expect!(tokens, Token::Symbol(',') => ());
    }

    Ok(ConfigFragment::List(values))
}
