use std::{
    fs::exists,
    path::{Path, PathBuf},
    sync::Arc,
};

use nix::errno::Errno;

use crate::{
    fs::{FileSystemError, dir_entries, force_rm, make_path},
    get_current_time,
    lock::{DirLock, LockExclusive},
};

pub struct TempDirProvider {
    base: PathBuf,
}

pub struct TempDir {
    _lock: DirLock<LockExclusive>,
    provider: Arc<TempDirProvider>,
    name: String,
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = force_rm(self.path());
    }
}

impl TempDir {
    pub fn path(&self) -> PathBuf {
        self.provider.base.join(&self.name)
    }
}

impl TempDirProvider {
    pub fn init(base: impl AsRef<Path>) -> Result<Self, FileSystemError> {
        make_path(&base)?;

        let temp_dir_provider = Self {
            base: base.as_ref().to_path_buf(),
        };

        temp_dir_provider.purge()?;

        Ok(temp_dir_provider)
    }

    pub fn get(self: &Arc<Self>) -> Result<TempDir, FileSystemError> {
        loop {
            let name = get_current_time().as_nanos().to_string();
            let path = self.base.join(&name);

            if exists(&path).map_err(|err| FileSystemError::Exists {
                path: path.clone(),
                source: err,
            })? {
                continue;
            }

            make_path(&path)?;

            return Ok(TempDir {
                _lock: DirLock::exclusive(&path)?,
                provider: self.clone(),
                name,
            });
        }
    }

    pub fn purge(&self) -> Result<i64, FileSystemError> {
        let _dirs_lock = DirLock::exclusive(&self.base)?;

        let mut purged_count = 0;
        for entry in dir_entries(&self.base)? {
            match DirLock::exclusive_noblock(entry.path()) {
                Err(FileSystemError::FileLock(err)) if matches!(err.source, Errno::EWOULDBLOCK) => continue,
                Err(err) => return Err(err),
                Ok(_) => {
                    purged_count += 1;
                    force_rm(entry.path())?;
                }
            }
        }

        Ok(purged_count)
    }
}
