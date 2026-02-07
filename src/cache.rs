use std::{
    collections::BTreeMap,
    fs::{create_dir_all, exists, read_dir, read_to_string, write, File},
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use nix::unistd::Pid;

use crate::util::{acquire_lockfile, force_rm};

pub struct Cache {
    path: PathBuf,
    lock: Option<File>,
    proc_lock: Option<File>,
}

const CACHE_VERSION: i64 = 2;

impl Cache {
    pub fn init(path: impl AsRef<Path>, acquire_lock: bool) -> Result<Rc<Cache>> {
        create_dir_all(&path).context("Failed to create cache directory")?;

        let cache_state_path = path.as_ref().join("cache_state.toml");
        if exists(&cache_state_path)? {
            let data = read_to_string(&cache_state_path).context("Failed to read cache state")?;
            let state_table = data.parse::<toml::Table>().context("Failed to parse cache state")?;
            let version = state_table["version"].as_integer().unwrap_or(0);

            if version != CACHE_VERSION {
                bail!(
                    "Cache version mismatch, expected {}, got {}! Please manually delete it and chariot will generate a new one.",
                    CACHE_VERSION,
                    version
                );
            }
        } else {
            let mut state_table = toml::Table::new();
            state_table.insert(String::from("version"), toml::Value::Integer(CACHE_VERSION));

            let cache_state_data = toml::to_string(&state_table).context("Failed to serialize cache state")?;
            write(&cache_state_path, cache_state_data).context("Failed to write cache state")?;
        }

        let mut cache = Cache {
            path: path.as_ref().to_path_buf(),
            lock: None,
            proc_lock: None,
        };

        if acquire_lock {
            cache.lock = Some(acquire_lockfile(cache.path.join("cache.lock")).context("Failed to acquire cache lock")?);
        }

        if exists(cache.path_proc_caches())? {
            for proc_cache in read_dir(cache.path_proc_caches()).context("Failed to read proc caches dir")? {
                let lock_path = proc_cache.as_ref().unwrap().path().join("proc.lock");
                match acquire_lockfile(lock_path) {
                    Ok(lockfile) => FileExt::unlock(&lockfile).context("Failed to release proc lock")?,
                    Err(_) => continue,
                }

                force_rm(proc_cache.as_ref().unwrap().path()).with_context(|| format!("Failed to cleanup proc cache `{}`", proc_cache.unwrap().file_name().to_str().unwrap()))?;
            }
        }

        force_rm(cache.path_proc_cache()).context("Failed to clean to the proc cache")?;
        create_dir_all(cache.path_proc_cache()).context("Failed to create the proc cache")?;

        cache.proc_lock = Some(acquire_lockfile(cache.path_proc_cache().join("proc.lock")).context("Failed to acquire proc lock")?);

        Ok(Rc::new(cache))
    }

    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }

    pub fn path_proc_caches(&self) -> PathBuf {
        self.path.join("proc")
    }

    pub fn path_rootfs(&self) -> PathBuf {
        self.path.join("rootfs")
    }

    pub fn path_recipes(&self) -> PathBuf {
        self.path.join("recipes")
    }

    pub fn path_recipe(&self, namespace: &str, name: &str, options: &BTreeMap<&str, &str>) -> PathBuf {
        let mut recipe_path = self.path_recipes().join(namespace).join(name);
        for (option, value) in options {
            recipe_path = recipe_path.join("opt").join(option).join(value);
        }
        recipe_path
    }

    fn path_proc_cache(&self) -> PathBuf {
        self.path_proc_caches().join(Pid::this().to_string())
    }

    fn path_dependency_cache(&self) -> PathBuf {
        self.path_proc_cache().join("depcache")
    }

    pub fn path_dependency_cache_sources(&self) -> PathBuf {
        self.path_dependency_cache().join("sources")
    }

    pub fn path_dependency_cache_tools(&self) -> PathBuf {
        self.path_dependency_cache().join("tools")
    }

    pub fn path_dependency_cache_packages(&self) -> PathBuf {
        self.path_dependency_cache().join("packages")
    }
}
