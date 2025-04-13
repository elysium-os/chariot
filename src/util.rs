use std::{
    fs::{copy, create_dir, exists, read_link},
    os::unix::fs::symlink,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use log::warn;
use walkdir::WalkDir;

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

pub fn get_timestamp() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("Failed to get current time")?
        .as_secs())
}
