use std::{
    collections::{BTreeSet, HashMap},
    iter::Peekable,
};

use ariadne::{Color, Label, Report, ReportKind};
use chariot_orchestrator::{
    Script, ScriptLanguage,
    package::{Package, PackageFlags, PackagePlatform},
    recipe::{Collection, Dependency, Recipe, RecipeKind, RecipeKindTag, RecipeTag},
    source::{Source, SourceArchiveKind, SourceKind},
};
use logos::{Span, SpannedIter};

use crate::{
    Config, ConfigFile,
    lexer::{Block, NamespacedIdentifier, Token, TokenTag},
};

type Lexer<'a> = Peekable<SpannedIter<'a, Token<'a>>>;

enum Value {
    String(String),
    Boolean(bool),
    Script(Script),
    Dependency(Dependency),
    List(Vec<(Value, Span)>),
}

fn construct_error_report(file: &ConfigFile, span: &Span, message: &str, label_message: Option<&str>) -> Report<'static, (ConfigFile, Span)> {
    let report = Report::build(ReportKind::Error, (file.clone(), span.clone())).with_message(message);

    let mut label = Label::new((file.clone(), span.clone())).with_color(Color::Red);
    if let Some(label_message) = label_message {
        label = label.with_message(label_message);
    }

    report.with_label(label).finish()
}

fn report_expected(file: &ConfigFile, span: &Span, expected: &'static str) -> Report<'static, (ConfigFile, Span)> {
    construct_error_report(file, &span, "unexpected token", Some(&format!("expected {}", expected)))
}

fn report_unexpected_eof(file: &ConfigFile) -> Report<'static, (ConfigFile, Span)> {
    construct_error_report(file, &(0..0), "unexpected end of file", None)
}

fn report_unknown_token(file: &ConfigFile, span: &Span) -> Report<'static, (ConfigFile, Span)> {
    construct_error_report(file, span, "unknown token", None)
}

fn peek<'a>(file: &ConfigFile, lexer: &mut Lexer<'a>) -> Result<Option<(TokenTag, Span)>, Report<'static, (ConfigFile, Span)>> {
    match lexer.peek() {
        Some((Err(()), span)) => Err(report_unknown_token(file, span)),
        Some((Ok(token), span)) => Ok(Some((token.tag(), span.clone()))),
        None => Ok(None),
    }
}

fn advance<'a>(file: &ConfigFile, lexer: &mut Lexer<'a>) -> Result<Option<(Token<'a>, Span)>, Report<'static, (ConfigFile, Span)>> {
    match lexer.next() {
        Some((Err(()), span)) => Err(report_unknown_token(file, &span)),
        Some((Ok(token), span)) => Ok(Some((token, span))),
        None => Ok(None),
    }
}

fn expect<'a>(file: &ConfigFile, lexer: &mut Lexer<'a>, expected: TokenTag) -> Result<Span, Report<'static, (ConfigFile, Span)>> {
    match advance(file, lexer)? {
        Some((token, span)) if token.tag() == expected => Ok(span),
        Some((_, span)) => Err(report_expected(file, &span, expected.as_str())),
        None => Err(report_unexpected_eof(file)),
    }
}

fn if_expect<'a>(file: &ConfigFile, lexer: &mut Lexer<'a>, expected: TokenTag) -> Result<Option<Span>, Report<'static, (ConfigFile, Span)>> {
    match peek(file, lexer)? {
        None => return Err(report_unexpected_eof(file)),
        Some((tag, span)) => {
            if tag == expected {
                lexer.next();
                Ok(Some(span))
            } else {
                Ok(None)
            }
        }
    }
}

