use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use nix::unistd::{Gid, Uid};

use child::stage1;

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

#[allow(dead_code)]
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

#[allow(dead_code)]
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

    pub fn set_read_only(mut self, read_only: bool) -> RuntimeConfig {
        self.read_only = read_only;
        self
    }

    pub fn set_network_isolation(mut self, network_isolation: bool) -> RuntimeConfig {
        self.network_isolation = network_isolation;
        self
    }

    pub fn set_uid(mut self, uid: Uid) -> RuntimeConfig {
        self.uid = uid;
        self
    }

    pub fn set_gid(mut self, gid: Gid) -> RuntimeConfig {
        self.gid = gid;
        self
    }

    pub fn set_cwd(mut self, cwd: impl AsRef<Path>) -> RuntimeConfig {
        self.cwd = cwd.as_ref().to_path_buf();
        self
    }

    pub fn set_mounts(mut self, mounts: Vec<Mount>) -> RuntimeConfig {
        self.mounts = mounts;
        self
    }

    pub fn set_environment(mut self, environment: HashMap<String, String>) -> RuntimeConfig {
        self.environment = environment;
        self
    }

    pub fn set_output_config(mut self, config: OutputConfig) -> RuntimeConfig {
        self.output_config = Some(config);
        self
    }

    pub fn add_mount(mut self, mount: Mount) -> RuntimeConfig {
        self.mounts.push(mount);
        self
    }

    pub fn add_env_var(mut self, name: String, value: String) -> RuntimeConfig {
        self.environment.insert(name, value);
        self
    }

    pub fn root_user(mut self) -> RuntimeConfig {
        self.gid = Gid::from(0);
        self.uid = Uid::from(0);
        self
    }

    pub fn rw(mut self) -> RuntimeConfig {
        self.read_only = false;
        self
    }
}

impl RuntimeConfig {
    fn relative_rootfs_path(&self, path: &str) -> PathBuf {
        match path.to_string().strip_prefix("/") {
            Some(str) => self.rootfs_path.join(Path::new(str)),
            None => self.rootfs_path.join(path),
        }
    }

    pub fn run(&self, args: Vec<String>) -> Result<()> {
        stage1(self, args)
    }

    pub fn run_script(&self, language: impl AsRef<str>, script: impl AsRef<str>) -> Result<()> {
        match language.as_ref() {
            "sh" | "shell" | "bash" => self.run_shell(script),
            "py" | "python" => self.run_python(script),
            _ => bail!("Unknown runtime language `{}`", language.as_ref()),
        }
    }

    pub fn run_shell(&self, script: impl AsRef<str>) -> Result<()> {
        self.run(vec![String::from("bash"), String::from("-e"), String::from("-c"), script.as_ref().to_string()])
    }

    pub fn run_python(&self, script: impl AsRef<str>) -> Result<()> {
        self.run(vec![String::from("python3"), String::from("-c"), script.as_ref().to_string()])
    }
}
