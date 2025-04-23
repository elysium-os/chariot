use std::{
    fs::{copy, create_dir, exists, hard_link, read_link, remove_dir_all, set_permissions},
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use log::warn;
use nix::{
    libc::{S_IRWXG, S_IRWXO, S_IRWXU},
    sys::stat::Mode,
    unistd::mkdir,
};
use walkdir::WalkDir;

pub fn get_timestamp() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH).context("Failed to get current time")?.as_secs())
}

pub fn clean(path: impl AsRef<Path>) -> Result<()> {
    if !exists(&path)? {
        return Ok(());
    }
    rewrite_permissions(&path).context("Failed to rewrite permissions")?;
    remove_dir_all(&path).context("Failed to remove directory")?;
    Ok(())
}

fn rewrite_permissions(path: impl AsRef<Path>) -> Result<()> {
    for entry in WalkDir::new(&path).contents_first(true) {
        let entry = &entry?;

        if entry.path_is_symlink() {
            continue;
        }

        set_permissions(entry.path(), PermissionsExt::from_mode(S_IRWXU | S_IRWXG | S_IRWXO))?;
    }
    Ok(())
}

pub fn link_recursive(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<()> {
    for entry in WalkDir::new(&from).min_depth(1) {
        let entry = &entry?;
        let meta = entry.metadata()?;
        let relative_path = entry.path().strip_prefix(&from)?;

        let dest_path = to.as_ref().join(relative_path);

        if meta.is_dir() {
            mkdir(&dest_path, Mode::S_IRWXU | Mode::S_IRWXG | Mode::S_IROTH | Mode::S_IXOTH).context("Failed to mkdir")?;
            continue;
        }

        hard_link(entry.path(), &dest_path).context("Failed to hard link")?;
    }
    Ok(())
}

pub fn copy_recursive(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> Result<()> {
    for entry in WalkDir::new(src.as_ref()).min_depth(1) {
        let entry = &entry?;

        let meta = entry.metadata().context("failed to get metadata")?;
        let relative_path = entry.path().strip_prefix(src.as_ref()).context("failed to compute relative path")?;

        let dest_path = dest.as_ref().join(relative_path);

        if exists(&dest_path)? {
            if !meta.is_dir() {
                warn!("copy_recursive conflict on path `{}` skipping...", dest_path.to_str().unwrap());
            }
            continue;
        }

        if meta.is_dir() {
            create_dir(&dest_path).context("create_dir failed")?;
            continue;
        }

        if meta.is_symlink() {
            symlink(read_link(entry.path())?, dest_path).context("symlink failed")?;
            continue;
        }

        copy(entry.path(), &dest_path).context("copy failed")?;
    }
    Ok(())
}
