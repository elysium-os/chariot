use std::{
    fs::{exists, hard_link, remove_dir_all, set_permissions},
    os::unix::fs::PermissionsExt,
    path::Path,
};

use anyhow::{Context, Result};
use nix::{
    libc::{S_IRWXG, S_IRWXO, S_IRWXU},
    sys::stat::Mode,
    unistd::mkdir,
};
use walkdir::WalkDir;

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
