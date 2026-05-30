use std::{collections::BTreeSet, io::Write, path::PathBuf, sync::Arc};

use chariot_runtime::RuntimeError;
use chariot_util::{
    fs::{FileSystemError, deduplicate, dir_entries, dir_size, force_rm_contents, make_path},
    lock::{DirLock, LockShared},
};
use thiserror::Error;

use crate::{RootFS, RootFSHandle, RootFSPath};

#[derive(Debug, Error)]
pub enum GetPkgSetError {
    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error(transparent)]
    Database(#[from] rusqlite::Error),

    #[error("Failed to download native package `{}`", name)]
    DownloadPackageError { name: String },

    #[error("Failed to install native package `{}`", name)]
    InstallPackageError { name: String },
}

pub enum PkgSetState {
    Unknown,
    Cached,
    Deduplicated,
}

pub struct CachedPkgSet {
    pub handle: Arc<RootFSHandle>,
    _lock: DirLock<LockShared>,
    pub(super) base: Option<Arc<CachedPkgSet>>,
    base_count: usize,
    id: i64,
    size: u64,
}

impl CachedPkgSet {
    pub fn get(
        rootfs: &RootFS,
        base: Option<Arc<CachedPkgSet>>,
        pkgset: &BTreeSet<&str>,
        logger: &mut dyn Write,
    ) -> Result<Option<Arc<Self>>, GetPkgSetError> {
        if pkgset.is_empty() {
            if let Some(base) = base {
                return Ok(Some(base.clone()));
            }

            return Ok(None);
        }

        let id = rootfs.db.get_pkgset_id(base.as_ref().map(|pkgset| pkgset.id), pkgset)?;

        let _pkgsets_lock = DirLock::exclusive(rootfs.handle.sub_path(RootFSPath::PackageSets))?;

        let (state, base_id, mut size) = rootfs.db.get_pkgset(id)?;
        assert!(base.as_ref().map(|pkgset| pkgset.id) == base_id);

        let pkgset_path = rootfs.handle.sub_path(RootFSPath::PackageSet(id));
        make_path(&pkgset_path)?;

        let base_count = match &base {
            None => 0,
            Some(base) => base.base_count + 1,
        };

        let cached_pkgset = Self {
            handle: rootfs.handle.clone(),
            base,
            base_count,
            _lock: DirLock::shared_noblock(&pkgset_path)?,
            id,
            size,
        };

        match state {
            PkgSetState::Deduplicated => return Ok(Some(Arc::new(cached_pkgset))),
            PkgSetState::Cached => {}
            PkgSetState::Unknown => {
                {
                    let _rootfs_lock = DirLock::exclusive(rootfs.handle.sub_path(RootFSPath::Fs))?;
                    for pkg in pkgset {
                        if !rootfs.download_native_package(pkg, logger)? {
                            return Err(GetPkgSetError::DownloadPackageError { name: pkg.to_string() });
                        }
                    }
                }

                let workdir_path = rootfs.handle.sub_path(RootFSPath::PackageSetWork);
                make_path(&workdir_path)?;
                force_rm_contents(&workdir_path, None)?;

                for pkg in pkgset {
                    if !rootfs.install_native_package(cached_pkgset.base.as_deref(), &pkgset_path, &workdir_path, pkg, logger)? {
                        return Err(GetPkgSetError::InstallPackageError { name: pkg.to_string() });
                    }
                }

                size = dir_size(&pkgset_path)?;

                rootfs.db.update_pkgset(id, &PkgSetState::Cached, Some(size))?;
            }
        }

        for entry in dir_entries(&rootfs.handle.sub_path(RootFSPath::PackageSets))? {
            if pkgset_path == entry.path() {
                continue;
            }

            deduplicate(&pkgset_path, entry.path())?;
        }

        rootfs.db.update_pkgset(id, &PkgSetState::Deduplicated, None)?;

        Ok(Some(Arc::new(cached_pkgset)))
    }

    pub fn path(&self) -> PathBuf {
        self.handle.sub_path(RootFSPath::PackageSet(self.id))
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn base_count(&self) -> usize {
        self.base_count
    }
}
