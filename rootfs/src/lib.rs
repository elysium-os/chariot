use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs::{rename, write},
    io::{self, Cursor, ErrorKind, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use chariot_runtime::{Mount, MountKind, Overlay, OverlayUpperDirectory, RuntimeError, runtime_execute};
use chariot_util::{
    fs::{FileSystemError, force_rm, force_rm_contents, make_path},
    lock::{DirLock, LockShared, block_attempted},
};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use tar::Archive;
use thiserror::Error;
use xz2::read::XzDecoder;

use crate::{
    db::Database,
    manifest::{Manifest, ManifestFetchError, PLACEHOLDER_PACKAGE, PLACEHOLDER_ROOT_PACKAGES},
    state::{CachedManifest, State},
};

pub use manifest::ManifestFetchSpec;
pub use pkgset::{CachedPkgSet, GetPkgSetError, PkgSetState};
pub use state::{StateReadError, StateWriteError};

mod db;
mod manifest;
mod pkgset;
mod state;

pub const DEFAULT_MANIFESTS_URL: &str = "https://rootfs.chariot.elysium-os.org/manifests/x86_64/@VERSION@.toml";

const ROOTFS_VERSION: i64 = 2;

#[derive(Debug, Error)]
pub enum RootFSInitError {
    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error(transparent)]
    Runtime(#[from] RuntimeError),

    #[error(transparent)]
    Database(#[from] rusqlite::Error),

    #[error("Failed to write state file")]
    StateWrite(#[from] StateWriteError),

    #[error("RootFS manifest fetch error")]
    ManifestFetch(#[from] ManifestFetchError),

    #[error("RootFS setup command exited with a non-zero code")]
    SetupScript,

    #[error("Failed to install archive `{}`", name)]
    ArchiveInstall { name: String, source: ArchiveInstallError },
}

#[derive(Debug, Error)]
pub enum ArchiveInstallError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error("Failed to unpack tar archive to `{}`", to.display())]
    TarUnpack { to: PathBuf, source: io::Error },

    #[error("Hash does not match expected hash, expected `{}`, got `{}`", expected, found)]
    HashMismatch { expected: String, found: String },

    #[error("Unsupported compression requested `{}`", .0)]
    UnknownCompression(String),

    #[error("Zstd decompression error")]
    ZstdDecompression(#[source] io::Error),
}

#[derive(Debug, Error)]
pub enum RootFSGetError {
    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error(transparent)]
    Database(#[from] rusqlite::Error),

    #[error("Failed to read state file")]
    StateRead(#[from] StateReadError),
}

#[derive(Debug, Error)]
pub enum RootFSPruneError {
    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error(transparent)]
    Database(#[from] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum RootFSListError {
    #[error(transparent)]
    FileSystem(#[from] FileSystemError),

    #[error(transparent)]
    Database(#[from] rusqlite::Error),
}

pub struct RootFSHandle {
    _lock: DirLock<LockShared>,
    path: PathBuf,
    state: State,
}

pub struct RootFS {
    pub handle: Arc<RootFSHandle>,
    db: Database,
}

enum RootFSPath {
    Gitignore,
    State,
    Database,
    Fs,
    ArchiveTmp,
    PackageSets,
    PackageSet(i64),
    PackageSetWork,
}

pub struct PkgSetMeta {
    pub id: i64,
    pub state: PkgSetState,
    pub base: Option<i64>,
    pub base_depth: u64,
    pub size: u64,
    pub packages: HashSet<String>,
}

fn rootfs_sub_path(rootfs_path: impl AsRef<Path>, sub_path: RootFSPath) -> PathBuf {
    let base = rootfs_path.as_ref();
    match sub_path {
        RootFSPath::Gitignore => base.join(".gitignore"),
        RootFSPath::State => base.join("state.toml"),
        RootFSPath::Database => base.join("rootfsdb.sqlite"),
        RootFSPath::Fs => base.join("fs"),
        RootFSPath::ArchiveTmp => base.join(".archive_tmp"),
        RootFSPath::PackageSets => base.join("pkgsets"),
        RootFSPath::PackageSet(id) => base.join("pkgsets").join(id.to_string()),
        RootFSPath::PackageSetWork => base.join("pkgsets").join(".work"),
    }
}

impl RootFSHandle {
    fn sub_path(&self, sub_path: RootFSPath) -> PathBuf {
        rootfs_sub_path(&self.path, sub_path)
    }

    pub fn get_manifest_spec(&self) -> &ManifestFetchSpec {
        &self.state.manifest
    }

    pub fn get_bsdtar_package(&self) -> &String {
        &self.state.cached_manifest.package_bsdtar
    }

    pub fn get_git_package(&self) -> &String {
        &self.state.cached_manifest.package_git
    }

    pub fn get_patch_package(&self) -> &String {
        &self.state.cached_manifest.package_patch
    }

    pub fn exec(
        &self,
        cwd: impl AsRef<Path>,
        mounts: &Vec<&Mount>,
        environment: &HashMap<impl AsRef<str>, impl AsRef<str>>,
        logger: &mut dyn Write,
        args: Vec<impl AsRef<str>>,
        pkgset: Option<&CachedPkgSet>,
    ) -> Result<i32, RuntimeError> {
        let mut lower_mounts = Vec::new();
        if let Some(pkgset) = pkgset {
            let mut lower_directories = Vec::from([self.sub_path(RootFSPath::Fs), pkgset.path()]);

            let mut base = &pkgset.base;
            while let Some(pkgset) = base {
                lower_directories.insert(1, pkgset.path());
                base = &pkgset.base;
            }

            lower_mounts.push(Mount {
                dest: PathBuf::new(),
                kind: MountKind::OverlayFS(Overlay {
                    upper_directory: None,
                    lower_directories,
                }),
            });
        }

        runtime_execute(
            self.sub_path(RootFSPath::Fs),
            true,
            self.state.cached_manifest.user_uid,
            self.state.cached_manifest.user_gid,
            cwd,
            &lower_mounts.iter().collect(),
            mounts,
            environment,
            false,
            logger,
            args,
        )
    }
}

impl RootFS {
    pub fn init(path: impl AsRef<Path>, manifest_spec: &ManifestFetchSpec, logger: &mut dyn Write) -> Result<Self, RootFSInitError> {
        let manifest = Manifest::fetch(&manifest_spec)?;

        make_path(&path)?;
        let rootfs_lock = DirLock::exclusive_noblock(&path)?;
        force_rm_contents(&path, None)?;

        let gitignore_path = rootfs_sub_path(&path, RootFSPath::Gitignore);
        write(&gitignore_path, "# Generated by Chariot\n*").map_err(|err| FileSystemError::WriteFile {
            path: gitignore_path.clone(),
            source: err,
        })?;

        for sub_path in [RootFSPath::Fs, RootFSPath::PackageSets] {
            make_path(rootfs_sub_path(&path, sub_path))?;
        }

        let state = State {
            manifest: manifest_spec.clone(),
            cached_manifest: CachedManifest {
                root_packages: manifest.packages.root,
                package_bsdtar: manifest.packages.bsdtar,
                package_git: manifest.packages.git,
                package_patch: manifest.packages.patch,
                command_pkg_download: manifest.commands.pkg_download,
                command_pkg_install: manifest.commands.pkg_install,
                user_uid: manifest.ids.user_uid,
                user_gid: manifest.ids.user_gid,
                root_uid: manifest.ids.root_uid,
                root_gid: manifest.ids.root_gid,
            },
        };

        state.write(rootfs_sub_path(&path, RootFSPath::State), false)?;

        for (name, archive) in manifest.archives {
            Self::install_archive(
                rootfs_sub_path(&path, RootFSPath::Fs),
                rootfs_sub_path(&path, RootFSPath::ArchiveTmp),
                archive.url,
                archive.compression,
                archive.hash,
                archive.subdir,
            )
            .map_err(|err| RootFSInitError::ArchiveInstall { name, source: err })?;
        }

        let setup_command = manifest.commands.setup.replace(
            PLACEHOLDER_ROOT_PACKAGES,
            &state
                .cached_manifest
                .root_packages
                .iter()
                .map(|str| str.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        );

        let exit_code = runtime_execute(
            rootfs_sub_path(&path, RootFSPath::Fs),
            false,
            state.cached_manifest.root_uid,
            state.cached_manifest.root_gid,
            "/",
            &vec![],
            &vec![],
            &HashMap::<&str, &str>::new(),
            false,
            logger,
            vec!["bash", "-c", &setup_command],
        )?;

        if exit_code != 0 {
            return Err(RootFSInitError::SetupScript);
        }

        let db = Database::connect(rootfs_sub_path(&path, RootFSPath::Database))?;

        state.write(rootfs_sub_path(&path, RootFSPath::State), true)?;

        let handle = Arc::new(RootFSHandle {
            _lock: rootfs_lock.relock_shared_noblock()?,
            path: path.as_ref().to_path_buf(),
            state,
        });

        Ok(Self { handle, db })
    }

    pub fn get(path: impl AsRef<Path>) -> Result<Option<Self>, RootFSGetError> {
        let cache_lock = match DirLock::shared(&path) {
            Err(FileSystemError::Open { source, .. }) if source.kind() == ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
            Ok(lock) => lock,
        };

        let state = match State::read(rootfs_sub_path(&path, RootFSPath::State))? {
            None => return Ok(None),
            Some(state) => state,
        };

        let handle = Arc::new(RootFSHandle {
            _lock: cache_lock,
            path: path.as_ref().to_path_buf(),
            state,
        });

        let cache = Self {
            handle,
            db: Database::connect(rootfs_sub_path(&path, RootFSPath::Database))?,
        };

        Ok(Some(cache))
    }

    fn install_archive(
        dest: impl AsRef<Path>,
        tmp_path: impl AsRef<Path>,
        url: impl AsRef<str>,
        compression: impl AsRef<str>,
        hash: impl AsRef<str>,
        subdir: Option<impl AsRef<str>>,
    ) -> Result<(), ArchiveInstallError> {
        let client = Client::builder().connect_timeout(Duration::from_secs(60)).build()?;
        let archive_data = client.get(url.as_ref()).send()?.error_for_status()?.bytes()?;
        let archive_hash = {
            let mut hasher = Sha256::new();
            hasher.update(&archive_data);
            hex::encode(hasher.finalize())
        };

        if archive_hash != hash.as_ref() {
            return Err(ArchiveInstallError::HashMismatch {
                expected: hash.as_ref().to_string(),
                found: archive_hash,
            });
        }

        let decompressor: &mut dyn io::Read = match compression.as_ref() {
            "xz" => &mut XzDecoder::new(Cursor::new(archive_data)),
            "zstd" => &mut zstd::Decoder::new(Cursor::new(archive_data)).map_err(|err| ArchiveInstallError::ZstdDecompression(err))?,
            _ => return Err(ArchiveInstallError::UnknownCompression(compression.as_ref().to_string())),
        };

        let unpack_path = match subdir {
            None => dest.as_ref(),
            Some(_) => tmp_path.as_ref(),
        };

        Archive::new(decompressor)
            .unpack(&unpack_path)
            .map_err(|err| ArchiveInstallError::TarUnpack {
                to: unpack_path.to_path_buf(),
                source: err,
            })?;

        if let Some(subdir) = subdir {
            let from_path = unpack_path.join(subdir.as_ref());
            rename(&from_path, &dest).map_err(|err| FileSystemError::Rename {
                from: from_path.to_path_buf(),
                to: dest.as_ref().to_path_buf(),
                source: err,
            })?;

            force_rm(unpack_path)?;
        }

        Ok(())
    }

    fn download_native_package(&self, package: impl AsRef<str>, logger: &mut dyn Write) -> Result<bool, RuntimeError> {
        let exit_code = runtime_execute(
            self.handle.sub_path(RootFSPath::Fs),
            false,
            self.handle.state.cached_manifest.root_uid,
            self.handle.state.cached_manifest.root_gid,
            "/",
            &vec![],
            &vec![],
            &HashMap::<&str, &str>::new(),
            false,
            logger,
            vec![
                "bash",
                "-c",
                &self
                    .handle
                    .state
                    .cached_manifest
                    .command_pkg_download
                    .replace(PLACEHOLDER_PACKAGE, package.as_ref()),
            ],
        )?;

        Ok(exit_code == 0)
    }

    fn install_native_package(
        &self,
        base: Option<&CachedPkgSet>,
        install_path: impl AsRef<Path>,
        work_path: impl AsRef<Path>,
        package: impl AsRef<str>,
        logger: &mut dyn Write,
    ) -> Result<bool, RuntimeError> {
        let mut lower_directories = vec![self.handle.sub_path(RootFSPath::Fs)];

        let mut base = base;
        while let Some(pkgset) = base {
            lower_directories.insert(1, pkgset.path());
            base = pkgset.base.as_deref();
        }

        let exit_code = runtime_execute(
            self.handle.sub_path(RootFSPath::Fs),
            true,
            self.handle.state.cached_manifest.root_uid,
            self.handle.state.cached_manifest.root_gid,
            "/",
            &vec![&Mount {
                dest: PathBuf::new(),
                kind: MountKind::OverlayFS(Overlay {
                    upper_directory: Some(OverlayUpperDirectory {
                        upper_directory: install_path.as_ref().to_path_buf(),
                        work_directory: work_path.as_ref().to_path_buf(),
                    }),
                    lower_directories,
                }),
            }],
            &vec![],
            &HashMap::<&str, &str>::new(),
            false,
            logger,
            vec![
                "bash",
                "-c",
                &self
                    .handle
                    .state
                    .cached_manifest
                    .command_pkg_install
                    .replace(PLACEHOLDER_PACKAGE, package.as_ref()),
            ],
        )?;

        return Ok(exit_code == 0);
    }

    pub fn list_pkgsets(&self) -> Result<Vec<PkgSetMeta>, RootFSListError> {
        let _pkgsets_lock = DirLock::exclusive(self.handle.sub_path(RootFSPath::PackageSets))?;

        let pkgsets = self.db.get_pkgsets()?;

        Ok(pkgsets)
    }

    pub fn prune_pkgsets(&self, predicate: fn(pkgset_meta: &PkgSetMeta) -> bool) -> Result<(usize, usize, usize), RootFSPruneError> {
        let _pkgsets_lock = DirLock::exclusive(self.handle.sub_path(RootFSPath::PackageSets))?;

        let pkgsets = self.db.get_pkgsets()?.into_iter().map(|meta| (meta.id, meta)).collect::<HashMap<_, _>>();

        let pkgsets_total = pkgsets.len();
        let mut pkgsets_removed: usize = 0;
        let mut pkgsets_in_use: usize = 0;

        let mut dep_counts = HashMap::new();
        for meta in pkgsets.values() {
            if let Some(base) = meta.base {
                *dep_counts.entry(base).or_insert(0) += 1;
            }
        }

        let mut queued_pkgsets = VecDeque::new();
        for id in pkgsets.keys() {
            if dep_counts.get(id).copied().unwrap_or(0) > 0 {
                continue;
            }
            queued_pkgsets.push_back(*id);
        }

        while let Some(pkgset_id) = queued_pkgsets.pop_front() {
            let pkgset = &pkgsets[&pkgset_id];
            let path = self.handle.sub_path(RootFSPath::PackageSet(pkgset.id));

            let _lock = match DirLock::exclusive_noblock(&path) {
                result if block_attempted(&result) => {
                    pkgsets_in_use += 1;
                    continue;
                }
                result => result,
            }?;

            if !predicate(&pkgset) {
                continue;
            }

            self.db.update_pkgset(pkgset.id, &PkgSetState::Unknown, None)?;
            force_rm(&path)?;

            self.db.remove_pkgset(pkgset.id)?;
            pkgsets_removed += 1;

            if let Some(base) = &pkgset.base {
                if let Some(count) = dep_counts.get_mut(base) {
                    *count -= 1;
                    if *count == 0 {
                        dep_counts.remove(base);
                        queued_pkgsets.push_back(*base);
                    }
                }
            }
        }

        Ok((pkgsets_total, pkgsets_removed, pkgsets_in_use))
    }
}
