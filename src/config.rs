use anyhow::{Context, Result, bail};
use log::debug;
use std::collections::HashMap;
use std::fmt::Display;
use std::iter::from_fn;
use std::ops::Deref;
use std::path::PathBuf;
use std::{fs, iter};

use crate::recipe::{self, Recipe, RecipeDependency, RecipeId};

enum Token {
    Identifier(String),
    Symbol(char),
    String(String),
    CodeBlock(String, String),
}

enum ConfigFragment {
    Directive(String, Box<ConfigFragment>),
    Definition(Box<ConfigFragment>, Box<ConfigFragment>),
    Object(HashMap<String, Box<ConfigFragment>>),
    String(String),
    List(Vec<ConfigFragment>),
    RecipeRef(String, String),
    CodeBlock(String, String),
    Unary(char, Box<ConfigFragment>),
}

impl Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            Self::Identifier(str) => write!(f, "Identifier({})", str)?,
            Self::Symbol(char) => write!(f, "Symbol({})", char)?,
            Self::String(str) => write!(f, "String({})", str)?,
            Self::CodeBlock(lang, _) => write!(f, "CodeBlock({})", lang)?,
        };
        Ok(())
    }
}

impl Display for ConfigFragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self {
            Self::Definition(_, _) => write!(f, "Definition(...)")?,
            Self::Directive(name, _) => write!(f, "Directive({})", name)?,
            Self::Object(_) => write!(f, "Object(...)")?,
            Self::String(str) => write!(f, "String({})", str)?,
            Self::List(_) => write!(f, "List(...)")?,
            Self::RecipeRef(namespace, name) => write!(f, "RecipeRef({}/{})", namespace, name)?,
            Self::CodeBlock(lang, _) => write!(f, "CodeBlock({})", lang)?,
            Self::Unary(operation, _) => write!(f, "Unary({})", operation)?,
        };
        Ok(())
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut iter = input.chars().peekable();

    while let Some(ch) = iter.next() {
        match ch {
            ch if ch.is_whitespace() => continue,
            '{' | '}' | ':' | '[' | ']' | ',' | '*' | '@' => tokens.push(Token::Symbol(ch)),
            ch if ch.is_alphabetic() => {
                let str: String = iter::once(ch)
                    .chain(from_fn(|| {
                        iter.by_ref().next_if(|ch| ch.is_alphanumeric() || *ch == '_' || *ch == '-' || *ch == '.')
                    }))
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
                    None => bail!("String does not terminate"),
                    Some(first_ch) => {
                        let str: String = iter::once(first_ch)
                            .chain(from_fn(|| iter.by_ref().next_if(|ch| *ch != '"')))
                            .collect::<String>();
                        if iter.next() == None {
                            bail!("String does not terminate")
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
                        Some(ch) => bail!("Unexpected character in embed tag `{}`", ch),
                        None => bail!("Unclosed embed tag"),
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
                                None => bail!("Unclosed embed"),
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
                                    None => bail!("Unclosed embed"),
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
                        None => bail!("Unclosed embed"),
                    }
                }

                tokens.push(Token::CodeBlock(lang, code));
            }
            _ => bail!("Invalid character `{}`", ch),
        }
    }

    tokens.reverse();

    Ok(tokens)
}

macro_rules! expect_frag {
    ($frag:expr, $pat:pat => $val:expr) => {
        match $frag {
            $pat => $val,
            frag => bail!("Unexpected config fragment `{}`", frag),
        }
    };
}

macro_rules! try_consume_field {
    ($fields:expr, $field:literal, $frag_pat:pat => $frag_expr:expr) => {{
        let mut value = None;
        for field in $fields {
            if *field.0 != $field {
                continue;
            }

            value = Some(expect_frag!(field.1.0.deref(), $frag_pat => $frag_expr));
            field.1.1 = true;
        }
        value
    }};
}

macro_rules! consume_field {
    ($fields:expr, $field:literal, $frag_pat:pat => $frag_expr:expr) => {
        match try_consume_field!($fields, $field, $frag_pat => $frag_expr) {
            None => bail!("Field `{}` missing", $field),
            Some(value) => value
        }
    };
}

