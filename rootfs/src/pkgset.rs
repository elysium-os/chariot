use std::{collections::BTreeSet, io::Write, path::PathBuf, sync::Arc};

use chariot_util::{
    fs::{deduplicate, dir_entries, dir_size, force_rm_contents, make_path},
    lock::{DirLock, LockShared},
};

use crate::{RootFS, RootFSError, RootFSPath};

pub enum PkgSetState {
    Unknown,
    Cached,
    Deduplicated,
}

pub struct CachedPkgSet {
    rootfs: Arc<RootFS>,
    _lock: DirLock<LockShared>,
    id: i64,
    size: u64,
}

impl CachedPkgSet {
    pub fn get(rootfs: &Arc<RootFS>, mut pkgset: BTreeSet<&str>, logger: &mut dyn Write) -> Result<Option<Self>, RootFSError> {
        for pkg in rootfs.state.root_packages.iter().chain(rootfs.state.extra_root_packages.iter()) {
            pkgset.remove(pkg.as_str());
        }

        if pkgset.is_empty() {
            return Ok(None);
        }

        let id = rootfs.db.get_pkgset_id(&pkgset)?;

        let _pkgsets_lock = DirLock::exclusive(rootfs.sub_path(RootFSPath::PackageSets))?;

        let (state, mut size) = rootfs.db.get_pkgset(id)?;

        let pkgset_path = rootfs.sub_path(RootFSPath::PackageSet(id));
        make_path(&pkgset_path)?;

        let cached_pkgset = Self {
            rootfs: rootfs.clone(),
            _lock: DirLock::shared_noblock(&pkgset_path)?,
            id,
            size,
        };

        match state {
            PkgSetState::Deduplicated => return Ok(Some(cached_pkgset)),
            PkgSetState::Cached => {}
            PkgSetState::Unknown => {
                {
                    let _rootfs_lock = DirLock::exclusive(rootfs.sub_path(RootFSPath::Fs))?;
                    for pkg in &pkgset {
                        rootfs.download_native_package(pkg, logger)?;
                    }
                }

                let workdir_path = rootfs.sub_path(RootFSPath::PackageSetWork);
                make_path(&workdir_path)?;
                force_rm_contents(&workdir_path, None)?;

                for pkg in &pkgset {
                    rootfs.install_native_package(&pkgset_path, &workdir_path, pkg, logger)?;
                }

                size = dir_size(&pkgset_path)?;

                rootfs.db.update_pkgset(id, &PkgSetState::Cached, Some(size))?;
            }
        }

        for entry in dir_entries(&rootfs.sub_path(RootFSPath::PackageSets))? {
            if pkgset_path == entry.path() {
                continue;
            }

            deduplicate(&pkgset_path, entry.path())?;
        }

        rootfs.db.update_pkgset(id, &PkgSetState::Deduplicated, None)?;

        Ok(Some(cached_pkgset))
    }

    pub fn path(&self) -> PathBuf {
        self.rootfs.sub_path(RootFSPath::PackageSet(self.id))
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}
