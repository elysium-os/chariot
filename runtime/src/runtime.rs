use std::{
    collections::HashMap,
    env,
    ffi::{CString, OsStr},
    fs::{File, create_dir_all, exists, metadata, remove_dir, remove_file, write},
    io::Write,
    os::fd::{AsFd, OwnedFd},
    panic,
    path::{Component, Path, PathBuf},
    process::exit,
};

use nix::{
    mount::{MsFlags, mount},
    poll::{PollFd, PollFlags, poll},
    sched::{CloneFlags, unshare},
    sys::wait::{WaitPidFlag, WaitStatus, waitpid},
    unistd::{ForkResult, Gid, Uid, chdir, chroot, dup2_stderr, dup2_stdout, execvp, fork, getegid, geteuid, pipe, read, setgid, setuid},
};

use crate::{Mount, MountKind, RuntimeError};

pub(super) fn runtime_execute_bare(
    rootfs_path: impl AsRef<Path>,
    rootfs_readonly: bool,
    uid: Uid,
    gid: Gid,
    cwd: impl AsRef<Path>,
    mounts: Vec<&Mount>,
    environment: HashMap<impl AsRef<OsStr>, impl AsRef<OsStr>>,
    network_isolation: bool,
    logger: &mut dyn Write,
    args: Vec<impl AsRef<str>>,
) -> Result<i32, RuntimeError> {
    let output_pipe = pipe().map_err(|errno| RuntimeError::Pipe { errno })?;

    let fork_result = unsafe { fork() }.map_err(|errno| RuntimeError::Fork { errno })?;
    match fork_result {
        ForkResult::Parent { child: child_pid } => {
            let mut buffer = [0; 1024];
            let mut poll_fds = [PollFd::new(output_pipe.0.as_fd(), PollFlags::POLLIN)];
            loop {
                match waitpid(child_pid, Some(WaitPidFlag::WNOHANG)).map_err(|errno| RuntimeError::WaitPID { errno })? {
                    WaitStatus::StillAlive => {}
                    WaitStatus::Exited(_, code) => return Ok(code),
                    status => return Err(RuntimeError::InvalidWaitStatus { status }),
                }

                let n = poll(&mut poll_fds, 300_u16).map_err(|errno| RuntimeError::Poll { errno })?;
                if n == 0 {
                    continue;
                }

                let pollin = poll_fds[0].revents().and_then(|flags| Some(flags.contains(PollFlags::POLLIN)));
                if matches!(pollin, None | Some(false)) {
                    continue;
                }

                let count = read(output_pipe.0.as_fd(), &mut buffer).map_err(|errno| RuntimeError::Read { errno })?;
                if count == 0 {
                    continue;
                }

                logger.write_all(&buffer[..count]).map_err(|err| RuntimeError::Write { source: err })?;
            }
        }
        ForkResult::Child => child(
            rootfs_path,
            rootfs_readonly,
            network_isolation,
            uid,
            gid,
            cwd.as_ref(),
            mounts,
            environment,
            args.iter().map(|arg| arg.as_ref().to_string()).collect(),
            output_pipe.1,
        ),
    }
}

fn child(
    rootfs_path: impl AsRef<Path>,
    rootfs_readonly: bool,
    network_isolation: bool,
    uid: Uid,
    gid: Gid,
    cwd: &Path,
    mounts: Vec<&Mount>,
    environment: HashMap<impl AsRef<OsStr>, impl AsRef<OsStr>>,
    args: Vec<String>,
    output_fd: OwnedFd,
) -> ! {
    panic::set_hook(Box::new(|info| {
        eprintln!(
            "Chariot runtime (child process) panic `{}`",
            info.payload_as_str().unwrap_or("no message")
        );
        exit(1);
    }));

    let euid = geteuid();
    let egid = getegid();

    unshare(CloneFlags::CLONE_NEWUSER).expect("unshare user failed");

    write("/proc/self/setgroups", "deny").expect("setgroups write failed");
    write("/proc/self/uid_map", format!("{} {} 1", uid, euid)).expect("uid_map write failed");
    write("/proc/self/gid_map", format!("{} {} 1", gid, egid)).expect("gid_map write failed");

    setuid(uid).expect("setuid failed");
    setgid(gid).expect("setgid failed");

    unshare(CloneFlags::CLONE_NEWPID).expect("unshare pid failed");

    let fork_result = unsafe { fork() }.expect("init process fork failed");
    match fork_result {
        ForkResult::Parent { child: child_pid } => {
            let status = waitpid(child_pid, None).expect("waitpid failed");

            if let WaitStatus::Exited(_, code) = status {
                exit(code);
            }

            panic!("waitpid returned invalid wait status");
        }
        ForkResult::Child => init(rootfs_path, rootfs_readonly, network_isolation, cwd, mounts, environment, args, output_fd),
    }
}