pub fn parse<'a>(file: &ConfigFile, lexer: &mut Lexer<'a>) -> Result<(Vec<String>, Config), Report<'static, (ConfigFile, Span)>> {
    let mut imports = Vec::new();

    let mut options = HashMap::new();

    let mut global_environment = HashMap::new();
    let mut global_packages = Vec::new();

    let mut rootfs_version = None;
    let mut rootfs_hash = None;
    let mut rootfs_origin = None;

    let mut recipes = Vec::new();
    let mut collections = Vec::new();

    while let Some((token, span)) = advance(file, lexer)? {
        match token {
            Token::Directive(directive) => match directive {
                "import" => match parse_value(file, lexer)? {
                    (Value::String(value), _) => imports.push(value),
                    (_, span) => return Err(report_expected(file, &span, "string")),
                },
                "rootfs_version" => match parse_value(file, lexer)? {
                    (Value::String(value), _) => rootfs_version = Some(value),
                    (_, span) => return Err(report_expected(file, &span, "string")),
                },
                "rootfs_hash" => match parse_value(file, lexer)? {
                    (Value::String(value), _) => rootfs_hash = Some(value),
                    (_, span) => return Err(report_expected(file, &span, "string")),
                },
                "rootfs_origin" => match parse_value(file, lexer)? {
                    (Value::String(value), _) => rootfs_origin = Some(value),
                    (_, span) => return Err(report_expected(file, &span, "string")),
                },
                "option" => {
                    let key = match parse_value(file, lexer)? {
                        (Value::String(value), _) => value,
                        (_, span) => return Err(report_expected(file, &span, "string")),
                    };

                    expect(file, lexer, TokenTag::SymbolEqual)?;

                    let values = match parse_value(file, lexer)? {
                        (Value::List(list), _) => {
                            let mut values = BTreeSet::new();
                            for value in list {
                                match value {
                                    (Value::String(value), _) => {
                                        values.insert(value);
                                    }
                                    (_, span) => return Err(report_expected(file, &span, "string")),
                                }
                            }
                            values
                        }
                        (_, span) => return Err(report_expected(file, &span, "string")),
                    };

                    options.insert(key, values);
                }
                "global_env" => {
                    let key = match parse_value(file, lexer)? {
                        (Value::String(value), _) => value,
                        (_, span) => return Err(report_expected(file, &span, "string")),
                    };

                    expect(file, lexer, TokenTag::SymbolEqual)?;

                    let value = match parse_value(file, lexer)? {
                        (Value::String(value), _) => value,
                        (_, span) => return Err(report_expected(file, &span, "string")),
                    };

                    global_environment.insert(key, value);
                }
                "global_pkg" => match parse_value(file, lexer)? {
                    (Value::String(value), _) => global_packages.push(value),
                    (Value::List(list), _) => {
                        for value in list {
                            match value {
                                (Value::String(value), _) => global_packages.push(value),
                                (_, span) => return Err(report_expected(file, &span, "string")),
                            }
                        }
                    }
                    (_, span) => return Err(report_expected(file, &span, "string")),
                },
                _ => {
                    return Err(construct_error_report(
                        file,
                        &span,
                        format!("unknown directive `{}`", directive).as_str(),
                        Some("expected import or global_env"),
                    ));
                }
            },
            Token::NamespacedIdentifier(NamespacedIdentifier { namespace, identifier }) => {
                let kind = match namespace {
                    "collection" => {
                        collections.push(Collection {
                            name: identifier.to_string(),
                            dependencies: {
                                let (list, _) = parse_list(file, lexer)?;

                                let mut dependencies = Vec::new();
                                for (value, value_span) in list {
                                    match value {
                                        Value::Dependency(dependency) => dependencies.push(dependency),
                                        _ => return Err(report_expected(file, &value_span, "dependency")),
                                    }
                                }

                                dependencies
                            },
                        });
                        continue;
                    }
                    "source" => RecipeKindTag::Source,
                    "host" => RecipeKindTag::HostPackage,
                    "target" => RecipeKindTag::TargetPackage,
                    _ => {
                        return Err(construct_error_report(
                            file,
                            &span,
                            "invalid namespace",
                            Some("expected collection, source, host, or target"),
                        ));
                    }
                };

                let mut fields = parse_recipe_body(file, lexer)?;

                let mut find_field = |search_key: &'static str| -> Option<(Value, Span)> {
                    let values = fields.extract_if(.., |((key, _), _)| *key == search_key).collect::<Vec<_>>();
                    return values.into_iter().next().and_then(|(_, v)| Some(v));
                };

                let report_missing_field =
                    |field: &'static str| construct_error_report(file, &span, format!("recipe is missing field `{}`", field).as_str(), None);

                let kind = match kind {
                    RecipeKindTag::Source => {
                        let (source_kind, source_kind_span) = match find_field("type") {
                            None => return Err(report_missing_field("type")),
                            Some((Value::String(value), span)) => (value, span),
                            Some((_, span)) => return Err(report_expected(file, &span, "string")),
                        };

                        let source_kind = match source_kind.as_str() {
                            "local" => SourceKind::Local,
                            "git" => {
                                let (revision, _) = match find_field("revision") {
                                    None => return Err(report_missing_field("revision")),
                                    Some((Value::String(value), span)) => (value, span),
                                    Some((_, span)) => return Err(report_expected(file, &span, "string")),
                                };

                                SourceKind::Git { revision: revision.clone() }
                            }
                            tar_type if tar_type.starts_with("tar.") => {
                                let archive_kind = match tar_type {
                                    "tar.gz" => SourceArchiveKind::TarGz,
                                    "tar.xz" => SourceArchiveKind::TarXz,
                                    "tar.bz2" => SourceArchiveKind::TarBz2,
                                    _ => {
                                        return Err(construct_error_report(
                                            file,
                                            &source_kind_span,
                                            "unknown tar compression",
                                            Some("expected tar.gz, tar.xz, or tar.bz2"),
                                        ));
                                    }
                                };

                                let (version, _) = match find_field("version") {
                                    None => return Err(report_missing_field("version")),
                                    Some((Value::String(value), span)) => (value, span),
                                    Some((_, span)) => return Err(report_expected(file, &span, "string")),
                                };

                                let (checksum, _) = match find_field("checksum") {
                                    None => return Err(report_missing_field("checksum")),
                                    Some((Value::String(value), span)) => (value, span),
                                    Some((_, span)) => return Err(report_expected(file, &span, "string")),
                                };

                                SourceKind::Archive {
                                    kind: archive_kind,
                                    version: version.clone(),
                                    checksum: checksum.clone(),
                                }
                            }
                            _ => {
                                return Err(construct_error_report(
                                    file,
                                    &source_kind_span,
                                    "unknown source type",
                                    Some("expected local, git, or tar.<compression>"),
                                ));
                            }
                        };

                        let url = match find_field("url") {
                            None => return Err(report_missing_field("url")),
                            Some((Value::String(value), _)) => value,
                            Some((_, span)) => return Err(report_expected(file, &span, "string")),
                        };

                        let prepare = match find_field("prepare") {
                            None => None,
                            Some((Value::Script(script), _)) => Some(script.clone()),
                            Some((_, span)) => return Err(report_expected(file, &span, "script")),
                        };

                        let patches = match find_field("patches") {
                            None => Vec::new(),
                            Some((Value::List(list), _)) => {
                                let mut patches = Vec::new();
                                for (element, element_span) in list {
                                    match element {
                                        Value::String(value) => patches.push(value.clone()),
                                        _ => return Err(report_expected(file, &element_span, "string")),
                                    }
                                }
                                patches
                            }
                            Some((_, span)) => return Err(report_expected(file, &span, "list")),
                        };

                        RecipeKind::Source(Source {
                            kind: source_kind,
                            url: url.clone(),
                            patches,
                            prepare,
                        })
                    }
                    kind @ (RecipeKindTag::HostPackage | RecipeKindTag::TargetPackage) => {
                        let configure = match find_field("configure") {
                            None => None,
                            Some((Value::Script(script), _)) => Some(script.clone()),
                            Some((_, span)) => return Err(report_expected(file, &span, "script")),
                        };

                        let build = match find_field("build") {
                            None => None,
                            Some((Value::Script(script), _)) => Some(script.clone()),
                            Some((_, span)) => return Err(report_expected(file, &span, "script")),
                        };

                        let install = match find_field("install") {
                            None => return Err(report_missing_field("install")),
                            Some((Value::Script(value), _)) => value.clone(),
                            Some((_, span)) => return Err(report_expected(file, &span, "script")),
                        };

                        let always_clean = match find_field("always_clean") {
                            None => false,
                            Some((Value::Boolean(value), _)) => value,
                            Some((_, span)) => return Err(report_expected(file, &span, "boolean")),
                        };

                        RecipeKind::Package(Package {
                            platform: match kind {
                                RecipeKindTag::HostPackage => PackagePlatform::Host,
                                RecipeKindTag::TargetPackage => PackagePlatform::Target,
                                _ => unreachable!(),
                            },
                            configure,
                            build,
                            install,
                            flags: PackageFlags { always_clean },
                        })
                    }
                };

                let subscribed_options = match find_field("options") {
                    None => BTreeSet::new(),
                    Some((Value::List(list), _)) => {
                        let mut options = BTreeSet::new();
                        for (element, element_span) in list {
                            match element {
                                Value::String(value) => {
                                    options.insert(value.clone());
                                }
                                _ => return Err(report_expected(file, &element_span, "string")),
                            }
                        }
                        options
                    }
                    Some((_, span)) => return Err(report_expected(file, &span, "list")),
                };

                let dependencies = match find_field("dependencies") {
                    None => Vec::new(),
                    Some((Value::List(list), _)) => {
                        let mut dependencies: Vec<Dependency> = Vec::new();
                        for (element, element_span) in list {
                            match element {
                                Value::Dependency(value) => dependencies.push(value),
                                _ => return Err(report_expected(file, &element_span, "dependency")),
                            }
                        }
                        dependencies
                    }
                    Some((_, span)) => return Err(report_expected(file, &span, "list")),
                };

                for ((key, key_span), _) in fields {
                    return Err(construct_error_report(
                        file,
                        &key_span,
                        format!("unknown field key `{}`", key).as_str(),
                        None,
                    ));
                }

                recipes.push(Recipe {
                    name: String::from(identifier),
                    kind,
                    subscribed_options,
                    dependencies,
                });
            }
            _ => return Err(report_expected(file, &span, "recipe, collection, or directive")),
        }
    }

    Ok((
        imports,
        Config {
            options,
            global_environment,
            global_packages,
            rootfs_version,
            rootfs_hash,
            rootfs_origin,
            collections,
            recipes,
        },
    ))
}

