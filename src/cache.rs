use crate::util::clean;
use anyhow::{Context, Result};
use lockfile::Lockfile;
use nix::unistd::Pid;
use std::{
    fs::{create_dir_all, read_dir},
    path::{Path, PathBuf},
    rc::Rc,
};

pub struct Cache {
    path: PathBuf,
    lock: Option<Lockfile>,
    proc_lock: Option<Lockfile>,
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
            cache.lock = Some(Lockfile::create(cache.path.join("cache.lock")).context("Failed to acquire cache lock")?);
        }

        for proc_cache in read_dir(cache.path_proc_caches()).context("Failed to read proc caches dir")? {
            let lock_path = proc_cache.unwrap().path().join("proc.lock");
            let lock = Lockfile::create(lock_path);
            match lock {
                Ok(lock) => lock.release().context("Failed to release proc lock")?,
                Err(_) => continue,
            }
        }

        clean(cache.path_proc()).context("Failed to clean to the proc cache")?;
        create_dir_all(cache.path_proc()).context("Failed to create the proc cache")?;

        cache.proc_lock = Some(Lockfile::create(cache.path_proc().join("proc.lock")).context("Failed to acquire proc lock")?);

        Ok(Rc::new(cache))
    }

    fn path_proc_caches(&self) -> PathBuf {
        self.path.join("proc")
    }

    fn path_proc(&self) -> PathBuf {
        self.path_proc_caches().join(Pid::this().to_string())
    }

    pub fn path_rootfs(&self) -> PathBuf {
        self.path.join("rootfs")
    }

    pub fn path_recipes(&self) -> PathBuf {
        self.path.join("recipes")
    }

    pub fn path_dependency_cache(&self) -> PathBuf {
        self.path_proc().join("depcache")
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
