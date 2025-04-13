use anyhow::{Context, Result, bail};
use log::error;
use nix::{
    libc::{STDERR_FILENO, STDOUT_FILENO},
    poll::{PollFd, PollFlags, poll},
    sys::wait::{WaitPidFlag, WaitStatus, waitpid},
    unistd::{Gid, Uid, close, dup2, pipe, read},
};
use std::{
    ffi::CString,
    fs::{File, exists, metadata},
    io::Write,
    os::fd::{AsFd, AsRawFd},
    path::{Path, PathBuf},
    process::exit,
};

use super::ContainerSet;

pub struct RuntimeConfig {
    rootfs_path: PathBuf,
    pub read_only: bool,
    pub network_isolation: bool,
    pub uid: Uid,
    pub gid: Gid,
    pub cwd: String,
    pub mounts: Vec<Mount>,
    pub env: Vec<EnvVar>,
    pub quiet_stdout: bool,
    pub quiet_stderr: bool,
    pub stdout_log_path: Option<PathBuf>,
    pub stderr_log_path: Option<PathBuf>,
}

pub struct Mount {
    from: String,
    dest: String,
    read_only: bool,
    is_file: bool,
}

pub struct EnvVar {
    name: String,
    value: String,
}

impl EnvVar {
    pub fn new(name: impl AsRef<str>, value: impl AsRef<str>) -> EnvVar {
        EnvVar {
            name: name.as_ref().to_string(),
            value: value.as_ref().to_string(),
        }
    }
}