fn parse_recipe_body<'a>(
    file: &ConfigFile,
    lexer: &mut Lexer<'a>,
) -> Result<Vec<((&'a str, Span), (Value, Span))>, Report<'static, (ConfigFile, Span)>> {
    let mut fields = Vec::new();

    expect(file, lexer, TokenTag::SymbolBraceLeft)?;
    loop {
        match advance(file, lexer)? {
            None => return Err(report_unexpected_eof(file)),
            Some((Token::SymbolBraceRight, _)) => break,
            Some((Token::Identifier(key), key_span)) => {
                expect(file, lexer, TokenTag::SymbolColon)?;
                fields.push(((key, key_span), parse_value(file, lexer)?));
            }
            Some((_, span)) => return Err(report_expected(file, &span, "field key or `}`")),
        }
    }

    Ok(fields)
}

fn parse_list<'a>(file: &ConfigFile, lexer: &mut Lexer<'a>) -> Result<(Vec<(Value, Span)>, Span), Report<'static, (ConfigFile, Span)>> {
    let mut list = Vec::new();

    let start_span = expect(file, lexer, TokenTag::SymbolBracketLeft)?;
    let end_span = loop {
        if let Some(span) = if_expect(file, lexer, TokenTag::SymbolBracketRight)? {
            break span;
        }

        list.push(parse_value(file, lexer)?);
    };

    Ok((list, Span::from(start_span.start..end_span.end)))
}

