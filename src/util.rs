use std::{
    fs::{copy, create_dir, exists, hard_link, read_dir, read_link, remove_dir, remove_file, set_permissions, symlink_metadata, File, OpenOptions},
    io,
    os::{
        linux::fs::MetadataExt,
        unix::fs::{symlink, PermissionsExt},
    },
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use fs2::FileExt;
use log::warn;
use nix::libc::{S_IRWXG, S_IRWXO, S_IRWXU};

pub fn get_timestamp() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH).context("Failed to get timestamp")?.as_secs())
}

pub fn format_duration(duration_in_seconds: u64) -> String {
    let hours = duration_in_seconds / 3600;
    let minutes = (duration_in_seconds / 60) % 60;
    let seconds = duration_in_seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

pub fn acquire_lockfile(path: impl AsRef<Path>) -> Result<File> {
    let file = OpenOptions::new().read(true).write(true).create(true).open(path).context("Failed to open lockfile")?;
    file.try_lock_exclusive().context("Failed to lock exclusive")?;
    Ok(file)
}

pub fn force_rm(path: impl AsRef<Path>) -> Result<()> {
    let meta = match symlink_metadata(&path) {
        Ok(meta) => Ok(meta),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => Err(e),
    }
    .with_context(|| format!("Failed to fetch metadata `{}`", path.as_ref().to_string_lossy()))?;

    if meta.is_dir() {
        force_rm_contents(&path, None)?;
        remove_dir(&path).with_context(|| format!("Failed to remove directory `{}`", path.as_ref().to_string_lossy()))?;
        return Ok(());
    }

    if !meta.is_symlink() {
        let expected_perms = PermissionsExt::from_mode(S_IRWXU | S_IRWXG | S_IRWXO);
        if meta.permissions() != expected_perms {
            set_permissions(&path, expected_perms).with_context(|| format!("Failed to set permissions `{}`", path.as_ref().to_string_lossy()))?;
        }
    }

    remove_file(&path).with_context(|| format!("Failed to remove `{}`", path.as_ref().to_string_lossy()))?;

    Ok(())
}

pub fn force_rm_contents(path: impl AsRef<Path>, exceptions: Option<Vec<&str>>) -> Result<()> {
    if !exists(&path)? {
        return Ok(());
    }

    let entries = read_dir(&path).with_context(|| format!("Failed to read directory `{}`", path.as_ref().to_string_lossy()))?;

    for entry in entries {
        let entry = entry?;

        if let Some(exceptions) = &exceptions {
            if exceptions.contains(&entry.file_name().to_string_lossy().to_string().as_str()) {
                continue;
            }
        }

        force_rm(entry.path())?;
    }

    Ok(())
}

pub fn recursive_hardlink(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<()> {
    for entry in read_dir(&from).with_context(|| format!("Failed to read directory `{}`", from.as_ref().to_string_lossy()))? {
        let entry = &entry?;
        let meta = entry.metadata().with_context(|| format!("Failed to fetch metadata `{}`", entry.path().to_string_lossy()))?;

        let dest = to.as_ref().join(entry.file_name());
        if meta.is_dir() {
            create_dir(&dest).with_context(|| format!("Failed to create directory `{}`", entry.path().to_string_lossy()))?;
            set_permissions(&dest, meta.permissions()).with_context(|| format!("Failed to set permissions `{}`", entry.path().to_string_lossy()))?;

            recursive_hardlink(entry.path(), &dest)?;
            continue;
        }

        hard_link(entry.path(), &dest).with_context(|| format!("Failed to hard link `{}` -> `{}`", entry.path().to_string_lossy(), dest.to_string_lossy()))?;
    }
    Ok(())
}

pub fn recursive_copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Result<()> {
    for entry in read_dir(&from).with_context(|| format!("Failed to read directory `{}`", from.as_ref().to_string_lossy()))? {
        let entry = &entry?;
        let meta = entry.metadata().with_context(|| format!("Failed to fetch metadata `{}`", entry.path().to_string_lossy()))?;

        let dest = to.as_ref().join(entry.file_name());
        if meta.is_dir() {
            create_dir(&dest).with_context(|| format!("Failed to create directory `{}`", dest.to_string_lossy()))?;
            set_permissions(&dest, meta.permissions()).with_context(|| format!("Failed to set permissions `{}`", entry.path().to_string_lossy()))?;

            recursive_copy(entry.path(), dest)?;
            continue;
        }

        let dest_exists = match symlink_metadata(&dest) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
        .with_context(|| format!("Failed to fetch metadata `{}`", dest.to_string_lossy()))?;

        if dest_exists {
            warn!("Recursive copy conflict on path `{}` skipping...", dest.to_string_lossy());
        }

        if meta.is_symlink() {
            let target = read_link(entry.path()).with_context(|| format!("Failed to read link `{}`", entry.path().to_string_lossy()))?;
            symlink(target, &dest).with_context(|| format!("Failed to symlink `{}` -> `{}`", entry.path().to_string_lossy(), dest.to_string_lossy()))?;
            continue;
        }

        copy(entry.path(), &dest).with_context(|| format!("Failed to copy file `{}` -> `{}`", entry.path().to_string_lossy(), dest.to_string_lossy()))?;
    }
    Ok(())
}

pub fn dir_changed_at(dir: impl AsRef<Path>) -> Result<Option<(i64, i64)>> {
    let mut latest = None;
    for entry in read_dir(&dir).with_context(|| format!("Failed to read directory `{}`", dir.as_ref().to_string_lossy()))? {
        let entry = entry?;
        let meta = entry.metadata().with_context(|| format!("Failed to fetch metadata `{}`", entry.path().to_string_lossy()))?;

        let ctime;
        if meta.is_dir() {
            ctime = dir_changed_at(entry.path())?;
        } else {
            ctime = Some((meta.st_ctime(), meta.st_ctime_nsec()))
        }

        if let Some(ctime) = ctime {
            if latest.is_none_or(|latest| ctime > latest) {
                latest = Some(ctime);
            }
        }
    }
    Ok(latest)
}