pub fn parse(path: PathBuf) -> Result<(HashMap<u32, Recipe>, HashMap<u32, Vec<RecipeDependency>>)> {
    let mut id_counter = 0_u32;
    let recipes_deps = parse_file(path, &mut id_counter).context("Failed to parse config")?;

    let mut dependencies: HashMap<u32, Vec<RecipeDependency>> = HashMap::new();
    for recipe in recipes_deps.iter() {
        let mut deps: Vec<RecipeDependency> = Vec::new();

        for dep in recipe.1.iter() {
            let mut found = false;
            for dep_recipe in recipes_deps.iter() {
                if dep_recipe.0.name != dep.1 {
                    continue;
                }

                if !match dep_recipe.0.kind {
                    recipe::Kind::Source(_) => dep.0 == "source",
                    recipe::Kind::Bare(_) => dep.0 == "bare",
                    recipe::Kind::Package(_) => dep.0 == "package",
                    recipe::Kind::Tool(_) => dep.0 == "tool",
                } {
                    continue;
                }

                deps.push(RecipeDependency {
                    recipe_id: dep_recipe.0.id,
                    runtime: dep.2,
                });
                found = true;
                break;
            }
            if !found {
                bail!("Unknown dependency `{}/{}`", dep.0, dep.1);
            }
        }

        dependencies.insert(recipe.0.id, deps);
    }

    let mut recipes: HashMap<u32, Recipe> = HashMap::new();
    for recipe in recipes_deps.into_iter() {
        recipes.insert(recipe.0.id, recipe.0);
    }

    Ok((recipes, dependencies))
}

