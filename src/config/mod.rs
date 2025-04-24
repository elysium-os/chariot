use std::{collections::HashMap, fmt::Display, fs::read_to_string, ops::Deref, path::Path, rc::Rc};

use anyhow::{Context, Result, bail};
use parser::{ConfigFragment, parse_config};

mod lexer;
mod parser;

pub type ConfigRecipeId = u32;

pub enum ConfigNamespace {
    Source(ConfigRecipeSource),
    Custom(ConfigRecipeCommon),
    Package(ConfigRecipeCommon),
    Tool(ConfigRecipeCommon),
}

pub struct ConfigRecipe {
    pub id: ConfigRecipeId,

    pub namespace: ConfigNamespace,
    pub name: String,

    pub used_options: Vec<String>,
    pub image_dependencies: Vec<String>,
}

pub enum ConfigSourceKind {
    Local,
    Git(String),
    TarGz(String),
    TarXz(String),
}

pub struct ConfigRecipeSource {
    pub url: String,
    pub patch: Option<String>,
    pub kind: ConfigSourceKind,
    pub regenerate: Option<ConfigCodeBlock>,
}

pub struct ConfigRecipeCommon {
    pub configure: Option<ConfigCodeBlock>,
    pub build: Option<ConfigCodeBlock>,
    pub install: Option<ConfigCodeBlock>,
}

pub struct ConfigRecipeDependency {
    pub recipe_id: ConfigRecipeId,
    pub runtime: bool,
    pub mutable: bool,
}

pub struct ConfigCodeBlock {
    pub lang: String,
    pub code: String,
}

pub struct Config {
    pub global_env: HashMap<String, String>,
    pub recipes: HashMap<ConfigRecipeId, ConfigRecipe>,
    pub dependency_map: HashMap<ConfigRecipeId, Vec<ConfigRecipeDependency>>,
    pub collections: HashMap<String, Vec<ConfigRecipeId>>,
    pub options: HashMap<String, Vec<String>>,
    pub global_pkgs: Vec<String>,
}

impl Display for ConfigRecipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.namespace, self.name)
    }
}

impl Display for ConfigNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let namespace = match &self {
            ConfigNamespace::Source(_) => "source",
            ConfigNamespace::Package(_) => "package",
            ConfigNamespace::Tool(_) => "tool",
            ConfigNamespace::Custom(_) => "custom",
        };

        write!(f, "{}", namespace)
    }
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
            None => bail!("Missing field `{}`", $field),
            Some(value) => value
        }
    };
}

impl Config {
    pub fn parse(path: impl AsRef<Path>) -> Result<Rc<Config>> {
        let mut id_counter: ConfigRecipeId = 0;
        let mut global_env: HashMap<String, String> = HashMap::new();
        let mut collections: HashMap<String, Vec<(String, String)>> = HashMap::new();
        let mut options: HashMap<String, Vec<String>> = HashMap::new();
        let mut global_pkgs: Vec<String> = Vec::new();

        let recipes_deps = parse_file(path, &mut id_counter, &mut global_env, &mut collections, &mut options, &mut global_pkgs)?;

        let mut dependency_map: HashMap<ConfigRecipeId, Vec<ConfigRecipeDependency>> = HashMap::new();
        for recipe in recipes_deps.iter() {
            let mut deps: Vec<ConfigRecipeDependency> = Vec::new();

            for dep in recipe.1.iter() {
                let mut found = false;
                for dep_recipe in recipes_deps.iter() {
                    if dep_recipe.0.name != dep.1 {
                        continue;
                    }

                    if !match dep_recipe.0.namespace {
                        ConfigNamespace::Source(_) => dep.0 == "source",
                        ConfigNamespace::Custom(_) => dep.0 == "custom",
                        ConfigNamespace::Package(_) => dep.0 == "package",
                        ConfigNamespace::Tool(_) => dep.0 == "tool",
                    } {
                        continue;
                    }

                    if dep.3 && !matches!(dep_recipe.0.namespace, ConfigNamespace::Source(_)) {
                        bail!("Mutable flag only valid for sources, used on non-source in recipe `{}`", recipe.0);
                    }

                    deps.push(ConfigRecipeDependency {
                        recipe_id: dep_recipe.0.id,
                        runtime: dep.2,
                        mutable: dep.3,
                    });
                    found = true;
                    break;
                }
                if !found {
                    bail!("Unknown dependency `{}/{}`", dep.0, dep.1);
                }
            }

            dependency_map.insert(recipe.0.id, deps);
        }

        for option in &options {
            for ch in option.0.chars() {
                if !ch.is_alphanumeric() {
                    bail!("Option `{}` is not alphanumeric", option.0);
                }
            }
        }

        let mut recipes: HashMap<ConfigRecipeId, ConfigRecipe> = HashMap::new();
        for recipe in recipes_deps.into_iter() {
            for option in recipe.0.used_options.iter() {
                if !options.contains_key(option) {
                    bail!("Recipe `{}` uses unknown option `{}`", recipe.0, option);
                }
            }

            for recipe_other in recipes.iter() {
                if recipe_other.1.namespace.to_string() != recipe.0.namespace.to_string() {
                    continue;
                }

                if recipe_other.1.name != recipe.0.name {
                    continue;
                }

                bail!("Recipe `{}` defined more than once", recipe.0.name);
            }

            recipes.insert(recipe.0.id, recipe.0);
        }

        let mut resolved_collections: HashMap<String, Vec<ConfigRecipeId>> = HashMap::new();
        for collection in collections {
            let mut resolved_recipes: Vec<ConfigRecipeId> = Vec::new();
            for value in collection.1 {
                let mut resolved_recipe: Option<ConfigRecipeId> = None;
                for recipe in recipes.values() {
                    if recipe.namespace.to_string() != value.0 {
                        continue;
                    }

                    if recipe.name != value.1 {
                        continue;
                    }

                    resolved_recipe = Some(recipe.id);
                }
                match resolved_recipe {
                    Some(id) => resolved_recipes.push(id),
                    None => bail!("Unknown recipe `{}/{}` in collection `{}`", value.0, value.1, collection.0),
                }
            }
            resolved_collections.insert(collection.0, resolved_recipes);
        }

        Ok(Rc::new(Config {
            global_env,
            recipes,
            dependency_map,
            collections: resolved_collections,
            options,
            global_pkgs,
        }))
    }
}

