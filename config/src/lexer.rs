use logos::{Lexer, Logos};

#[derive(Debug, PartialEq, Clone)]
pub struct Block<'a> {
    pub tag: &'a str,
    pub content: &'a str,
}

#[derive(Debug, PartialEq, Clone)]
pub struct NamespacedIdentifier<'a> {
    pub namespace: &'a str,
    pub identifier: &'a str,
}

#[derive(Logos, Debug, Clone)]
#[logos(skip r"[ \t\n\r\f]+")]
#[regex(r"//[^\n]*", skip)]
#[regex(r"\/\*(.|\n)*?\*\/", skip)]
pub enum Token<'a> {
    #[regex(r"[a-zA-Z][a-zA-Z0-9_\-+.]*")]
    Identifier(&'a str),

    #[regex(r"[a-zA-Z]+\/[a-zA-Z][a-zA-Z0-9_\-+.]*", lex_namespaced_identifier)]
    NamespacedIdentifier(NamespacedIdentifier<'a>),

    #[regex(r"@[a-zA-Z_]+", |lex| &lex.slice()[1..])]
    Directive(&'a str),

    #[regex(r#""[^\n"]*""#, |lex| let s = lex.slice(); &s[1..s.len() - 1] )]
    String(&'a str),

    #[regex(r"<", lex_code_block)]
    Block(Block<'a>),

    #[token("{")]
    SymbolBraceLeft,

    #[token("}")]
    SymbolBraceRight,

    #[token("[")]
    SymbolBracketLeft,

    #[token("]")]
    SymbolBracketRight,

    #[token(":")]
    SymbolColon,

    #[token(",")]
    SymbolComma,

    #[token("*")]
    SymbolStar,

    #[token("%")]
    SymbolPercentage,

    #[token("!")]
    SymbolExclamation,

    #[token("=")]
    SymbolEqual,

    #[token("?")]
    SymbolQuestion,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TokenTag {
    Identifier,
    NamespacedIdentifier,
    Directive,
    String,
    CodeBlock,
    SymbolBraceLeft,
    SymbolBraceRight,
    SymbolBracketLeft,
    SymbolBracketRight,
    SymbolColon,
    SymbolComma,
    SymbolStar,
    SymbolPercentage,
    SymbolExclamation,
    SymbolEqual,
    SymbolQuestion,
}

impl TokenTag {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Identifier => "Identifier",
            Self::NamespacedIdentifier => "Namespaced Identifier",
            Self::Directive => "Directive",
            Self::String => "String",
            Self::CodeBlock => "Code Block",
            Self::SymbolBraceLeft => "`{{`",
            Self::SymbolBraceRight => "`}}`",
            Self::SymbolBracketLeft => "`[`",
            Self::SymbolBracketRight => "`]`",
            Self::SymbolColon => "`:`",
            Self::SymbolComma => "`,`",
            Self::SymbolStar => "`*`",
            Self::SymbolPercentage => "`%`",
            Self::SymbolExclamation => "`!`",
            Self::SymbolEqual => "`=`",
            Self::SymbolQuestion => "`?`",
        }
    }
}

impl<'a> Token<'a> {
    pub fn tag(&self) -> TokenTag {
        match self {
            Self::Identifier(_) => TokenTag::Identifier,
            Self::NamespacedIdentifier(_) => TokenTag::NamespacedIdentifier,
            Self::Directive(_) => TokenTag::Directive,
            Self::String(_) => TokenTag::String,
            Self::Block(_) => TokenTag::CodeBlock,
            Self::SymbolBraceLeft => TokenTag::SymbolBraceLeft,
            Self::SymbolBraceRight => TokenTag::SymbolBraceRight,
            Self::SymbolBracketLeft => TokenTag::SymbolBracketLeft,
            Self::SymbolBracketRight => TokenTag::SymbolBracketRight,
            Self::SymbolColon => TokenTag::SymbolColon,
            Self::SymbolComma => TokenTag::SymbolComma,
            Self::SymbolStar => TokenTag::SymbolStar,
            Self::SymbolPercentage => TokenTag::SymbolPercentage,
            Self::SymbolExclamation => TokenTag::SymbolExclamation,
            Self::SymbolEqual => TokenTag::SymbolEqual,
            Self::SymbolQuestion => TokenTag::SymbolQuestion,
        }
    }
}

fn lex_namespaced_identifier<'a>(lex: &mut Lexer<'a, Token<'a>>) -> Option<NamespacedIdentifier<'a>> {
    let s = lex.slice();
    let slash = s.find('/')?;
    Some(NamespacedIdentifier {
        namespace: &s[..slash],
        identifier: &s[slash + 1..],
    })
}

fn lex_code_block<'a>(lex: &mut Lexer<'a, Token<'a>>) -> Option<Block<'a>> {
    let source = lex.remainder();

    let tag_end = source.find('>')?;
    let tag_name = &source[..tag_end];

    if !tag_name.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }

    let close = format!("</{}>", tag_name);
    let body_start = tag_end + 1;
    let close_pos = source[body_start..].find(&close)?;

    lex.bump(tag_end + 1 + close_pos + close.len());

    let full = lex.slice();
    let inner_start = tag_end + 2;
    let inner_end = full.len() - close.len();
    let content = &full[inner_start..inner_end];

    Some(Block {
        tag: tag_name,
        content: content.trim(),
    })
}
