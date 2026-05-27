use std::{collections::BTreeSet, io::Write, path::PathBuf, sync::Arc};

use chariot_util::{
    fs::{deduplicate, dir_entries, dir_size, force_rm_contents, make_path},
    lock::{DirLock, LockShared},
};

use crate::{RootFS, RootFSError, RootFSHandle, RootFSPath};

pub enum PkgSetState {
    Unknown,
    Cached,
    Deduplicated,
}

pub struct CachedPkgSet {
    pub handle: Arc<RootFSHandle>,
    _lock: DirLock<LockShared>,
    id: i64,
    size: u64,
}

impl CachedPkgSet {
    pub fn get(rootfs: &RootFS, pkgset: &BTreeSet<&str>, logger: &mut dyn Write) -> Result<Option<Self>, RootFSError> {
        if pkgset.is_empty() {
            return Ok(None);
        }

        let id = rootfs.db.get_pkgset_id(pkgset)?;

        let _pkgsets_lock = DirLock::exclusive(rootfs.handle.sub_path(RootFSPath::PackageSets))?;

        let (state, mut size) = rootfs.db.get_pkgset(id)?;

        let pkgset_path = rootfs.handle.sub_path(RootFSPath::PackageSet(id));
        make_path(&pkgset_path)?;

        let cached_pkgset = Self {
            handle: rootfs.handle.clone(),
            _lock: DirLock::shared_noblock(&pkgset_path)?,
            id,
            size,
        };

        match state {
            PkgSetState::Deduplicated => return Ok(Some(cached_pkgset)),
            PkgSetState::Cached => {}
            PkgSetState::Unknown => {
                {
                    let _rootfs_lock = DirLock::exclusive(rootfs.handle.sub_path(RootFSPath::Fs))?;
                    for pkg in pkgset {
                        rootfs.download_native_package(pkg, logger)?;
                    }
                }

                let workdir_path = rootfs.handle.sub_path(RootFSPath::PackageSetWork);
                make_path(&workdir_path)?;
                force_rm_contents(&workdir_path, None)?;

                for pkg in pkgset {
                    rootfs.install_native_package(&pkgset_path, &workdir_path, pkg, logger)?;
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

        Ok(Some(cached_pkgset))
    }

    pub fn path(&self) -> PathBuf {
        self.handle.sub_path(RootFSPath::PackageSet(self.id))
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}
