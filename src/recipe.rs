use anyhow::{Context, Result};
use std::{
    fmt::Display,
    fs::{exists, read_to_string, write},
    path::{Path, PathBuf},
};

use crate::util::get_timestamp;

pub type RecipeId = u32;

pub enum Kind {
    Source(RecipeSource),
    Custom(RecipeCommon),
    Package(RecipeCommon),
    Tool(RecipeCommon),
    Collection,
}

pub enum SourceKind {
    Local,
    Git(String),
    TarGz(String),
    TarXz(String),
}

pub struct Recipe {
    pub id: RecipeId,
    pub kind: Kind,
    pub name: String,
    pub image_dependencies: Vec<String>,
    pub mutable_sources: bool,
}

pub struct RecipeSource {
    pub url: String,
    pub patch: Option<String>,
    pub kind: SourceKind,
    pub regenerate: Option<RecipeCodeBlock>,
}

pub struct RecipeCommon {
    pub configure: Option<RecipeCodeBlock>,
    pub build: Option<RecipeCodeBlock>,
    pub install: Option<RecipeCodeBlock>,
}

pub struct RecipeCodeBlock {
    pub lang: String,
    pub code: String,
}

pub struct RecipeDependency {
    pub recipe_id: RecipeId,
    pub runtime: bool,
}

pub struct RecipeState {
    pub intact: bool,
    pub invalidated: bool,
    pub timestamp: u64,
}

impl PartialEq for Recipe {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Display for Recipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.namespace_string(), self.name)
    }
}

impl Recipe {
    pub fn namespace_string(&self) -> &str {
        match self.kind {
            Kind::Custom(_) => "custom",
            Kind::Source(_) => "source",
            Kind::Package(_) => "package",
            Kind::Tool(_) => "tool",
            Kind::Collection => "collection",
        }
    }

    pub fn path(&self, recipes_path: &Path) -> PathBuf {
        recipes_path.join(self.namespace_string()).join(self.name.as_str())
    }

    fn state_path(&self, recipes_path: &Path) -> PathBuf {
        self.path(recipes_path).join("state.toml")
    }

    pub fn state_invalidate(&self, recipes_path: &Path) -> Result<()> {
        let mut new_state = RecipeState {
            intact: false,
            invalidated: true,
            timestamp: get_timestamp()?,
        };
        if let Some(state) = self.state_parse(recipes_path)? {
            new_state.intact = state.intact;
        }
        self.state_write(recipes_path, new_state)?;
        Ok(())
    }

    pub fn state_parse(&self, recipes_path: &Path) -> Result<Option<RecipeState>> {
        let path = self.state_path(recipes_path);
        if !exists(&path)? {
            return Ok(None);
        }

        let data = read_to_string(&path).context("Failed to read recipe state")?;
        let table = data.parse::<toml::Table>().context("Failed to parse recipe state")?;
        let intact = table["intact"].as_bool().unwrap_or(false);
        let invalidated = table["invalidated"].as_bool().unwrap_or(false);
        let timestamp = table["timestamp"].as_integer().unwrap_or(0) as u64;
        Ok(Some(RecipeState {
            intact,
            invalidated,
            timestamp,
        }))
    }

    pub fn state_write(&self, recipes_path: &Path, state: RecipeState) -> Result<()> {
        let path = self.state_path(recipes_path);

        let mut state_table = toml::Table::new();
        state_table.insert(String::from("intact"), toml::Value::Boolean(state.intact));
        state_table.insert(String::from("invalidated"), toml::Value::Boolean(state.invalidated));
        state_table.insert(String::from("timestamp"), toml::Value::Integer(state.timestamp as i64));
        write(&path, toml::to_string(&state_table).context("Failed to serialize recipe state")?).context("Failed to write recipe state")?;
        Ok(())
    }
}
