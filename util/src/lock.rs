use std::{
    fmt::{self, Display, Formatter},
    fs::{File, OpenOptions},
    marker::PhantomData,
    path::{Path, PathBuf},
};

use nix::{
    errno::Errno,
    fcntl::{Flock, FlockArg},
};
use thiserror::Error;

use crate::fs::FileSystemError;

#[derive(Error, Debug)]
#[error("Failed to acquire an `{}` on `{}`", kind, path.display())]
pub struct FileLockError {
    pub kind: FileLockKind,
    pub path: PathBuf,
    pub source: Errno,
}

#[derive(Debug)]
pub enum FileLockKind {
    Exclusive,
    ExclusiveNoBlock,
    Shared,
    SharedNoBlock,
}

pub struct LockExclusive;
pub struct LockShared;

pub struct DirLock<LockType> {
    _lock_type: PhantomData<LockType>,
    lock: Flock<File>,
    path: PathBuf,
}

impl Display for FileLockKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exclusive => write!(f, "exclusive lock"),
            Self::ExclusiveNoBlock => write!(f, "exclusive non-blocking lock"),
            Self::Shared => write!(f, "shared lock"),
            Self::SharedNoBlock => write!(f, "shared non-blocking lock"),
        }
    }
}

impl From<&FileLockKind> for FlockArg {
    fn from(value: &FileLockKind) -> FlockArg {
        match value {
            FileLockKind::Exclusive => Self::LockExclusive,
            FileLockKind::ExclusiveNoBlock => Self::LockExclusiveNonblock,
            FileLockKind::Shared => Self::LockShared,
            FileLockKind::SharedNoBlock => Self::LockSharedNonblock,
        }
    }
}

impl DirLock<LockShared> {
    pub fn shared(path: impl AsRef<Path>) -> Result<Self, FileSystemError> {
        Self::get(path, FileLockKind::Shared)
    }

    pub fn shared_noblock(path: impl AsRef<Path>) -> Result<Self, FileSystemError> {
        Self::get(path, FileLockKind::SharedNoBlock)
    }

    pub fn relock_exclusive(self) -> Result<DirLock<LockExclusive>, FileSystemError> {
        self.relock(FileLockKind::Exclusive)?;
        Ok(DirLock {
            _lock_type: PhantomData,
            lock: self.lock,
            path: self.path,
        })
    }

    pub fn relock_exclusive_noblock(self) -> Result<DirLock<LockExclusive>, FileSystemError> {
        self.relock(FileLockKind::ExclusiveNoBlock)?;
        Ok(DirLock {
            _lock_type: PhantomData,
            lock: self.lock,
            path: self.path,
        })
    }
}

impl DirLock<LockExclusive> {
    pub fn exclusive(path: impl AsRef<Path>) -> Result<Self, FileSystemError> {
        Self::get(path, FileLockKind::Exclusive)
    }

    pub fn exclusive_noblock(path: impl AsRef<Path>) -> Result<Self, FileSystemError> {
        Self::get(path, FileLockKind::ExclusiveNoBlock)
    }

    pub fn relock_shared(self) -> Result<DirLock<LockShared>, FileSystemError> {
        self.relock(FileLockKind::Shared)?;
        Ok(DirLock {
            _lock_type: PhantomData,
            lock: self.lock,
            path: self.path,
        })
    }

    pub fn relock_shared_noblock(self) -> Result<DirLock<LockShared>, FileSystemError> {
        self.relock(FileLockKind::SharedNoBlock)?;
        Ok(DirLock {
            _lock_type: PhantomData,
            lock: self.lock,
            path: self.path,
        })
    }
}

impl<LockType> DirLock<LockType> {
    fn get(path: impl AsRef<Path>, kind: FileLockKind) -> Result<DirLock<LockType>, FileSystemError> {
        let lock = open_file_locked(&path, OpenOptions::new().read(true), kind)?;
        Ok(Self {
            _lock_type: PhantomData,
            lock,
            path: path.as_ref().to_path_buf(),
        })
    }

    fn relock(&self, kind: FileLockKind) -> Result<(), FileSystemError> {
        self.lock.relock((&kind).into()).map_err(|err| FileLockError {
            kind,
            path: self.path.clone(),
            source: err,
        })?;

        Ok(())
    }
}

pub fn block_attempted<T>(result: &Result<DirLock<T>, FileSystemError>) -> bool {
    matches!(
        result,
        Err(FileSystemError::FileLock(FileLockError {
            source: Errno::EWOULDBLOCK,
            ..
        }))
    )
}

pub fn open_file_locked(path: impl AsRef<Path>, open_options: &OpenOptions, kind: FileLockKind) -> Result<Flock<File>, FileSystemError> {
    let file = open_options.open(&path).map_err(|err| FileSystemError::Open {
        path: path.as_ref().to_path_buf(),
        source: err,
    })?;

    let locked_file = Flock::lock(file, (&kind).into()).map_err(|(_, errno)| FileLockError {
        kind,
        path: path.as_ref().to_path_buf(),
        source: errno,
    })?;

    Ok(locked_file)
}
