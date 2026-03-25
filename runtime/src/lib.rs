use std::{
    collections::HashMap,
    env,
    error::Error,
    ffi::{CString, OsString},
    fmt::Display,
    fs::{File, create_dir_all, exists, metadata, remove_dir, remove_file, write},
    io::Write,
    os::fd::{AsFd, AsRawFd},
    panic,
    path::{Component, Path, PathBuf},
    process::exit,
};

use nix::{
    errno::Errno,
    mount::{MsFlags, mount},
    poll::{PollFd, PollFlags, poll},
    sched::{CloneFlags, unshare},
    sys::wait::{WaitPidFlag, WaitStatus, waitpid},
    unistd::{ForkResult, Gid, Uid, chdir, chroot, close, dup2_stderr, dup2_stdout, execvp, fork, getegid, geteuid, pipe, read, setgid, setuid},
};

const DEFAULT_DEVICE_FILES: &[&str] = &["tty", "random", "urandom", "null", "zero", "full"];

pub enum Mount {
    Bind {
        from: PathBuf,
        to: PathBuf,
        read_only: bool,
        is_file: bool,
    },
    TmpFS {
        dest: PathBuf,
    },
}

pub struct Overlay {
    pub overlay_path: PathBuf,
    pub work_dir: PathBuf,
}

#[derive(Debug)]
pub enum RuntimeError {
    Fork { errno: Errno },
    WaitPID { errno: Errno },
    InvalidWaitStatus { status: WaitStatus },
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Fork { errno } | Self::WaitPID { errno } => return Some(errno),
            Self::InvalidWaitStatus { status: _ } => None,
        }
    }
}

impl Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            RuntimeError::Fork { errno: _ } => format!("Fork failed"),
            RuntimeError::WaitPID { errno: _ } => format!("WaitPID failed"),
            RuntimeError::InvalidWaitStatus { status: _ } => format!("Runtime returned an invalid wait status"),
        };

        f.write_str(&str)
    }
}

pub fn runtime_execute(
    rootfs_path: impl AsRef<Path>,
    rootfs_read_only: bool,
    rootfs_overlay: Option<Overlay>,
    uid: u32,
    gid: u32,
    cwd: impl AsRef<Path>,
    mounts: Vec<&Mount>,
    environment: HashMap<&str, &str>,
    network_isolation: bool,
    log_writers: Vec<&mut dyn Write>,
    args: Vec<impl AsRef<str>>,
) -> Result<i32, RuntimeError> {
    let fork_result = unsafe { fork() }.map_err(|errno| RuntimeError::Fork { errno: errno })?;
    match fork_result {
        ForkResult::Parent { child: init_pid } => {
            let i = waitpid(init_pid, None).map_err(|errno| RuntimeError::WaitPID { errno })?;
            match i {
                WaitStatus::Exited(_, code) => Ok(code),
                status => Err(RuntimeError::InvalidWaitStatus { status }),
            }
        }
        ForkResult::Child => child(
            rootfs_path.as_ref(),
            rootfs_read_only,
            rootfs_overlay,
            network_isolation,
            Uid::from(uid),
            Gid::from(gid),
            cwd.as_ref(),
            mounts,
            environment,
            log_writers,
            args.iter().map(|arg| arg.as_ref().to_string()).collect(),
        ),
    }
}

fn relative_rootfs_path(rootfs_path: &Path, path: impl AsRef<Path>) -> PathBuf {
    let mut rootfs_relative_path = PathBuf::from(rootfs_path);

    for component in path.as_ref().components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => rootfs_relative_path.push(".."),
            Component::Normal(component) => rootfs_relative_path.push(component),
        }
    }

    rootfs_relative_path
}

fn ensure_dir(rootfs_path: &Path, path: impl AsRef<Path>) -> PathBuf {
    let dir = relative_rootfs_path(rootfs_path, path);
    create_dir_all(&dir).unwrap_or_else(|err| panic!("unable to create {}: {}", dir.to_string_lossy(), err));
    dir
}

fn device_path(rootfs_path: &Path, device_name: &str) -> PathBuf {
    relative_rootfs_path(rootfs_path, "/dev").join(device_name)
}