impl Mount {
    pub fn new(from: impl AsRef<str>, to: impl AsRef<str>) -> Mount {
        Mount {
            from: from.as_ref().to_string(),
            dest: to.as_ref().to_string(),
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
    pub fn default_rootfs(rootfs_path: PathBuf) -> RuntimeConfig {
        RuntimeConfig {
            rootfs_path,
            read_only: true,
            network_isolation: false,
            uid: Uid::from(1000),
            gid: Gid::from(1000),
            cwd: String::from("/root"),
            mounts: Vec::new(),
            env: Vec::new(),
            quiet_stdout: true,
            quiet_stderr: false,
            stdout_log_path: None,
            stderr_log_path: None,
        }
    }

    pub fn default(container_set: &ContainerSet) -> RuntimeConfig {
        RuntimeConfig::default_rootfs(container_set.rootfs_path())
    }

    pub fn set_read_only(mut self, read_only: bool) -> RuntimeConfig {
        self.read_only = read_only;
        self
    }

    pub fn set_quiet(mut self, stdout: bool, stderr: bool) -> RuntimeConfig {
        self.quiet_stdout = stdout;
        self.quiet_stderr = stderr;
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

    pub fn set_cwd(mut self, cwd: impl AsRef<str>) -> RuntimeConfig {
        self.cwd = cwd.as_ref().to_string();
        self
    }

    pub fn add_mount(mut self, mount: Mount) -> RuntimeConfig {
        self.mounts.push(mount);
        self
    }

    pub fn as_root(self) -> RuntimeConfig {
        self.set_uid(Uid::from(0)).set_gid(Gid::from(0))
    }

    pub fn rw(self) -> RuntimeConfig {
        self.set_read_only(false)
    }

    pub fn set_log_file(&mut self, stdout: Option<impl AsRef<Path>>, stderr: Option<impl AsRef<Path>>) {
        self.stdout_log_path = match stdout {
            None => None,
            Some(path) => Some(path.as_ref().to_path_buf()),
        };
        self.stderr_log_path = match stderr {
            None => None,
            Some(path) => Some(path.as_ref().to_path_buf()),
        };
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
        let mut stdout_logfile = None;
        if let Some(path) = &self.stdout_log_path {
            let file = File::create(path);
            stdout_logfile = match file {
                Err(e) => {
                    error!("Failed to create stdout log file: {}", e);
                    None
                }
                Ok(f) => Some(f),
            }
        }

        let mut stderr_logfile = None;
        if let Some(path) = &self.stderr_log_path {
            let file = File::create(path);
            stderr_logfile = match file {
                Err(e) => {
                    error!("Failed to create stderr log file: {}", e);
                    None
                }
                Ok(f) => Some(f),
            }
        }

        let fork_result = unsafe { nix::unistd::fork() }.context("Failed to fork")?;
        match fork_result {
            nix::unistd::ForkResult::Child => stage1(self, args, stdout_logfile, stderr_logfile),
            nix::unistd::ForkResult::Parent { child: init_pid } => {
                let i = nix::sys::wait::waitpid(init_pid, None).context("Failed to waitpid")?;
                match i {
                    nix::sys::wait::WaitStatus::Exited(_, code) => {
                        if code == 0 {
                            return Ok(());
                        }
                        bail!("runtime exited with non-zero error code `{}`", code);
                    }
                    _ => bail!("runtime process failed"),
                }
            }
        }
    }

    pub fn run_shell(&self, command: impl AsRef<str>) -> Result<()> {
        self.run(vec![String::from("bash"), String::from("-c"), String::from(command.as_ref())])
            .context(format!("run_shell failed `{}`", command.as_ref()))
    }

    pub fn run_python(&self, command: impl AsRef<str>) -> Result<()> {
        self.run(vec![String::from("python3"), String::from("-c"), String::from(command.as_ref())])
            .context(format!("run_python failed `{}`", command.as_ref()))
    }
}

fn stage1(config: &RuntimeConfig, args: Vec<String>, stdout_logfile: Option<File>, stderr_logfile: Option<File>) -> ! {
    let euid = nix::unistd::geteuid();
    let egid = nix::unistd::getegid();

    nix::sched::unshare(nix::sched::CloneFlags::CLONE_NEWUSER | nix::sched::CloneFlags::CLONE_NEWPID).expect("USER/PID unshare failed");

    std::fs::write("/proc/self/setgroups", "deny").expect("setgroups write failed");
    std::fs::write("/proc/self/uid_map", format!("{} {} 1", config.uid, euid)).expect("uid_map write failed");
    std::fs::write("/proc/self/gid_map", format!("{} {} 1", config.gid, egid)).expect("gid_map write failed");

    nix::unistd::setuid(config.uid).expect("setuid failed");
    nix::unistd::setgid(config.gid).expect("setgid failed");

    let fork_result = unsafe { nix::unistd::fork() }.expect("second fork failed");
    match fork_result {
        nix::unistd::ForkResult::Child => stage2(config, args, stdout_logfile, stderr_logfile),
        nix::unistd::ForkResult::Parent { child: child_pid } => {
            let status = nix::sys::wait::waitpid(child_pid, None).expect("second waitpid failed");
            if let nix::sys::wait::WaitStatus::Exited(_, code) = status {
                exit(code);
            }
            panic!("runtime child process failed");
        }
    }
}

fn stage2(config: &RuntimeConfig, args: Vec<String>, mut stdout_logfile: Option<File>, mut stderr_logfile: Option<File>) -> ! {
    let mut clone_flags = nix::sched::CloneFlags::CLONE_NEWNS;
    if config.network_isolation {
        clone_flags |= nix::sched::CloneFlags::CLONE_NEWNS;
    }
    nix::sched::unshare(clone_flags).expect("unshare failed");

    nix::mount::mount(
        Some(&config.rootfs_path),
        &config.rootfs_path,
        None::<&str>,
        nix::mount::MsFlags::MS_BIND,
        None::<&str>,
    )
    .expect("rootfs mount failed");

    let devices = vec!["tty", "random", "urandom", "null", "zero", "full"];
    for dev in &devices {
        let dev_path = config.relative_rootfs_path("/dev").join(dev);
        std::fs::File::create(&dev_path).expect(format!("{:?} creation failed", dev_path).as_str());
    }

    let pts_path = &config.relative_rootfs_path("/dev/pts");
    std::fs::create_dir_all(pts_path).expect("/dev/pts creation failed");

    let shm_path = &config.relative_rootfs_path("/dev/shm");
    std::fs::create_dir_all(shm_path).expect("/dev/shm creation failed");

    for mount in config.mounts.iter() {
        let path = config.relative_rootfs_path(&mount.dest);
        if exists(&path).expect("mount path exists failed") {
            let meta = metadata(&path).expect("mount path metadata failed");
            if mount.is_file {
                if !meta.is_file() {
                    std::fs::remove_dir(&path).expect("mount path remove_dir failed");
                }
            } else {
                if !meta.is_dir() {
                    std::fs::remove_file(&path).expect("mount path remove_file failed");
                }
            }
        }

        if mount.is_file {
            std::fs::File::create(&path).expect("mount path file creation failed");
        } else {
            std::fs::create_dir_all(&path).expect("mount path dir creation failed");
        }
    }

    let mut remount_flags =
        nix::mount::MsFlags::MS_BIND | nix::mount::MsFlags::MS_REMOUNT | nix::mount::MsFlags::MS_NODEV | nix::mount::MsFlags::MS_NOSUID;
    if config.read_only {
        remount_flags |= nix::mount::MsFlags::MS_RDONLY;
    }
    nix::mount::mount(Some(&config.rootfs_path), &config.rootfs_path, None::<&str>, remount_flags, None::<&str>)
        .expect("rootfs readonly remount failed");

    for dev in devices {
        nix::mount::mount(
            Some(&Path::new("/dev").join(dev)),
            config.relative_rootfs_path("/dev").join(dev).to_str().unwrap(),
            None::<&str>,
            nix::mount::MsFlags::MS_BIND,
            None::<&str>,
        )
        .expect("device mount failed")
    }

    if !config.network_isolation {
        nix::mount::mount(
            Some(&std::fs::canonicalize("/etc/resolv.conf").unwrap()),
            &config.relative_rootfs_path("/etc/resolv.conf"),
            None::<&str>,
            nix::mount::MsFlags::MS_BIND,
            None::<&str>,
        )
        .expect("resolv.conf mount failed");
    }

    nix::mount::mount(None::<&str>, pts_path, Some("devpts"), nix::mount::MsFlags::empty(), None::<&str>).expect("/dev/pts mount failed");
    nix::mount::mount(None::<&str>, shm_path, Some("tmpfs"), nix::mount::MsFlags::empty(), None::<&str>).expect("/dev/shm mount failed");
    nix::mount::mount(
        None::<&str>,
        &config.relative_rootfs_path("/run"),
        Some("tmpfs"),
        nix::mount::MsFlags::empty(),
        None::<&str>,
    )
    .expect("/run mount failed");
    nix::mount::mount(
        None::<&str>,
        &config.relative_rootfs_path("/tmp"),
        Some("tmpfs"),
        nix::mount::MsFlags::empty(),
        None::<&str>,
    )
    .expect("/tmp mount failed");
    nix::mount::mount(
        None::<&str>,
        &config.relative_rootfs_path("/proc"),
        Some("proc"),
        nix::mount::MsFlags::empty(),
        None::<&str>,
    )
    .expect("/proc mount failed");

    for mount in config.mounts.iter() {
        let mut flags = nix::mount::MsFlags::MS_BIND;
        if !mount.is_file {
            flags |= nix::mount::MsFlags::MS_REC;
        }
        if mount.read_only {
            flags |= nix::mount::MsFlags::MS_RDONLY;
        }
        nix::mount::mount(
            Some(mount.from.as_str()),
            &config.relative_rootfs_path(&mount.dest),
            None::<&str>,
            flags,
            None::<&str>,
        )
        .expect("configured mount failed");
    }

    nix::unistd::chroot(&config.rootfs_path).expect("chroot failed");
    nix::unistd::chdir(config.cwd.as_str()).expect("chdir failed");

    let (stdout_read_fd, stdout_write_fd) = pipe().expect("pipe for stdout failed");
    let (stderr_read_fd, stderr_write_fd) = pipe().expect("pipe for stderr failed");

    let fork_result = unsafe { nix::unistd::fork() }.expect("third fork failed");
    match fork_result {
        nix::unistd::ForkResult::Child => {
            dup2(stdout_write_fd.as_raw_fd(), STDOUT_FILENO).expect("dup2 stdout_write_fd failed");
            close(stdout_read_fd.as_raw_fd()).expect("close stdout_read_fd failed");
            close(stdout_write_fd.as_raw_fd()).expect("close stdout_write_fd failed");

            dup2(stderr_write_fd.as_raw_fd(), STDERR_FILENO).expect("dup2 stderr_write_fd failed");
            close(stderr_read_fd.as_raw_fd()).expect("close stderr_read_fd failed");
            close(stderr_write_fd.as_raw_fd()).expect("close stderr_write_fd failed");

            unsafe {
                for v in std::env::vars() {
                    std::env::remove_var(v.0);
                }

                if config.uid.as_raw() == 0 {
                    std::env::set_var("PATH", "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin");
                } else {
                    std::env::set_var("PATH", "/usr/local/bin:/usr/bin:/bin");
                }

                std::env::set_var("HOME", &config.cwd);
                std::env::set_var("LANG", "C");
                std::env::set_var("TERM", "xterm-256color");

                for var in config.env.iter() {
                    std::env::set_var(&var.name, &var.value);
                }
            }

            let exec_result = nix::unistd::execvp(
                &CString::new(args[0].as_str()).unwrap(),
                &args.iter().map(|a| CString::new(a.as_str()).unwrap()).collect::<Vec<_>>(),
            );
            eprintln!("error when executing program: {}", exec_result.unwrap_err());
            exit(1);
        }
        nix::unistd::ForkResult::Parent { child: init_pid } => {
            close(stdout_write_fd.as_raw_fd()).expect("close stdout_write_fd failed");
            close(stderr_write_fd.as_raw_fd()).expect("close stderr_write_fd failed");

            let mut buffer = [0u8; 1024];
            let mut poll_fds = [
                PollFd::new(stdout_read_fd.as_fd(), PollFlags::POLLIN),
                PollFd::new(stderr_read_fd.as_fd(), PollFlags::POLLIN),
            ];

            loop {
                match waitpid(init_pid, Some(WaitPidFlag::WNOHANG)).expect("waitpid failed") {
                    WaitStatus::StillAlive => {}
                    status => {
                        if let WaitStatus::Exited(_, code) = status {
                            if code == 0 {
                                exit(code);
                            }
                            panic!("process returned non-zero exit code `{}`", code);
                        }
                        panic!("runtime process failed: {:?}", status);
                    }
                }

                let n = poll(&mut poll_fds, 100_u8).expect("poll failed");
                if n == 0 {
                    continue;
                }

                if poll_fds[0].revents().unwrap().contains(PollFlags::POLLIN) {
                    let count = read(stdout_read_fd.as_raw_fd(), &mut buffer).expect("stdout pipe read failed");
                    if count != 0 {
                        if !config.quiet_stdout {
                            std::io::stdout().write_all(&buffer[..count]).unwrap();
                            std::io::stdout().flush().unwrap();
                        }
                        if let Some(file) = &mut stdout_logfile {
                            file.write_all(&buffer[..count]).unwrap();
                            file.flush().unwrap();
                        }
                    }
                }

                if poll_fds[1].revents().unwrap().contains(PollFlags::POLLIN) {
                    let count = read(stderr_read_fd.as_raw_fd(), &mut buffer).expect("stderr pipe read failed");
                    if count != 0 {
                        if !config.quiet_stderr {
                            std::io::stderr().write_all(&buffer[..count]).unwrap();
                            std::io::stderr().flush().unwrap();
                        }
                        if let Some(file) = &mut stderr_logfile {
                            file.write_all(&buffer[..count]).unwrap();
                            file.flush().unwrap();
                        }
                    }
                }
            }
        }
    };
}
