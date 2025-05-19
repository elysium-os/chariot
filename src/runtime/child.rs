use std::{
    env,
    ffi::CString,
    fs::{File, create_dir_all, exists, metadata, remove_dir, remove_file, write},
    io::{self, Write},
    os::fd::{AsFd, AsRawFd},
    panic,
    path::Path,
    process::exit,
};

use anyhow::{Context, Result, bail};
use log::error;
use nix::{
    libc::{STDERR_FILENO, STDOUT_FILENO},
    mount::{MsFlags, mount},
    poll::{PollFd, PollFlags, poll},
    sched::{CloneFlags, unshare},
    sys::wait::{WaitPidFlag, WaitStatus, wait, waitpid},
    unistd::{ForkResult, chdir, chroot, close, dup2, execvp, fork, getegid, geteuid, pipe, read, setgid, setuid},
};

use super::RuntimeConfig;

pub fn stage1(config: &RuntimeConfig, args: Vec<String>) -> Result<()> {
    let mut log_file = None;
    if let Some(config_output) = &config.output_config {
        if let Some(path) = &config_output.log_path {
            log_file = match File::create(path) {
                Err(e) => {
                    error!("Failed to create log file: {}", e);
                    None
                }
                Ok(f) => Some(f),
            }
        }
    }

    let fork_result = unsafe { fork() }.context("Failed to fork")?;
    match fork_result {
        ForkResult::Child => stage2(config, args, log_file),
        ForkResult::Parent { child: init_pid } => {
            let i = waitpid(init_pid, None).context("Failed to waitpid")?;
            match i {
                WaitStatus::Exited(_, code) => {
                    if code == 0 {
                        return Ok(());
                    }
                    bail!("Runtime exited with non-zero error code `{}`", code);
                }
                _ => bail!("Runtime process failed"),
            }
        }
    }
}

fn stage2(config: &RuntimeConfig, args: Vec<String>, log_file: Option<File>) -> ! {
    panic::set_hook(Box::new(|info| {
        eprintln!("Chariot runtime panic `{}`", info);
        exit(1);
    }));

    let euid = geteuid();
    let egid = getegid();

    unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWPID).expect("USER/PID unshare failed");

    write("/proc/self/setgroups", "deny").expect("setgroups write failed");
    write("/proc/self/uid_map", format!("{} {} 1", config.uid, euid)).expect("uid_map write failed");
    write("/proc/self/gid_map", format!("{} {} 1", config.gid, egid)).expect("gid_map write failed");

    setuid(config.uid).expect("setuid failed");
    setgid(config.gid).expect("setgid failed");

    let fork_result = unsafe { fork() }.expect("second fork failed");
    match fork_result {
        ForkResult::Child => stage3(config, args, log_file),
        ForkResult::Parent { child: child_pid } => {
            let status = waitpid(child_pid, None).expect("second waitpid failed");
            if let WaitStatus::Exited(_, code) = status {
                exit(code);
            }
            panic!("runtime child process failed");
        }
    }
}

