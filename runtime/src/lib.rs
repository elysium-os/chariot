use std::{
    collections::HashMap,
    error::Error,
    ffi::{OsStr, OsString},
    fmt::{self, Debug, Display, Formatter},
    fs::canonicalize,
    io::{self, Write},
    path::{Path, PathBuf},
};

use nix::{
    errno::Errno,
    sys::wait::WaitStatus,
    unistd::{Gid, Uid},
};

use crate::runtime::runtime_execute_bare;

#[cfg(not(all(target_os = "linux")))]
compile_error!("Chariot runtime only supports linux.");

const FILESYSTEMS: &[(&str, &str)] = &[
    ("/dev/pts", "devpts"),
    ("/dev/shm", "tmpfs"),
    ("/run", "tmpfs"),
    ("/tmp", "tmpfs"),
    ("/proc", "proc"),
];

const BOUND_DEVICE_FILES: &[&str] = &["tty", "random", "urandom", "null", "zero", "full"];

const RESOLV_CONF_PATH: &str = "/etc/resolv.conf";

mod runtime;

#[derive(Debug, Clone)]
pub struct OverlayUpperDirectory {
    pub work_directory: PathBuf,
    pub upper_directory: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Overlay {
    pub upper_directory: Option<OverlayUpperDirectory>,
    pub lower_directories: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum MountKind {
    Bind { from: PathBuf, read_only: bool, is_file: bool },
    FS { fstype: String },
    OverlayFS(Overlay),
}

#[derive(Debug, Clone)]
pub struct Mount {
    pub dest: PathBuf,
    pub kind: MountKind,
}

#[derive(Debug)]
pub enum RuntimeError {
    Read { errno: Errno },
    Write { source: io::Error },
    Pipe { errno: Errno },
    Poll { errno: Errno },
    Fork { errno: Errno },
    WaitPID { errno: Errno },
    InvalidWaitStatus { status: WaitStatus },
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { errno } | Self::Pipe { errno } | Self::Poll { errno } | Self::Fork { errno } | Self::WaitPID { errno } => Some(errno),
            Self::Write { source } => Some(source),
            Self::InvalidWaitStatus { status: _ } => None,
        }
    }
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Read { .. } => write!(f, "Failed to read from output pipe"),
            RuntimeError::Write { .. } => write!(f, "Failed to write to log writer"),
            RuntimeError::Pipe { .. } => write!(f, "Failed to create output pipe"),
            RuntimeError::Poll { .. } => write!(f, "Failed to poll on output pipe"),
            RuntimeError::Fork { .. } => write!(f, "Failed to fork"),
            RuntimeError::WaitPID { .. } => write!(f, "WaitPID failed on child fork"),
            RuntimeError::InvalidWaitStatus { .. } => write!(f, "Runtime returned an invalid wait status"),
        }
    }
}

impl Overlay {
    fn data_string(&self) -> OsString {
        let mut data = OsString::new();
        data.push("lowerdir=");
        match self.lower_directories.first() {
            Some(dir) => data.push(&dir),
            None => panic!("Overlay must contain at least one lower directory"),
        }
        for lower_dir in self.lower_directories.iter().skip(1) {
            data.push(":");
            data.push(lower_dir);
        }
        if let Some(upper) = &self.upper_directory {
            data.push(",upperdir=");
            data.push(&upper.upper_directory);
            data.push(",workdir=");
            data.push(&upper.work_directory);
        }
        data.push(",userxattr");
        data
    }
}

pub fn runtime_execute(
    rootfs_path: impl AsRef<Path>,
    root_readonly: bool,
    uid: u32,
    gid: u32,
    cwd: impl AsRef<Path>,
    early_mounts: &Vec<&Mount>,
    late_mounts: &Vec<&Mount>,
    environment: &HashMap<impl AsRef<str>, impl AsRef<str>>,
    network_isolation: bool,
    logger: &mut dyn Write,
    args: Vec<impl AsRef<str>>,
) -> Result<i32, RuntimeError> {
    let mut default_env: HashMap<&OsStr, &OsStr> = HashMap::new();
    default_env.insert("HOME".as_ref(), cwd.as_ref().as_os_str());

    let mut default_env_add = |key: &'static str, value: &'static str| default_env.insert(key.as_ref(), value.as_ref());

    if uid == 0 {
        default_env_add("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin");
    } else {
        default_env_add("PATH", "/usr/local/bin:/usr/bin:/bin");
    }
    default_env_add("LD_LIBRARY_PATH", "/usr/local/lib64:/usr/local/lib:/usr/lib64:/usr/lib");
    default_env_add("LANG", "en_US.UTF-8");
    default_env_add("TERM", "xterm-256color");

    for (k, v) in environment.iter() {
        default_env.insert(k.as_ref().as_ref(), v.as_ref().as_ref());
    }

    let mut additional_mounts: Vec<Mount> = Vec::new();

    if !network_isolation {
        if let Ok(resolv_conf_path) = canonicalize(RESOLV_CONF_PATH) {
            additional_mounts.push(Mount {
                dest: PathBuf::from(RESOLV_CONF_PATH),
                kind: MountKind::Bind {
                    from: resolv_conf_path,
                    read_only: true,
                    is_file: true,
                },
            });
        }
    }

    for device_mount in BOUND_DEVICE_FILES {
        let path = PathBuf::from("/dev").join(device_mount);
        additional_mounts.push(Mount {
            dest: path.clone(),
            kind: MountKind::Bind {
                from: path,
                read_only: false,
                is_file: true,
            },
        });
    }

    for (path, fs) in FILESYSTEMS {
        additional_mounts.push(Mount {
            dest: PathBuf::from(path),
            kind: MountKind::FS { fstype: String::from(*fs) },
        });
    }

    let mut new_mounts: Vec<&Mount> = Vec::new();
    for mount in early_mounts {
        new_mounts.push(mount);
    }

    for mount in &additional_mounts {
        new_mounts.push(mount);
    }

    for mount in late_mounts {
        new_mounts.push(mount);
    }

    runtime_execute_bare(
        rootfs_path.as_ref(),
        root_readonly,
        Uid::from_raw(uid),
        Gid::from_raw(gid),
        cwd.as_ref(),
        new_mounts,
        default_env,
        network_isolation,
        logger,
        args,
    )
}