fn parse_file(path: PathBuf, id_counter: &mut RecipeId) -> Result<Vec<(Recipe, Vec<(String, String, bool)>)>> {
    let data: String = fs::read_to_string(&path).context("Failed to read config")?;
    debug!("Parsing file at {}", path.to_str().unwrap());

    let tokens = &mut tokenize(data.as_str()).context("Failed to tokenize config")?;

    let mut definitions: Vec<ConfigFragment> = Vec::new();
    let mut directives: Vec<ConfigFragment> = Vec::new();
    while !tokens.is_empty() {
        match tokens.last() {
            Some(Token::Symbol('@')) => directives.push(parse_directive(tokens)?),
            _ => definitions.push(parse_definition(tokens)?),
        }
    }

    let mut recipes_deps: Vec<(Recipe, Vec<(String, String, bool)>)> = Vec::new();
    for directive in directives.iter() {
        let (name, value) = expect_frag!(directive, ConfigFragment::Directive(name, value) => (name, value));

        if name == "import" {
            let value = expect_frag!(value.as_ref(), ConfigFragment::String(v) => v);

            match path.parent() {
                Some(parent) => {
                    let mut imported_recdeps =
                        parse_file(parent.join(value), id_counter).context(format!("Failed to parse imported file: {}", value))?;
                    recipes_deps.append(&mut imported_recdeps);
                }
                None => bail!("Failed to import file {}", value),
            }
        }
    }

    for definition in definitions.iter() {
        let (key, value) = expect_frag!(definition, ConfigFragment::Definition(key, value) => (key, value));

        let (namespace, name) = expect_frag!(key.as_ref(), ConfigFragment::RecipeRef(namespace, name) => (namespace, name));

        let mut consumable_fields: HashMap<&String, (&Box<ConfigFragment>, bool)> = HashMap::new();
        for field in expect_frag!(value.as_ref(), ConfigFragment::Object(fields) => fields) {
            consumable_fields.insert(field.0, (field.1, false));
        }

        let mut deps: Vec<(String, String, bool)> = Vec::new();
        let mut image_deps: Vec<String> = Vec::new();

        match try_consume_field!(&mut consumable_fields, "dependencies", ConfigFragment::List(v) => v) {
            Some(recipe_deps) => {
                for dependency in recipe_deps {
                    let (dep, runtime) = match dependency {
                        ConfigFragment::Unary('*', frag) => (frag.deref(), true),
                        dep => (dep, false),
                    };

                    let (namespace, name) = expect_frag!(dep, ConfigFragment::RecipeRef(namespace, name) => (namespace, name));
                    if namespace == "image" {
                        image_deps.push(name.clone());
                    } else {
                        deps.push((namespace.clone(), name.clone(), runtime));
                    }
                }
            }
            None => {}
        }

        let recipe = recipe::Recipe {
            id: *id_counter,
            name: name.clone(),
            image_dependencies: image_deps,
            kind: match namespace.as_str() {
                "source" => {
                    let url = consume_field!(&mut consumable_fields, "url", ConfigFragment::String(v) => v.to_string());
                    let source_type = consume_field!(&mut consumable_fields, "type", ConfigFragment::String(v) => v);
                    let patch = try_consume_field!(&mut consumable_fields, "patch", ConfigFragment::String(v) => v.to_string());
                    let regenerate = try_consume_field!(&mut consumable_fields, "regenerate", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));

                    let kind = match source_type.as_str() {
                        "local" => recipe::SourceKind::Local,
                        "git" => recipe::SourceKind::Git(consume_field!(&mut consumable_fields, "ref", ConfigFragment::String(v) => v.to_string())),
                        "tar.gz" => {
                            recipe::SourceKind::TarGz(consume_field!(&mut consumable_fields, "b2sum", ConfigFragment::String(v) => v.to_string()))
                        }
                        "tar.xz" => {
                            recipe::SourceKind::TarXz(consume_field!(&mut consumable_fields, "b2sum", ConfigFragment::String(v) => v.to_string()))
                        }
                        v => bail!("Unknown source type {}", v),
                    };

                    recipe::Kind::Source(recipe::RecipeSource {
                        url,
                        kind,
                        patch,
                        regenerate: match regenerate {
                            None => None,
                            Some(regenerate) => Some(recipe::RecipeCodeBlock {
                                lang: regenerate.0,
                                code: regenerate.1,
                            }),
                        },
                    })
                }
                "bare" => {
                    let configure = try_consume_field!(&mut consumable_fields, "configure", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));
                    let build = try_consume_field!(&mut consumable_fields, "build", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));
                    let install = try_consume_field!(&mut consumable_fields, "install", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));

                    recipe::Kind::Bare(recipe::RecipeCommon {
                        configure: match configure {
                            Some(configure) => Some(recipe::RecipeCodeBlock {
                                lang: configure.0,
                                code: configure.1,
                            }),
                            None => None,
                        },
                        build: match build {
                            Some(build) => Some(recipe::RecipeCodeBlock {
                                lang: build.0,
                                code: build.1,
                            }),
                            None => None,
                        },
                        install: match install {
                            Some(install) => Some(recipe::RecipeCodeBlock {
                                lang: install.0,
                                code: install.1,
                            }),
                            None => None,
                        },
                    })
                }
                "package" => {
                    let configure = try_consume_field!(&mut consumable_fields, "configure", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));
                    let build = try_consume_field!(&mut consumable_fields, "build", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));
                    let install = try_consume_field!(&mut consumable_fields, "install", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));

                    recipe::Kind::Package(recipe::RecipeCommon {
                        configure: match configure {
                            Some(configure) => Some(recipe::RecipeCodeBlock {
                                lang: configure.0,
                                code: configure.1,
                            }),
                            None => None,
                        },
                        build: match build {
                            Some(build) => Some(recipe::RecipeCodeBlock {
                                lang: build.0,
                                code: build.1,
                            }),
                            None => None,
                        },
                        install: match install {
                            Some(install) => Some(recipe::RecipeCodeBlock {
                                lang: install.0,
                                code: install.1,
                            }),
                            None => None,
                        },
                    })
                }
                "tool" => {
                    let configure = try_consume_field!(&mut consumable_fields, "configure", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));
                    let build = try_consume_field!(&mut consumable_fields, "build", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));
                    let install = try_consume_field!(&mut consumable_fields, "install", ConfigFragment::CodeBlock(lang, code) => (lang.to_string(), code.to_string()));

                    recipe::Kind::Tool(recipe::RecipeCommon {
                        configure: match configure {
                            Some(configure) => Some(recipe::RecipeCodeBlock {
                                lang: configure.0,
                                code: configure.1,
                            }),
                            None => None,
                        },
                        build: match build {
                            Some(build) => Some(recipe::RecipeCodeBlock {
                                lang: build.0,
                                code: build.1,
                            }),
                            None => None,
                        },
                        install: match install {
                            Some(install) => Some(recipe::RecipeCodeBlock {
                                lang: install.0,
                                code: install.1,
                            }),
                            None => None,
                        },
                    })
                }
                namespace => bail!("Invalid namespace \"{}\"", namespace),
            },
        };

        *id_counter += 1;

        for field in consumable_fields {
            if field.1.1 {
                continue;
            }
            bail!("Unknown field `{}`", field.0);
        }

        recipes_deps.push((recipe, deps));
    }
    return Ok(recipes_deps);
}