fn init(
    rootfs_path: impl AsRef<Path>,
    rootfs_readonly: bool,
    network_isolation: bool,
    cwd: &Path,
    mounts: Vec<&Mount>,
    environment: HashMap<impl AsRef<OsStr>, impl AsRef<OsStr>>,
    args: Vec<String>,
    output_fd: OwnedFd,
) -> ! {
    panic::set_hook(Box::new(|info| {
        eprintln!("Chariot runtime (init process) panic `{}`", info.payload_as_str().unwrap_or("no message"));
        exit(1);
    }));

    unshare(CloneFlags::CLONE_NEWNS).expect("unshare mounts failed");
    if network_isolation {
        unshare(CloneFlags::CLONE_NEWNET).expect("unshare network failed");
    }

    // Helpers
    let relative_rootfs_path = |path: &Path| -> PathBuf {
        let mut rootfs_relative_path = rootfs_path.as_ref().to_path_buf();

        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
                Component::ParentDir => rootfs_relative_path.push(".."),
                Component::Normal(component) => rootfs_relative_path.push(component),
            }
        }

        rootfs_relative_path
    };

    let prepare_mount = |mount: &Mount| {
        let is_file = match mount.kind {
            MountKind::Bind { is_file, .. } => is_file,
            _ => false,
        };

        let path = relative_rootfs_path(&mount.dest);
        if exists(&path).expect("prepare_mount failed: exists failed") {
            let meta = metadata(&path).expect("prepare_mount failed: metadata failed");
            if is_file && !meta.is_file() {
                remove_dir(&path).expect("prepare_mount failed: remove_dir failed");
            } else if !is_file && !meta.is_dir() {
                remove_file(&path).expect("prepare_mount failed: remove_file failed");
            }
        }

        if is_file {
            if let Some(parent) = path.parent() {
                create_dir_all(parent).expect("prepare_mount failed: parent creation failed");
            }
            File::create(&path).expect("prepare_mount failed: file creation failed");
        } else {
            create_dir_all(&path).expect("prepare_mount failed: dir creation failed");
        }
    };

    let do_mount = |mount_config: &Mount| match &mount_config.kind {
        MountKind::Bind { from, read_only, is_file } => {
            let mut flags = MsFlags::MS_BIND;
            if !is_file {
                flags |= MsFlags::MS_REC;
            }

            let target = relative_rootfs_path(&mount_config.dest);
            mount(Some(from), &target, None::<&str>, flags, None::<&str>).expect("configured bind mount failed");
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
        MountKind::FS { fstype } => {
            mount(
                None::<&str>,
                &relative_rootfs_path(&mount_config.dest),
                Some(fstype.as_str()),
                MsFlags::empty(),
                None::<&str>,
            )
            .expect("configured tmpfs mount failed");
        }
        MountKind::OverlayFS(overlay) => {
            mount(
                Some("overlay"),
                &relative_rootfs_path(&mount_config.dest),
                Some("overlay"),
                MsFlags::empty(),
                Some(overlay.data_string().as_os_str()),
            )
            .expect("configured overlayfs mount failed");
        }
    };

    // Create mount files & directories
    for device_mount in &mounts {
        prepare_mount(&device_mount);
    }

    // Mount rootfs as read-only
    mount(
        Some(rootfs_path.as_ref()),
        rootfs_path.as_ref(),
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .expect("rootfs mount failed");
    let mut remount_flags = MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_NODEV | MsFlags::MS_NOSUID;
    if rootfs_readonly {
        remount_flags |= MsFlags::MS_RDONLY;
    }
    mount(
        Some(rootfs_path.as_ref()),
        rootfs_path.as_ref(),
        None::<&str>,
        remount_flags,
        None::<&str>,
    )
    .expect("rootfs remount failed");

    // Create mounts
    for mount in mounts {
        do_mount(mount);
    }

    // Enter rootfs
    chroot(rootfs_path.as_ref()).expect("chroot failed");
    chdir(cwd).expect("cwd chdir failed");

    // Run program
    match unsafe { fork() }.expect("program fork failed") {
        ForkResult::Parent { child: child_pid } => {
            let status = waitpid(child_pid, None).expect("waitpid failed");

            if let WaitStatus::Exited(_, code) = status {
                exit(code);
            }

            panic!("waitpid returned invalid wait status");
        }
        ForkResult::Child => program(environment, args, output_fd),
    };
}

fn program(environment: HashMap<impl AsRef<OsStr>, impl AsRef<OsStr>>, args: Vec<String>, output_fd: OwnedFd) -> ! {
    panic::set_hook(Box::new(|info| {
        eprintln!(
            "Chariot runtime (program process) panic `{}`",
            info.payload_as_str().unwrap_or("no message")
        );
        exit(1);
    }));

    dup2_stdout(output_fd.as_fd()).expect("dup2 stdout failed");
    dup2_stderr(output_fd.as_fd()).expect("dup2 stderr failed");

    for name in env::vars().map(|(name, _)| name).collect::<Vec<_>>() {
        unsafe {
            env::remove_var(name);
        }
    }

    for (name, value) in environment.iter() {
        unsafe {
            env::set_var(name, value);
        }
    }

    let exec_result = execvp(
        &CString::new(args[0].as_str()).unwrap(),
        &args.iter().map(|a| CString::new(a.as_str()).unwrap()).collect::<Vec<_>>(),
    );

    panic!("error while executing program: {}", exec_result.unwrap_err());
}
