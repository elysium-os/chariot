use std::{
    fs::create_dir_all,
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result};

pub struct Cache {
    path: PathBuf,
}

impl Cache {
    pub fn init(path: impl AsRef<Path>) -> Result<Rc<Cache>> {
        create_dir_all(&path).context("Failed to create cache directory")?;

        Ok(Rc::new(Cache {
            path: path.as_ref().to_path_buf(),
        }))
    }

    pub fn path_rootfs(&self) -> PathBuf {
        self.path.join("rootfs")
    }
}