fn parse_file(
    path: impl AsRef<Path>,
    id_counter: &mut ConfigRecipeId,
    global_env: &mut HashMap<String, String>,
    collections: &mut HashMap<String, Vec<(String, String)>>,
    options: &mut HashMap<String, Vec<String>>,
    global_pkgs: &mut Vec<String>,
) -> Result<Vec<(ConfigRecipe, Vec<(String, String, bool, bool)>)>> {
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

    let mut recipes_deps: Vec<(ConfigRecipe, Vec<(String, String, bool, bool)>)> = Vec::new();
    for directive in directives.iter() {
        let (name, value) = expect_frag!(directive, ConfigFragment::Directive{name, value} => (name, value));

        match name.as_str() {
            "import" => {
                let value = expect_frag!(value.deref(), ConfigFragment::String(v) => v);

                match path.as_ref().parent() {
                    Some(parent) => {
                        let mut imported_recdeps = parse_file(parent.join(value), id_counter, global_env, collections, options, global_pkgs)
                            .with_context(|| format!("Failed to import \"{}\"", value))?;
                        recipes_deps.append(&mut imported_recdeps);
                    }
                    None => bail!("Failed to import \"{}\"", value),
                }
            }
            "env" => {
                let (op, left, right) = expect_frag!(value.deref(), ConfigFragment::Binary {operation, left, right} => (operation, left, right));
                if *op != '=' {
                    bail!("Unexpected binary operation `{}` in env directive", op);
                }

                let key = expect_frag!(left.deref(), ConfigFragment::String(v) => v.to_string());
                let value = expect_frag!(right.deref(), ConfigFragment::String(v) => v.to_string());
                global_env.insert(key, value);
            }
            "collection" => {
                let (op, left, right) = expect_frag!(value.deref(), ConfigFragment::Binary {operation, left, right} => (operation, left, right));
                if *op != '=' {
                    bail!("Unexpected binary operation `{}` in collection directive", op);
                }

                let mut values: Vec<(String, String)> = Vec::new();
                for value in expect_frag!(right.deref(), ConfigFragment::List(v) => v) {
                    values.push(expect_frag!(value, ConfigFragment::RecipeRef { namespace, name } => (namespace.to_string(), name.to_string())));
                }
                collections.insert(expect_frag!(left.deref(), ConfigFragment::String(v) => v.to_string()), values);
            }
            "option" => {
                let (op, left, right) = expect_frag!(value.deref(), ConfigFragment::Binary {operation, left, right} => (operation, left, right));
                if *op != '=' {
                    bail!("Unexpected binary operation `{}` in option directive", op);
                }

                let mut allowed_values: Vec<String> = Vec::new();
                for value in expect_frag!(right.deref(), ConfigFragment::List(v) => v) {
                    allowed_values.push(expect_frag!(value, ConfigFragment::String(v) => v.to_string()));
                }

                let key = expect_frag!(left.deref(), ConfigFragment::String(v) => v.to_string());
                if options.contains_key(&key) {
                    bail!("Option `{}` defined more than once", key);
                }
                options.insert(key, allowed_values);
            }
            "global_pkg" => {
                let pkgs = match value.deref() {
                    ConfigFragment::String(pkg) => vec![pkg],
                    ConfigFragment::List(pkgs) => {
                        let mut vec = Vec::new();
                        for pkg in pkgs {
                            vec.push(expect_frag!(pkg, ConfigFragment::String(v) => v));
                        }
                        vec
                    }
                    frag => bail!("Invalid frag `{}` passed to global_pkg", frag),
                };

                for pkg in pkgs {
                    if global_pkgs.contains(pkg) {
                        bail!("Global package `{}` declared more than once", pkg);
                    }
                    global_pkgs.push(pkg.clone());
                }
            }
            _ => bail!("Unknown directive `{}`", name),
        }
    }

    for definition in definitions.iter() {
        let (key, value) = expect_frag!(definition, ConfigFragment::Definition {key, value} => (key, value));

        let (namespace, name) = expect_frag!(key.as_ref(), ConfigFragment::RecipeRef {namespace, name} => (namespace, name));

        let mut consumable_fields: HashMap<&String, (&Box<ConfigFragment>, bool)> = HashMap::new();
        for field in expect_frag!(value.as_ref(), ConfigFragment::Object(fields) => fields) {
            consumable_fields.insert(field.0, (field.1, false));
        }

        let mut deps: Vec<(String, String, bool, bool)> = Vec::new();
        let mut image_deps: Vec<String> = Vec::new();

        match try_consume_field!(&mut consumable_fields, "dependencies", ConfigFragment::List(v) => v) {
            Some(recipe_deps) => {
                for dependency in recipe_deps {
                    let mut runtime = false;
                    let mut mutable = false;

                    let mut dep = dependency;
                    loop {
                        dep = match dep {
                            ConfigFragment::Unary { operation: '*', value: frag } => {
                                if runtime {
                                    bail!("Unary `*` defined more than once for dependency in recipe `{}/{}`", namespace, name)
                                }
                                runtime = true;
                                frag.deref()
                            }
                            ConfigFragment::Unary { operation: '%', value: frag } => {
                                if mutable {
                                    bail!("Unary `%` defined more than once for dependency in recipe `{}/{}`", namespace, name)
                                }
                                mutable = true;
                                frag.deref()
                            }
                            _ => break,
                        };
                    }

                    let (namespace, name) = expect_frag!(dep, ConfigFragment::RecipeRef {namespace, name} => (namespace, name));
                    if namespace == "image" {
                        image_deps.push(name.clone());
                    } else {
                        deps.push((namespace.clone(), name.clone(), runtime, mutable));
                    }
                }
            }
            None => {}
        }

        let mut used_options: Vec<String> = Vec::new();
        if let Some(options) = try_consume_field!(&mut consumable_fields, "options", ConfigFragment::List(v) => v) {
            for option in options {
                let option = expect_frag!(option, ConfigFragment::String(v) => v.to_string());
                if used_options.contains(&option) {
                    bail!("Recipe `{}` uses option `{}` more than once", namespace, name);
                }
                used_options.push(option);
            }
        }

        let recipe = ConfigRecipe {
            id: *id_counter,
            name: name.clone(),
            image_dependencies: image_deps,
            used_options,
            namespace: match namespace.as_str() {
                "source" => {
                    let url = consume_field!(&mut consumable_fields, "url", ConfigFragment::String(v) => v.to_string());
                    let source_type = consume_field!(&mut consumable_fields, "type", ConfigFragment::String(v) => v.to_string());
                    let patch = try_consume_field!(&mut consumable_fields, "patch", ConfigFragment::String(v) => v.to_string());
                    let regenerate = try_consume_field!(&mut consumable_fields, "regenerate", ConfigFragment::CodeBlock {lang, code} => (lang.to_string(), code.to_string()));

                    let kind = match source_type.as_str() {
                        "local" => ConfigSourceKind::Local,
                        "git" => ConfigSourceKind::Git(consume_field!(&mut consumable_fields, "revision", ConfigFragment::String(v) => v.to_string())),
                        "tar.gz" => ConfigSourceKind::TarGz(consume_field!(&mut consumable_fields, "b2sum", ConfigFragment::String(v) => v.to_string())),
                        "tar.xz" => ConfigSourceKind::TarXz(consume_field!(&mut consumable_fields, "b2sum", ConfigFragment::String(v) => v.to_string())),
                        v => bail!("Unknown source type `{}`", v),
                    };

                    ConfigNamespace::Source(ConfigRecipeSource {
                        url,
                        kind,
                        patch,
                        regenerate: match regenerate {
                            None => None,
                            Some(regenerate) => Some(ConfigCodeBlock {
                                lang: regenerate.0,
                                code: regenerate.1,
                            }),
                        },
                    })
                }
                "package" | "tool" | "custom" => {
                    let configure = try_consume_field!(&mut consumable_fields, "configure", ConfigFragment::CodeBlock {lang, code} => ConfigCodeBlock {lang: lang.to_string(), code: code.to_string()});
                    let build = try_consume_field!(&mut consumable_fields, "build", ConfigFragment::CodeBlock {lang, code} => ConfigCodeBlock {lang: lang.to_string(), code: code.to_string()});
                    let install = try_consume_field!(&mut consumable_fields, "install", ConfigFragment::CodeBlock {lang, code} => ConfigCodeBlock {lang: lang.to_string(), code: code.to_string()});

                    match namespace.as_str() {
                        "package" => ConfigNamespace::Package(ConfigRecipeCommon { configure, build, install }),
                        "tool" => ConfigNamespace::Tool(ConfigRecipeCommon { configure, build, install }),
                        "custom" => ConfigNamespace::Custom(ConfigRecipeCommon { configure, build, install }),
                        _ => bail!("Invalid namespace `{}`", namespace),
                    }
                }
                namespace => bail!("Invalid namespace `{}`", namespace),
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
