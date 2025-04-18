use std::{collections::HashMap, fs::read_to_string, ops::Deref, path::PathBuf};

use anyhow::{bail, Context, Result};
use parser::{parse_config, ConfigFragment};

use crate::recipe::{self, Recipe, RecipeDependency, RecipeId};

mod lexer;
mod parser;

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
            None => bail!("Missing field `{}`", $field),
            Some(value) => value
        }
    };
}

pub fn parse(path: PathBuf) -> Result<(HashMap<u32, Recipe>, HashMap<u32, Vec<RecipeDependency>>)> {
    let mut id_counter = 0_u32;
    let recipes_deps = parse_file(path, &mut id_counter)?;

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
    let data: String = read_to_string(&path).context("Config read failed")?;

    let tokens = &mut lexer::tokenize(data.as_str())?;

    let mut definitions: Vec<ConfigFragment> = Vec::new();
    let mut directives: Vec<ConfigFragment> = Vec::new();
    for frag in parse_config(tokens)? {
        match frag {
            ConfigFragment::Directive { name: _, value: _ } => directives.push(frag),
            frag => definitions.push(expect_frag!(frag, ConfigFragment::Definition { key: _, value: _ } => frag)),
        }
    }

    let mut recipes_deps: Vec<(Recipe, Vec<(String, String, bool)>)> = Vec::new();
    for directive in directives.iter() {
        let (name, value) = expect_frag!(directive, ConfigFragment::Directive{name, value} => (name, value));

        if name == "import" {
            let value = expect_frag!(value.deref(), ConfigFragment::String(v) => v);

            match path.parent() {
                Some(parent) => {
                    let mut imported_recdeps =
                        parse_file(parent.join(value), id_counter).with_context(|| format!("Failed to import \"{}\"", value))?;
                    recipes_deps.append(&mut imported_recdeps);
                }
                None => bail!("Failed to import \"{}\"", value),
            }
        } else {
            bail!("Unknown directive `{}`", name);
        }
    }

    for definition in definitions.iter() {
        let (key, value) = expect_frag!(definition, ConfigFragment::Definition {key, value} => (key, value));

        let (namespace, name) = expect_frag!(key.as_ref(), ConfigFragment::RecipeRef {namespace, name} => (namespace, name));

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
                        ConfigFragment::Unary { operation: '*', value: frag } => (frag.deref(), true),
                        dep => (dep, false),
                    };

                    let (namespace, name) = expect_frag!(dep, ConfigFragment::RecipeRef {namespace, name} => (namespace, name));
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
                    let source_type = consume_field!(&mut consumable_fields, "type", ConfigFragment::String(v) => v.to_string());
                    let patch = try_consume_field!(&mut consumable_fields, "patch", ConfigFragment::String(v) => v.to_string());
                    let regenerate = try_consume_field!(&mut consumable_fields, "regenerate", ConfigFragment::CodeBlock {lang, code} => (lang.to_string(), code.to_string()));

                    let kind = match source_type.as_str() {
                        "local" => recipe::SourceKind::Local,
                        "git" => recipe::SourceKind::Git(consume_field!(&mut consumable_fields, "ref", ConfigFragment::String(v) => v.to_string())),
                        "tar.gz" => {
                            recipe::SourceKind::TarGz(consume_field!(&mut consumable_fields, "b2sum", ConfigFragment::String(v) => v.to_string()))
                        }
                        "tar.xz" => {
                            recipe::SourceKind::TarXz(consume_field!(&mut consumable_fields, "b2sum", ConfigFragment::String(v) => v.to_string()))
                        }
                        v => bail!("Unknown source type `{}`", v),
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
                "bare" | "package" | "tool" => {
                    let configure = try_consume_field!(&mut consumable_fields, "configure", ConfigFragment::CodeBlock {lang, code} => recipe::RecipeCodeBlock {lang: lang.to_string(), code: code.to_string()});
                    let build = try_consume_field!(&mut consumable_fields, "build", ConfigFragment::CodeBlock {lang, code} => recipe::RecipeCodeBlock {lang: lang.to_string(), code: code.to_string()});
                    let install = try_consume_field!(&mut consumable_fields, "install", ConfigFragment::CodeBlock {lang, code} => recipe::RecipeCodeBlock {lang: lang.to_string(), code: code.to_string()});

                    match namespace.as_str() {
                        "bare" => recipe::Kind::Bare(recipe::RecipeCommon { configure, build, install }),
                        "package" => recipe::Kind::Package(recipe::RecipeCommon { configure, build, install }),
                        "tool" => recipe::Kind::Tool(recipe::RecipeCommon { configure, build, install }),
                        _ => bail!("Invalid namespace `{}`", namespace),
                    }
                }
                namespace => bail!("Invalid namespace `{}`", namespace),
            },
        };

        *id_counter += 1;

        for field in consumable_fields {
            if field.1 .1 {
                continue;
            }
            bail!("Unknown field `{}`", field.0);
        }

        recipes_deps.push((recipe, deps));
    }
    return Ok(recipes_deps);
}
