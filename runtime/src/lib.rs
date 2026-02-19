use std::{
    collections::HashMap,
    env,
    ffi::{CString, OsString},
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
    unistd::{
        ForkResult, Gid, Uid, chdir, chroot, close, dup2_stderr, dup2_stdout, execvp, fork,
        getegid, geteuid, pipe, read, setgid, setuid,
    },
};

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
    NonZeroExit { code: i32 },
}

pub fn runtime_execute(
    rootfs_path: impl AsRef<Path>,
    rootfs_read_only: bool,
    rootfs_overlay: Option<Overlay>,
    uid: u32,
    gid: u32,
    cwd: impl AsRef<Path>,
    mounts: Vec<Mount>,
    environment: HashMap<String, String>,
    network_isolation: bool,
    log_writers: Vec<&mut dyn Write>,
    args: Vec<String>,
) -> Result<(), RuntimeError> {
    let fork_result = unsafe { fork() }.map_err(|errno| RuntimeError::Fork { errno: errno })?;
    match fork_result {
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
            args,
        ),
        ForkResult::Parent { child: init_pid } => {
            let i = waitpid(init_pid, None).map_err(|errno| RuntimeError::WaitPID { errno })?;
            match i {
                WaitStatus::Exited(_, code) => {
                    if code == 0 {
                        return Ok(());
                    }
                    return Err(RuntimeError::NonZeroExit { code });
                }
                status => return Err(RuntimeError::InvalidWaitStatus { status }),
            }
        }
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

fn child(
    rootfs_path: &Path,
    rootfs_read_only: bool,
    rootfs_overlay: Option<Overlay>,
    network_isolation: bool,
    uid: Uid,
    gid: Gid,
    cwd: &Path,
    mounts: Vec<Mount>,
    environment: HashMap<String, String>,
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

    mount(
        Some(rootfs_path),
        rootfs_path,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .expect("rootfs mount failed");

    let devices = vec!["tty", "random", "urandom", "null", "zero", "full"];
    for dev in &devices {
        let dev_path = relative_rootfs_path(rootfs_path, "/dev").join(dev);
        File::create(&dev_path).expect(format!("{:?} creation failed", dev_path).as_str());
    }

    let pts_path = &relative_rootfs_path(rootfs_path, "/dev/pts");
    create_dir_all(pts_path).expect("/dev/pts creation failed");

    let shm_path = &relative_rootfs_path(rootfs_path, "/dev/shm");
    create_dir_all(shm_path).expect("/dev/shm creation failed");

    for mount in mounts.iter() {
        let (is_file, dest) = match mount {
            Mount::Bind {
                from: _,
                to,
                read_only: _,
                is_file,
            } => (*is_file, to),
            Mount::TmpFS { dest } => (false, dest),
        };

        let path = relative_rootfs_path(rootfs_path, dest);
        if exists(&path).expect("mount path exists failed") {
            let meta = metadata(&path).expect("mount path metadata failed");
            if is_file {
                if !meta.is_file() {
                    remove_dir(&path).expect("mount path remove_dir failed");
                }
            } else {
                if !meta.is_dir() {
                    remove_file(&path).expect("mount path remove_file failed");
                }
            }
        }

        if is_file {
            File::create(&path).expect("mount path file creation failed");
        } else {
            create_dir_all(&path).expect("mount path dir creation failed");
        }
    }

    if let Some(overlay) = rootfs_overlay {
        let mut overlay_data = OsString::new();
        overlay_data.push("lowerdir=");
        overlay_data.push(rootfs_path);
        overlay_data.push(",upperdir=");
        overlay_data.push(overlay.overlay_path);
        overlay_data.push(",workdir=");
        overlay_data.push(overlay.work_dir);
        overlay_data.push(",userxattr");

        mount(
            Some("overlay"),
            rootfs_path,
            Some("overlay"),
            MsFlags::empty(),
            Some(overlay_data.as_os_str()),
        )
        .expect("overlay test failed");
    }

    let mut remount_flags =
        MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_NODEV | MsFlags::MS_NOSUID;
    if rootfs_read_only {
        remount_flags |= MsFlags::MS_RDONLY;
    }
    mount(
        Some(rootfs_path),
        rootfs_path,
        None::<&str>,
        remount_flags,
        None::<&str>,
    )
    .expect("rootfs readonly remount failed");

    for dev in devices {
        mount(
            Some(&Path::new("/dev").join(dev)),
            relative_rootfs_path(rootfs_path, "/dev")
                .join(dev)
                .to_str()
                .unwrap(),
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        )
        .expect("device mount failed")
    }

    if !network_isolation {
        mount(
            Some(&std::fs::canonicalize("/etc/resolv.conf").unwrap()),
            &relative_rootfs_path(rootfs_path, "/etc/resolv.conf"),
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        )
        .expect("resolv.conf mount failed");
    }

    mount(
        None::<&str>,
        pts_path,
        Some("devpts"),
        MsFlags::empty(),
        None::<&str>,
    )
    .expect("/dev/pts mount failed");
    mount(
        None::<&str>,
        shm_path,
        Some("tmpfs"),
        MsFlags::empty(),
        None::<&str>,
    )
    .expect("/dev/shm mount failed");
    mount(
        None::<&str>,
        &relative_rootfs_path(rootfs_path, "/run"),
        Some("tmpfs"),
        MsFlags::empty(),
        None::<&str>,
    )
    .expect("/run mount failed");
    mount(
        None::<&str>,
        &relative_rootfs_path(rootfs_path, "/tmp"),
        Some("tmpfs"),
        MsFlags::empty(),
        None::<&str>,
    )
    .expect("/tmp mount failed");
    mount(
        None::<&str>,
        &relative_rootfs_path(rootfs_path, "/proc"),
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )
    .expect("/proc mount failed");

    for m in mounts.iter() {
        match m {
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
                if *read_only {
                    mount(
                        Some(from),
                        &relative_rootfs_path(rootfs_path, to),
                        None::<&str>,
                        flags,
                        None::<&str>,
                    )
                    .expect("configured first rw mount failed");
                    flags |= MsFlags::MS_RDONLY | MsFlags::MS_REMOUNT;
                }
                mount(
                    Some(from),
                    &relative_rootfs_path(rootfs_path, to),
                    None::<&str>,
                    flags,
                    None::<&str>,
                )
                .expect("configured mount failed");
            }
            Mount::TmpFS { dest } => {
                mount(
                    None::<&str>,
                    &relative_rootfs_path(rootfs_path, dest),
                    Some("tmpfs"),
                    MsFlags::empty(),
                    None::<&str>,
                )
                .expect("configured mount failed");
            }
        }
    }

    chroot(rootfs_path).expect("chroot failed");
    chdir(cwd).expect("chdir failed");

    let output_pipe = pipe().expect("log pipe creation failed");

    let fork_result = unsafe { fork() }.expect("third fork failed");
    match fork_result {
        ForkResult::Child => {
            dup2_stdout(output_pipe.1.as_fd()).expect("dup2 stdout failed");
            dup2_stderr(output_pipe.1.as_fd()).expect("dup2 stderr failed");

            unsafe {
                for v in env::vars() {
                    env::remove_var(v.0);
                }

                if uid.as_raw() == 0 {
                    env::set_var("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin");
                } else {
                    env::set_var("PATH", "/usr/local/bin:/usr/bin:/bin");
                }
                env::set_var(
                    "LD_LIBRARY_PATH",
                    "/usr/local/lib64:/usr/local/lib:/usr/lib64:/usr/lib",
                );
                env::set_var("HOME", &cwd);
                env::set_var("LANG", "C");
                env::set_var("LC_COLLATE", "C");
                env::set_var("TERM", "xterm-256color");

                for (name, value) in environment.iter() {
                    env::set_var(name, value);
                }
            }

            let exec_result = execvp(
                &CString::new(args[0].as_str()).unwrap(),
                &args
                    .iter()
                    .map(|a| CString::new(a.as_str()).unwrap())
                    .collect::<Vec<_>>(),
            );

            eprintln!(
                "error while executing program: {}",
                exec_result.unwrap_err()
            );
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

                for writer in &mut log_writers {
                    writer.write(&buffer[..count]).unwrap();
                }
            }
        }
    };
}
