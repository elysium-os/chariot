#![allow(dead_code)]

use std::{
    fs::{DirEntry, File, copy, create_dir_all, exists, hard_link, read_dir, remove_dir, remove_file, rename, set_permissions, symlink_metadata},
    io::{self, BufReader, ErrorKind, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

use nix::libc::{S_IRWXG, S_IRWXO, S_IRWXU};
use thiserror::Error;

use crate::lock::FileLockError;

#[derive(Error, Debug)]
pub enum FileSystemError {
    #[error("Failed to check existence of `{}`", path.display())]
    Exists { path: PathBuf, source: io::Error },

    #[error("Failed to get metadata of `{}`", path.display())]
    Metadata { path: PathBuf, source: io::Error },

    #[error("Failed to get file type of `{}`", path.display())]
    FileType { path: PathBuf, source: io::Error },

    #[error("Failed to create directories along the path of `{}`", path.display())]
    CreateDirAll { path: PathBuf, source: io::Error },

    #[error("Failed to create file `{}`", path.display())]
    CreateFile { path: PathBuf, source: io::Error },

    #[error("Failed to remove file `{}`", path.display())]
    RemoveFile { path: PathBuf, source: io::Error },

    #[error("Failed to remove directory `{}`", path.display())]
    RemoveDirectory { path: PathBuf, source: io::Error },

    #[error("Failed to read directory `{}`", path.display())]
    ReadDirectory { path: PathBuf, source: io::Error },

    #[error("Failed to set permissions `{}`", path.display())]
    SetPermissions { path: PathBuf, source: io::Error },

    #[error("Failed to copy file from `{}` to `{}`", from.display(), to.display())]
    CopyFile { from: PathBuf, to: PathBuf, source: io::Error },

    #[error("Failed to open `{}`", path.display())]
    Open { path: PathBuf, source: io::Error },

    #[error("Failed to read file `{}`", path.display())]
    ReadFile { path: PathBuf, source: io::Error },

    #[error("Failed to seek file `{}`", path.display())]
    SeekFile { path: PathBuf, source: io::Error },

    #[error("Failed to write file `{}`", path.display())]
    WriteFile { path: PathBuf, source: io::Error },

    #[error("Failed to resolve directory entry in `{}`", parent_path.display())]
    DirEntry { parent_path: PathBuf, source: io::Error },

    #[error("Failed to hardlink file `{}` to `{}`", from.display(), to.display())]
    Hardlink { from: PathBuf, to: PathBuf, source: io::Error },

    #[error("Failed to rename `{}` to `{}`", from.display(), to.display())]
    Rename { from: PathBuf, to: PathBuf, source: io::Error },

    #[error(transparent)]
    FileLock(#[from] FileLockError),
}

pub fn make_path(path: impl AsRef<Path>) -> Result<(), FileSystemError> {
    create_dir_all(&path).map_err(|err| FileSystemError::CreateDirAll {
        path: path.as_ref().to_path_buf(),
        source: err,
    })
}

pub fn force_rm(path: impl AsRef<Path>) -> Result<(), FileSystemError> {
    let meta = match symlink_metadata(&path) {
        Ok(meta) => Ok(meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => Err(e),
    }
    .map_err(|err| FileSystemError::Metadata {
        path: path.as_ref().to_path_buf(),
        source: err,
    })?;

    if !meta.is_symlink() {
        let expected_perms = PermissionsExt::from_mode(S_IRWXU | S_IRWXG | S_IRWXO);
        if meta.permissions() != expected_perms {
            set_permissions(&path, expected_perms).map_err(|err| FileSystemError::SetPermissions {
                path: path.as_ref().to_path_buf(),
                source: err,
            })?;
        }
    }

    if meta.is_dir() {
        force_rm_contents(&path, None)?;
        remove_dir(&path).map_err(|err| FileSystemError::RemoveDirectory {
            path: path.as_ref().to_path_buf(),
            source: err,
        })?;
        return Ok(());
    }

    remove_file(&path).map_err(|err| FileSystemError::RemoveFile {
        path: path.as_ref().to_path_buf(),
        source: err,
    })?;

    Ok(())
}

pub fn force_rm_contents(path: impl AsRef<Path>, exceptions: Option<Vec<&str>>) -> Result<(), FileSystemError> {
    if !exists(&path).map_err(|err| FileSystemError::Exists {
        path: path.as_ref().to_path_buf(),
        source: err,
    })? {
        return Ok(());
    }

    let entries = match dir_entries(&path) {
        Ok(entries) => Ok(entries),
        Err(FileSystemError::ReadDirectory { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied => {
            set_permissions(&path, PermissionsExt::from_mode(S_IRWXU | S_IRWXG | S_IRWXO)).map_err(|err| FileSystemError::SetPermissions {
                path: path.as_ref().to_path_buf(),
                source: err,
            })?;

            dir_entries(&path)
        }
        Err(e) => Err(e),
    }?;

    for entry in entries {
        if let Some(exceptions) = &exceptions {
            if exceptions.contains(&entry.file_name().to_string_lossy().as_ref()) {
                continue;
            }
        }

        force_rm(entry.path())?;
    }

    Ok(())
}

pub fn copy_recursive(src: impl AsRef<std::path::Path>, dest: impl AsRef<std::path::Path>) -> Result<(), FileSystemError> {
    create_dir_all(&dest).map_err(|err| FileSystemError::CreateDirAll {
        path: dest.as_ref().to_path_buf(),
        source: err,
    })?;

    for entry in dir_entries(&src)? {
        let file_type = entry.file_type().map_err(|err| FileSystemError::FileType {
            path: entry.path(),
            source: err,
        })?;
        let dest_path = dest.as_ref().join(entry.file_name());

        if file_type.is_dir() {
            copy_recursive(entry.path(), dest_path)?;
        } else {
            copy(entry.path(), &dest_path).map_err(|err| FileSystemError::CopyFile {
                from: entry.path(),
                to: dest_path,
                source: err,
            })?;
        }
    }

    Ok(())
}

fn files_identical(path_a: impl AsRef<Path>, path_b: impl AsRef<Path>) -> Result<bool, FileSystemError> {
    let mut file_a = BufReader::new(File::open(&path_a).map_err(|err| FileSystemError::Open {
        path: path_a.as_ref().to_path_buf(),
        source: err,
    })?);
    let mut file_b = BufReader::new(File::open(&path_b).map_err(|err| FileSystemError::Open {
        path: path_b.as_ref().to_path_buf(),
        source: err,
    })?);

    let mut buf_a = [0u8; 64 * 1024];
    let mut buf_b = [0u8; 64 * 1024];

    loop {
        let count_a = file_a.read(&mut buf_a).map_err(|err| FileSystemError::ReadFile {
            path: path_a.as_ref().to_path_buf(),
            source: err,
        })?;
        let count_b = file_b.read(&mut buf_b).map_err(|err| FileSystemError::ReadFile {
            path: path_b.as_ref().to_path_buf(),
            source: err,
        })?;

        if count_a != count_b || buf_a[..count_a] != buf_b[..count_b] {
            return Ok(false);
        }

        if count_a == 0 {
            return Ok(true);
        }
    }
}

pub fn deduplicate(path_to: impl AsRef<Path>, path_from: impl AsRef<Path>) -> Result<(), FileSystemError> {
    for entry in dir_entries(&path_to)? {
        let path_to = path_to.as_ref().join(entry.file_name());
        let path_from = path_from.as_ref().join(entry.file_name());

        let meta_to = entry.metadata().map_err(|err| FileSystemError::Metadata {
            path: entry.path(),
            source: err,
        })?;

        if meta_to.is_dir() {
            deduplicate(path_to, path_from)?;
            continue;
        }

        if !meta_to.is_file() {
            continue;
        }

        let meta_from = match path_from.metadata() {
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(err) => panic!("{}", err),
            Ok(meta) => meta,
        };

        if meta_to.ino() == meta_from.ino() {
            continue;
        }

        if meta_to.size() != meta_from.size() {
            continue;
        }

        if !files_identical(&path_to, &path_from)? {
            continue;
        }

        let tmp_path = path_from.with_extension(".dedup_tmp");
        hard_link(&path_to, &tmp_path).map_err(|err| FileSystemError::Hardlink {
            from: path_to,
            to: tmp_path.clone(),
            source: err,
        })?;
        rename(&tmp_path, &path_from).map_err(|err| FileSystemError::Rename {
            from: tmp_path,
            to: path_from,
            source: err,
        })?;
    }

    Ok(())
}

pub fn dir_entries(path: impl AsRef<Path>) -> Result<Vec<DirEntry>, FileSystemError> {
    let entries = read_dir(&path).map_err(|err| FileSystemError::ReadDirectory {
        path: path.as_ref().to_path_buf(),
        source: err,
    })?;

    let entries = entries
        .into_iter()
        .map(|entry| {
            entry.map_err(|err| FileSystemError::DirEntry {
                parent_path: path.as_ref().to_path_buf(),
                source: err,
            })
        })
        .collect::<Result<Vec<DirEntry>, FileSystemError>>()?;

    Ok(entries)
}

pub fn dir_size(dir: impl AsRef<Path>) -> Result<u64, FileSystemError> {
    let mut size: u64 = 0;
    for entry in dir_entries(&dir)? {
        let meta = entry.metadata().map_err(|err| FileSystemError::Metadata {
            path: entry.path(),
            source: err,
        })?;

        if meta.is_dir() {
            size += dir_size(entry.path())?;
            continue;
        }

        size += meta.len();
    }

    Ok(size)
}

pub fn move_contents(from: impl AsRef<Path>, to: impl AsRef<Path>, exceptions: Option<Vec<&str>>) -> Result<(), FileSystemError> {
    for entry in dir_entries(from)? {
        if let Some(exceptions) = &exceptions {
            if exceptions.contains(&entry.file_name().to_string_lossy().as_ref()) {
                continue;
            }
        }

        let path_from = entry.path();
        let path_to = to.as_ref().join(entry.file_name());

        rename(&path_from, &path_to).map_err(|err| FileSystemError::Rename {
            from: path_from.clone(),
            to: path_to.clone(),
            source: err,
        })?;
    }
    Ok(())
}

pub fn join_soft(a: impl AsRef<Path>, b: impl AsRef<Path>) -> PathBuf {
    let mut joined = a.as_ref().to_path_buf();
    for component in b.as_ref().components() {
        match component {
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
            Component::ParentDir | Component::Normal(_) => joined.push(component),
        }
    }
    joined
}
