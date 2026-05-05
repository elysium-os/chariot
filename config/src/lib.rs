use std::{
    collections::{BTreeSet, HashMap},
    fmt::{self, Display, Formatter},
    fs::read_to_string,
    path::{Path, PathBuf},
};

use ariadne::Report;
use chariot_orchestrator::recipe::{Collection, Recipe, RecipeProvider, RecipeTag};
use chariot_util::fs::FileSystemError;
use glob::glob;
use logos::{Logos, Span};
use thiserror::Error;

use crate::{lexer::Token, parser::parse};

mod lexer;
mod parser;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error("Glob error")]
    Glob(#[source] glob::GlobError),

    #[error("Invalid glob pattern")]
    GlobPattern(#[source] glob::PatternError),

    #[error("Failed to resolve directory of config path `{}`", .0.display())]
    ResolveDir(PathBuf),

    #[error("Failed to resolve file of config path `{}`", .0.display())]
    ResolveFile(PathBuf),
}

#[derive(Debug)]
pub struct Config {
    pub options: HashMap<String, BTreeSet<String>>,

    pub global_environment: HashMap<String, String>,
    pub global_packages: Vec<String>,

    pub rootfs_version: Option<String>,
    pub rootfs_hash: Option<String>,
    pub rootfs_origin: Option<String>,

    pub collections: Vec<Collection>,
    pub recipes: Vec<Recipe>,
}

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct ConfigFile {
    pub path: PathBuf,
}

impl RecipeProvider for Config {
    fn query_recipe(&self, tag: &RecipeTag) -> Option<&Recipe> {
        self.recipes.iter().find(|r| tag.kind == r.kind && r.name == tag.name)
    }
}

impl Display for ConfigFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path.display())
    }
}

pub fn parse_file(path: impl AsRef<Path>) -> Result<(Result<Config, Report<'static, (ConfigFile, Span)>>, HashMap<ConfigFile, String>), ConfigError> {
    let config_dir = match path.as_ref().parent() {
        None => return Err(ConfigError::ResolveDir(path.as_ref().to_path_buf())),
        Some(dir) => dir,
    };

    let config_file_path = match path.as_ref().file_name() {
        None => return Err(ConfigError::ResolveFile(path.as_ref().to_path_buf())),
        Some(file) => file,
    };

    let config_data = read_to_string(&path).map_err(|err| FileSystemError::ReadFile {
        path: path.as_ref().to_path_buf(),
        source: err,
    })?;

    let config_file = ConfigFile {
        path: PathBuf::from(config_file_path),
    };

    let result = parse(&config_file, &mut Token::lexer(&config_data).spanned().peekable());
    let mut file_cache = HashMap::from([(config_file, config_data)]);

    let (imports, mut config) = match result {
        Err(report) => return Ok((Err(report), file_cache)),
        Ok(result) => result,
    };

    for import in imports
        .iter()
        .map(|import| glob(import).map_err(|err| ConfigError::GlobPattern(err)))
        .collect::<Result<Vec<_>, ConfigError>>()?
        .into_iter()
        .flatten()
        .map(|path| path.map_err(|err| ConfigError::Glob(err)))
        .collect::<Result<Vec<PathBuf>, ConfigError>>()?
    {
        let (result, imported_file_cache) = parse_file(config_dir.join(import))?;

        for (config_file, data) in imported_file_cache {
            file_cache.insert(config_file, data);
        }

        let mut imported_config = match result {
            Err(report) => return Ok((Err(report), file_cache)),
            Ok(config) => config,
        };

        config.options.extend(imported_config.options.drain());
        config.global_environment.extend(imported_config.global_environment.drain());
        config.global_packages.append(&mut imported_config.global_packages);
        if imported_config.rootfs_version.is_some() {
            config.rootfs_version = imported_config.rootfs_version.take();
        }
        if imported_config.rootfs_hash.is_some() {
            config.rootfs_hash = imported_config.rootfs_hash.take();
        }
        if imported_config.rootfs_origin.is_some() {
            config.rootfs_origin = imported_config.rootfs_origin.take();
        }
        config.collections.append(&mut imported_config.collections);
        config.recipes.append(&mut imported_config.recipes);
    }

    Ok((Ok(config), file_cache))
}