fn stage3(config: &RuntimeConfig, args: Vec<String>, mut log_file: Option<File>) -> ! {
    let mut clone_flags = CloneFlags::CLONE_NEWNS;
    if config.network_isolation {
        clone_flags |= CloneFlags::CLONE_NEWNS;
    }
    unshare(clone_flags).expect("unshare failed");

    mount(Some(&config.rootfs_path), &config.rootfs_path, None::<&str>, MsFlags::MS_BIND, None::<&str>).expect("rootfs mount failed");

    let devices = vec!["tty", "random", "urandom", "null", "zero", "full"];
    for dev in &devices {
        let dev_path = config.relative_rootfs_path("/dev").join(dev);
        File::create(&dev_path).expect(format!("{:?} creation failed", dev_path).as_str());
    }

    let pts_path = &config.relative_rootfs_path("/dev/pts");
    create_dir_all(pts_path).expect("/dev/pts creation failed");

    let shm_path = &config.relative_rootfs_path("/dev/shm");
    create_dir_all(shm_path).expect("/dev/shm creation failed");

    for mount in config.mounts.iter() {
        let path = config.relative_rootfs_path(&mount.to.to_str().unwrap());
        if exists(&path).expect("mount path exists failed") {
            let meta = metadata(&path).expect("mount path metadata failed");
            if mount.is_file {
                if !meta.is_file() {
                    remove_dir(&path).expect("mount path remove_dir failed");
                }
            } else {
                if !meta.is_dir() {
                    remove_file(&path).expect("mount path remove_file failed");
                }
            }
        }

        if mount.is_file {
            File::create(&path).expect("mount path file creation failed");
        } else {
            create_dir_all(&path).expect("mount path dir creation failed");
        }
    }

    let mut remount_flags = MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_NODEV | MsFlags::MS_NOSUID;
    if config.read_only {
        remount_flags |= MsFlags::MS_RDONLY;
    }
    mount(Some(&config.rootfs_path), &config.rootfs_path, None::<&str>, remount_flags, None::<&str>).expect("rootfs readonly remount failed");

    for dev in devices {
        mount(
            Some(&Path::new("/dev").join(dev)),
            config.relative_rootfs_path("/dev").join(dev).to_str().unwrap(),
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        )
        .expect("device mount failed")
    }

    if !config.network_isolation {
        mount(
            Some(&std::fs::canonicalize("/etc/resolv.conf").unwrap()),
            &config.relative_rootfs_path("/etc/resolv.conf"),
            None::<&str>,
            MsFlags::MS_BIND,
            None::<&str>,
        )
        .expect("resolv.conf mount failed");
    }

    mount(None::<&str>, pts_path, Some("devpts"), MsFlags::empty(), None::<&str>).expect("/dev/pts mount failed");
    mount(None::<&str>, shm_path, Some("tmpfs"), MsFlags::empty(), None::<&str>).expect("/dev/shm mount failed");
    mount(None::<&str>, &config.relative_rootfs_path("/run"), Some("tmpfs"), MsFlags::empty(), None::<&str>).expect("/run mount failed");
    mount(None::<&str>, &config.relative_rootfs_path("/tmp"), Some("tmpfs"), MsFlags::empty(), None::<&str>).expect("/tmp mount failed");
    mount(None::<&str>, &config.relative_rootfs_path("/proc"), Some("proc"), MsFlags::empty(), None::<&str>).expect("/proc mount failed");

    for m in config.mounts.iter() {
        let mut flags = MsFlags::MS_BIND;
        if !m.is_file {
            flags |= MsFlags::MS_REC;
        }
        if m.read_only {
            mount(Some(&m.from), &config.relative_rootfs_path(&m.to.to_str().unwrap()), None::<&str>, flags, None::<&str>).expect("configured first rw mount failed");
            flags |= MsFlags::MS_RDONLY | MsFlags::MS_REMOUNT;
        }
        mount(Some(&m.from), &config.relative_rootfs_path(&m.to.to_str().unwrap()), None::<&str>, flags, None::<&str>).expect("configured mount failed");
    }

    chroot(&config.rootfs_path).expect("chroot failed");
    chdir(&config.cwd).expect("chdir failed");

    let output_config = match &config.output_config {
        Some(pipe_config) => Some((pipe_config, pipe().expect("log pipe creation failed"))),
        None => None,
    };

    let fork_result = unsafe { fork() }.expect("third fork failed");
    match fork_result {
        ForkResult::Child => {
            if let Some(output_config) = &output_config {
                dup2(output_config.1.1.as_raw_fd(), STDOUT_FILENO).expect("dup2 stdout failed");
                dup2(output_config.1.1.as_raw_fd(), STDERR_FILENO).expect("dup2 stderr failed");
            };

            unsafe {
                for v in env::vars() {
                    env::remove_var(v.0);
                }

                if config.uid.as_raw() == 0 {
                    env::set_var("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin");
                } else {
                    env::set_var("PATH", "/usr/local/bin:/usr/bin:/bin");
                }
                env::set_var("LD_LIBRARY_PATH", "/usr/local/lib64:/usr/local/lib:/usr/lib64:/usr/lib");
                env::set_var("HOME", &config.cwd);
                env::set_var("LANG", "C");
                env::set_var("LC_COLLATE", "C");
                env::set_var("TERM", "xterm-256color");

                for (name, value) in config.environment.iter() {
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
        ForkResult::Parent { child: init_pid } => match output_config {
            Some(output_config) => {
                close(output_config.1.1.as_raw_fd()).expect("close stdout_write_fd failed");

                let mut poll_fds = [PollFd::new(output_config.1.0.as_fd(), PollFlags::POLLIN)];
                let mut log_buffer = Vec::new();

                let mut start = true;
                let mut buffer = [0u8; 1024];
                loop {
                    match waitpid(init_pid, Some(WaitPidFlag::WNOHANG)).expect("waitpid failed") {
                        WaitStatus::StillAlive => {}
                        status => {
                            if let WaitStatus::Exited(_, code) = status {
                                if code != 0 && output_config.0.quiet {
                                    error!("Logs for runtime failure");
                                    io::stdout().write_all(log_buffer.as_slice()).unwrap();
                                    io::stdout().flush().unwrap();
                                }
                                exit(code);
                            }
                            panic!("runtime process failed: {:?}", status);
                        }
                    }

                    let n = poll(&mut poll_fds, 300_u16).expect("poll failed");
                    if n == 0 {
                        continue;
                    }

                    if poll_fds[0].revents().unwrap().contains(PollFlags::POLLIN) {
                        let count = read(output_config.1.0.as_raw_fd(), &mut buffer).expect("pipe read failed");
                        if count > 0 {
                            for b in &buffer[..count] {
                                if start {
                                    if output_config.0.quiet {
                                        log_buffer.write_all("\x1b[0m| ".as_bytes()).unwrap();
                                    } else {
                                        std::io::stdout().write_all("\x1b[0m| ".as_bytes()).unwrap();
                                    }
                                }
                                if output_config.0.quiet {
                                    log_buffer.write(&[*b]).unwrap();
                                } else {
                                    std::io::stdout().write(&[*b]).unwrap();
                                }
                                start = b.to_ascii_lowercase() as char == '\n';
                            }
                            if !output_config.0.quiet {
                                std::io::stdout().flush().unwrap();
                            }

                            if let Some(file) = &mut log_file {
                                file.write_all(&buffer[..count]).unwrap();
                                file.flush().unwrap();
                            }
                        }
                    }
                }
            }
            None => {
                let status = wait().expect("wait failed");
                match status {
                    WaitStatus::Exited(_, code) => exit(code),
                    status => panic!("runtime process failed: {:?}", status),
                }
            }
        },
    };
}