fn parse_value<'a>(file: &ConfigFile, lexer: &mut Lexer<'a>) -> Result<(Value, Span), Report<'static, (ConfigFile, Span)>> {
    if matches!(peek(file, lexer)?, Some((TokenTag::SymbolBracketLeft, _))) {
        let (list, span) = parse_list(file, lexer)?;
        return Ok((Value::List(list), span));
    }

    let value = match advance(file, lexer)? {
        None => return Err(report_unexpected_eof(file)),
        Some((Token::String(value), span)) => (Value::String(value.to_string()), span),
        Some((Token::Identifier("true"), span)) => (Value::Boolean(true), span),
        Some((Token::Identifier("false"), span)) => (Value::Boolean(false), span),
        Some((Token::Block(Block { tag, content }), span)) => {
            let language = match tag {
                "sh" | "shell" => ScriptLanguage::Shell,
                "py" | "python" => ScriptLanguage::Python,
                _ => {
                    return Err(construct_error_report(
                        file,
                        &span,
                        "unknown script language",
                        Some("expected sh, shell, py, or python"),
                    ));
                }
            };

            (Value::Script(Script::new(language, content)), span)
        }
        Some((Token::NamespacedIdentifier(NamespacedIdentifier { namespace, identifier }), span)) => {
            let kind = match namespace {
                "native" => {
                    return Ok((Value::Dependency(Dependency::Native(identifier.to_string())), span));
                }
                "collection" => {
                    return Ok((Value::Dependency(Dependency::Collection(identifier.to_string())), span));
                }
                "source" => RecipeKindTag::Source,
                "host" => RecipeKindTag::HostPackage,
                "target" => RecipeKindTag::TargetPackage,
                _ => {
                    return Err(construct_error_report(
                        file,
                        &span,
                        "invalid dependency",
                        Some("expected native, collection, source, host, or target"),
                    ));
                }
            };

            (
                Value::Dependency(Dependency::Recipe {
                    recipe: RecipeTag {
                        kind,
                        name: identifier.to_string(),
                    },
                }),
                span,
            )
        }
        Some((_, span)) => return Err(report_expected(file, &span, "string, boolean, script, or list")),
    };

    Ok(value)
}
