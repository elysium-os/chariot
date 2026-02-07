use std::{
    fs::{copy, create_dir, exists, hard_link, metadata, read_dir, read_link, remove_dir, remove_file, set_permissions, symlink_metadata, File, OpenOptions},
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
use nix::{
    libc::{S_IRWXG, S_IRWXO, S_IRWXU},
    sys::stat::Mode,
    unistd::mkdir,
};
use walkdir::WalkDir;

pub fn get_timestamp() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH).context("Failed to get current time")?.as_secs())
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
            set_permissions(&path, expected_perms).with_context(|| format!("Failed to write permissions `{}`", path.as_ref().to_string_lossy()))?;
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
        let relative_path = entry.path().strip_prefix(src.as_ref()).context("failed to resolve relative path")?;

        let dest_path = dest.as_ref().join(relative_path);

        if exists(&dest_path)? {
            if !meta.is_dir() {
                warn!("copy_recursive conflict on path `{}` skipping...", dest_path.to_str().unwrap());
            }
            continue;
        }

        if meta.is_dir() {
            create_dir(&dest_path).with_context(|| format!("create_dir failed (`{}`)", dest_path.to_str().unwrap()))?;
            continue;
        }

        if meta.is_symlink() {
            symlink(read_link(entry.path())?, &dest_path).with_context(|| format!("symlink failed (`{}` -> `{}`)", entry.path().to_str().unwrap(), dest_path.to_str().unwrap()))?;
            continue;
        }

        copy(entry.path(), &dest_path).with_context(|| format!("copy failed (`{}` -> `{}`)", entry.path().to_str().unwrap(), dest_path.to_str().unwrap()))?;
    }
    Ok(())
}

fn ctime(md: &std::fs::Metadata) -> (i64, i64) {
    (md.st_ctime(), md.st_ctime_nsec())
}

pub fn dir_changed_at(dir: impl AsRef<Path>) -> Result<(i64, i64)> {
    let meta = metadata(&dir)?;
    let mut latest = ctime(&meta);

    for entry in WalkDir::new(&dir).follow_links(false) {
        let entry = entry?;
        let meta = entry.metadata()?;

        let t = ctime(&meta);
        if t > latest {
            latest = t;
        }
    }

    Ok(latest)
}
