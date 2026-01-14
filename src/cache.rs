use std::{
    collections::BTreeMap,
    fs::{create_dir_all, exists, read_dir, File},
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result};
use fs2::FileExt;
use nix::unistd::Pid;

use crate::util::{acquire_lockfile, clean};

pub struct Cache {
    path: PathBuf,
    lock: Option<File>,
    proc_lock: Option<File>,
}

impl Cache {
    pub fn init(path: impl AsRef<Path>, acquire_lock: bool) -> Result<Rc<Cache>> {
        create_dir_all(&path).context("Failed to create cache directory")?;

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

                clean(proc_cache.as_ref().unwrap().path()).with_context(|| format!("Failed to cleanup proc cache `{}`", proc_cache.unwrap().file_name().to_str().unwrap()))?;
            }
        }

        clean(cache.path_proc_cache()).context("Failed to clean to the proc cache")?;
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