fn child(
    rootfs_path: &Path,
    rootfs_read_only: bool,
    rootfs_overlay: Option<Overlay>,
    network_isolation: bool,
    uid: Uid,
    gid: Gid,
    cwd: &Path,
    mounts: Vec<&Mount>,
    environment: HashMap<&str, &str>,
    mut log_writers: Vec<&mut dyn Write>,
    args: Vec<String>,
) -> ! {
    panic::set_hook(Box::new(|info| {
        eprintln!("Chariot runtime panic `{}`", info);
        exit(1);
    }));

    let euid = geteuid();
    let egid = getegid();

    unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWPID).expect("USER/PID unshare failed");

    write("/proc/self/setgroups", "deny").expect("setgroups write failed");
    write("/proc/self/uid_map", format!("{} {} 1", uid, euid)).expect("uid_map write failed");
    write("/proc/self/gid_map", format!("{} {} 1", gid, egid)).expect("gid_map write failed");

    setuid(uid).expect("setuid failed");
    setgid(gid).expect("setgid failed");

    let fork_result = unsafe { fork() }.expect("second fork failed");
    if let ForkResult::Parent { child: child_pid } = fork_result {
        let status = waitpid(child_pid, None).expect("second waitpid failed");
        if let WaitStatus::Exited(_, code) = status {
            exit(code);
        }
        panic!("runtime child process failed");
    }

    let mut clone_flags = CloneFlags::CLONE_NEWNS;
    if network_isolation {
        clone_flags |= CloneFlags::CLONE_NEWNET;
    }
    unshare(clone_flags).expect("unshare failed");

    mount(Some(rootfs_path), rootfs_path, None::<&str>, MsFlags::MS_BIND, None::<&str>).expect("rootfs mount failed");

    ensure_dir(rootfs_path, "/dev");
    for dev in DEFAULT_DEVICE_FILES {
        let dev_path = device_path(rootfs_path, dev);
        File::create(&dev_path).unwrap_or_else(|err| panic!("{:?} creation failed: {}", dev_path, err));
    }

    for mount in &mounts {
        let (is_file, dest) = match mount {
            Mount::Bind { to, is_file, .. } => (*is_file, to),
            Mount::TmpFS { dest } => (false, dest),
        };

        let path = relative_rootfs_path(rootfs_path, dest);
        if exists(&path).expect("mount path exists failed") {
            let meta = metadata(&path).expect("mount path metadata failed");
            if is_file && !meta.is_file() {
                remove_dir(&path).expect("mount path remove_dir failed");
            } else if !is_file && !meta.is_dir() {
                remove_file(&path).expect("mount path remove_file failed");
            }
        }

        if is_file {
            if let Some(parent) = path.parent() {
                create_dir_all(parent).expect("mount path parent creation failed");
            }
            File::create(&path).expect("mount path file creation failed");
        } else {
            create_dir_all(&path).expect("mount path dir creation failed");
        }
    }

    if let Some(overlay) = rootfs_overlay.as_ref() {
        let mut overlay_data = OsString::new();
        overlay_data.push("lowerdir=");
        overlay_data.push(rootfs_path);
        overlay_data.push(",upperdir=");
        overlay_data.push(&overlay.overlay_path);
        overlay_data.push(",workdir=");
        overlay_data.push(&overlay.work_dir);
        overlay_data.push(",userxattr");

        mount(
            Some("overlay"),
            rootfs_path,
            Some("overlay"),
            MsFlags::empty(),
            Some(overlay_data.as_os_str()),
        )
        .expect("overlay mount failed");
    }

    let mut remount_flags = MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_NODEV | MsFlags::MS_NOSUID;
    if rootfs_read_only {
        remount_flags |= MsFlags::MS_RDONLY;
    }
    mount(Some(rootfs_path), rootfs_path, None::<&str>, remount_flags, None::<&str>).expect("rootfs readonly remount failed");

    for dev in DEFAULT_DEVICE_FILES {
        let host_device = Path::new("/dev").join(dev);
        let dest = device_path(rootfs_path, dev);
        mount(Some(&host_device), dest.as_path(), None::<&str>, MsFlags::MS_BIND, None::<&str>).expect("device mount failed");
    }

    if !network_isolation {
        let host_resolv = std::fs::canonicalize("/etc/resolv.conf").expect("resolv.conf canonicalize failed");
        let dest = relative_rootfs_path(rootfs_path, "/etc/resolv.conf");
        mount(Some(&host_resolv), dest.as_path(), None::<&str>, MsFlags::MS_BIND, None::<&str>).expect("resolv.conf mount failed");
    }

    let pts_path = ensure_dir(rootfs_path, "/dev/pts");
    mount(None::<&str>, pts_path.as_path(), Some("devpts"), MsFlags::empty(), None::<&str>).expect("/dev/pts mount failed");

    let shm_path = ensure_dir(rootfs_path, "/dev/shm");
    mount(None::<&str>, shm_path.as_path(), Some("tmpfs"), MsFlags::empty(), None::<&str>).expect("/dev/shm mount failed");

    let run_path = ensure_dir(rootfs_path, "/run");
    mount(None::<&str>, run_path.as_path(), Some("tmpfs"), MsFlags::empty(), None::<&str>).expect("/run mount failed");

    let tmp_path = ensure_dir(rootfs_path, "/tmp");
    mount(None::<&str>, tmp_path.as_path(), Some("tmpfs"), MsFlags::empty(), None::<&str>).expect("/tmp mount failed");

    let proc_path = ensure_dir(rootfs_path, "/proc");
    mount(None::<&str>, proc_path.as_path(), Some("proc"), MsFlags::empty(), None::<&str>).expect("/proc mount failed");

    for mount_config in &mounts {
        match mount_config {
            Mount::Bind {
                from,
                to,
                read_only,
                is_file,
            } => {
                let mut flags = MsFlags::MS_BIND;
                if !is_file {
                    flags |= MsFlags::MS_REC;
                }

                let target = relative_rootfs_path(rootfs_path, to);
                mount(Some(from), &target, None::<&str>, flags, None::<&str>).expect("configured mount failed");
                if *read_only {
                    mount(
                        Some(from),
                        &target,
                        None::<&str>,
                        flags | MsFlags::MS_RDONLY | MsFlags::MS_REMOUNT,
                        None::<&str>,
                    )
                    .expect("configured readonly remount failed");
                }
            }
            Mount::TmpFS { dest } => {
                mount(
                    None::<&str>,
                    &relative_rootfs_path(rootfs_path, dest),
                    Some("tmpfs"),
                    MsFlags::empty(),
                    None::<&str>,
                )
                .expect("configured tmpfs mount failed");
            }
        }
    }

    chroot(rootfs_path).expect("chroot failed");
    chdir(cwd).expect("chdir failed");

    let output_pipe = pipe().expect("log pipe creation failed");

    match unsafe { fork() }.expect("third fork failed") {
        ForkResult::Child => {
            dup2_stdout(output_pipe.1.as_fd()).expect("dup2 stdout failed");
            dup2_stderr(output_pipe.1.as_fd()).expect("dup2 stderr failed");

            let existing_vars: Vec<String> = env::vars().map(|(name, _)| name).collect();
            unsafe {
                for name in existing_vars {
                    env::remove_var(name);
                }

                if uid.as_raw() == 0 {
                    env::set_var("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin");
                } else {
                    env::set_var("PATH", "/usr/local/bin:/usr/bin:/bin");
                }
                env::set_var("LD_LIBRARY_PATH", "/usr/local/lib64:/usr/local/lib:/usr/lib64:/usr/lib");
                env::set_var("HOME", cwd);
                env::set_var("LANG", "en_US.UTF-8");
                env::set_var("TERM", "xterm-256color");

                for (name, value) in environment.iter() {
                    env::set_var(name, value);
                }
            }

            let exec_result = execvp(
                &CString::new(args[0].as_str()).unwrap(),
                &args.iter().map(|a| CString::new(a.as_str()).unwrap()).collect::<Vec<_>>(),
            );

            eprintln!("error while executing program: {}", exec_result.unwrap_err());
            exit(1);
        }
        ForkResult::Parent { child: init_pid } => {
            close(output_pipe.1.as_raw_fd()).expect("close stdout_write_fd failed");

            let mut buffer = [0u8; 1024];
            let mut poll_fds = [PollFd::new(output_pipe.0.as_fd(), PollFlags::POLLIN)];
            loop {
                match waitpid(init_pid, Some(WaitPidFlag::WNOHANG)).expect("waitpid failed") {
                    WaitStatus::StillAlive => {}
                    status => {
                        if let WaitStatus::Exited(_, code) = status {
                            exit(code);
                        }
                        panic!("runtime process failed: {:?}", status);
                    }
                }

                let n = poll(&mut poll_fds, 300_u16).expect("poll failed");
                if n == 0 {
                    continue;
                }

                if !poll_fds[0].revents().unwrap().contains(PollFlags::POLLIN) {
                    continue;
                }

                let count = read(output_pipe.0.as_fd(), &mut buffer).expect("pipe read failed");
                if count == 0 {
                    continue;
                }

                for writer in log_writers.iter_mut() {
                    writer.write_all(&buffer[..count]).expect("log writer failed");
                }
            }
        }
    };
}
