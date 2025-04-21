use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use child::stage1;
use nix::unistd::{Gid, Uid};

mod child;

pub struct RuntimeConfig {
    rootfs_path: PathBuf,
    pub read_only: bool,
    pub network_isolation: bool,
    pub uid: Uid,
    pub gid: Gid,
    pub cwd: PathBuf,
    pub mounts: Vec<Mount>,
    pub environment: HashMap<String, String>,
    pub output_config: Option<OutputConfig>,
}

pub struct OutputConfig {
    pub quiet: bool,
    pub log_path: Option<PathBuf>,
}

pub struct Mount {
    pub from: PathBuf,
    pub to: PathBuf,
    pub read_only: bool,
    pub is_file: bool,
}

impl Mount {
    pub fn new(from: impl AsRef<Path>, to: impl AsRef<Path>) -> Mount {
        Mount {
            from: from.as_ref().to_path_buf(),
            to: to.as_ref().to_path_buf(),
            read_only: false,
            is_file: false,
        }
    }

    pub fn is_file(mut self) -> Mount {
        self.is_file = true;
        self
    }

    pub fn read_only(mut self) -> Mount {
        self.read_only = true;
        self
    }
}

impl RuntimeConfig {
    pub fn new(rootfs_path: impl AsRef<Path>) -> RuntimeConfig {
        RuntimeConfig {
            rootfs_path: rootfs_path.as_ref().to_path_buf(),
            read_only: true,
            network_isolation: false,
            uid: Uid::from(1000),
            gid: Gid::from(1000),
            cwd: Path::new("/root").to_path_buf(),
            mounts: Vec::new(),
            environment: HashMap::new(),
            output_config: None,
        }
    }

    fn relative_rootfs_path(&self, path: &str) -> PathBuf {
        match path.to_string().strip_prefix("/") {
            Some(str) => self.rootfs_path.join(Path::new(str)),
            None => self.rootfs_path.join(path),
        }
    }

    pub fn run(&self, args: Vec<String>) -> Result<()> {
        stage1(self, args)
    }

    pub fn run_shell(&self, command: impl AsRef<str>) -> Result<()> {
        self.run(vec![String::from("bash"), String::from("-c"), command.as_ref().to_string()])
    }

    pub fn run_python(&self, command: impl AsRef<str>) -> Result<()> {
        self.run(vec![String::from("python3"), String::from("-c"), command.as_ref().to_string()])
    }
}