macro_rules! expect {
    ($vec:expr, $pat:pat => $val:expr) => {
        match $vec.pop() {
            Some($pat) => $val,
            Some(frag) => bail!("Unexpected token `{}`", frag),
            None => bail!("Unexpected EOF"),
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

fn parse_definition(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    let recipe_ref = parse_recipe_ref(tokens)?;
    let obj = parse_object(tokens)?;
    Ok(ConfigFragment::Definition(Box::new(recipe_ref), Box::new(obj)))
}

fn parse_directive(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    expect!(tokens, Token::Symbol('@') => ());
    let name = expect!(tokens, Token::Identifier(v) => v);
    Ok(ConfigFragment::Directive(name, Box::new(parse_value(tokens)?)))
}

fn parse_value(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    let value = match tokens.last() {
        Some(Token::Symbol('[')) => parse_list(tokens),
        Some(Token::Symbol('{')) => parse_object(tokens),
        Some(Token::Symbol('*')) => parse_unary(tokens),
        Some(Token::String(_)) => parse_string(tokens),
        Some(Token::Identifier(_)) => parse_recipe_ref(tokens),
        Some(Token::CodeBlock(_, _)) => parse_code_block(tokens),
        Some(token) => bail!("Unexpected token `{}`", token),
        None => bail!("Unexpected EOF"),
    };
    return value;
}

fn parse_object(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    expect!(tokens, Token::Symbol('{') => ());

    let mut values = HashMap::<String, Box<ConfigFragment>>::new();
    while try_expect!(tokens, Token::Symbol('}') => ()).is_none() {
        let key = expect!(tokens, Token::Identifier(v) => v);
        expect!(tokens, Token::Symbol(':') => ());

        if values.insert(key.clone(), Box::new(parse_value(tokens)?)).is_some() {
            if key != "dependencies" {
                bail!("Cannot define key twice \"{}\"", key)
            }
        }

        try_expect!(tokens, Token::Symbol(',') => ());
    }

    Ok(ConfigFragment::Object(values))
}

fn parse_unary(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    expect!(tokens, Token::Symbol('*') => ());

    return Ok(ConfigFragment::Unary('*', Box::new(parse_value(tokens)?)));
}

fn parse_string(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    Ok(ConfigFragment::String(expect!(tokens, Token::String(v) => v)))
}

fn parse_list(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    expect!(tokens, Token::Symbol('[') => ());

    let mut values = Vec::<ConfigFragment>::new();
    while try_expect!(tokens, Token::Symbol(']') => ()).is_none() {
        values.push(parse_value(tokens)?);
        try_expect!(tokens, Token::Symbol(',') => ());
    }

    Ok(ConfigFragment::List(values))
}

fn parse_recipe_ref(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    let namespace = expect!(tokens, Token::Identifier(v) => v);
    expect!(tokens, Token::Symbol('/') => ());
    let recipe = expect!(tokens, Token::Identifier(v) => v);

    Ok(ConfigFragment::RecipeRef(namespace, recipe))
}

fn parse_code_block(tokens: &mut Vec<Token>) -> Result<ConfigFragment> {
    let (lang, code) = expect!(tokens, Token::CodeBlock(lang, code) => (lang, code));
    Ok(ConfigFragment::CodeBlock(lang, code))
}
