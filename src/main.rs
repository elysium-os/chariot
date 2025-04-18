use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use colog::format::CologStyle;
use container::{
    clean,
    runtime::{EnvVar, Mount, RuntimeConfig},
    Container,
};
use log::{error, info, warn};
use nix::{
    fcntl::{Flock, FlockArg},
    libc,
    sys::signal::{
        self, kill, SigHandler,
        Signal::{self, SIGKILL},
    },
    unistd::{chdir, Gid, Pid, Uid},
};
use pipeline::{Pipeline, PipelineOptions};
use recipe::RecipeId;
use std::fs::{create_dir_all, exists};
use std::{fs::File, path::Path, process::exit, rc::Rc};

mod config;
mod container;
mod pipeline;
mod recipe;
mod util;

#[derive(Parser)]
#[command(version, next_line_help = true)]
struct ChariotOptions {
    #[arg(long, help = "path to chariot config", default_value = "config.chariot")]
    config: String,

    #[arg(long, help = "path to chariot cache", default_value = ".chariot-cache")]
    cache: String,

    #[arg(long, help = "wipe chariot cache")]
    wipe_cache: bool,

    #[arg(long, help = "override default rootfs version", default_value = "2024.09.01")]
    rootfs_version: String,

    #[arg(long, help = "dont acquire lockfile, use with care")]
    no_lockfile: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "build recipe(s)")]
    Build(BuildOptions),

    #[command(about = "execute a command within the container")]
    Exec(ExecOptions),
}

#[derive(Args)]
struct BuildOptions {
    #[arg(long, short, help = "log recipe output in realtime")]
    verbose: bool,

    #[arg(long, short = 'c', help = "threads of parallelism", default_value = "8")]
    thread_count: u32,

    #[arg(long, help = "target prefix", default_value = "/usr")]
    prefix: String,

    #[arg(help = "recipes to process")]
    recipes: Vec<String>,
}

#[derive(Args)]
struct ExecOptions {
    #[arg(long, short, help = "package(s) for exec")]
    package: Vec<String>,

    #[arg(long, short, value_parser = keyvalue_opt_validate, help = "environment variable(s) for exec")]
    env: Vec<(String, String)>,

    #[arg(long, short, value_parser = mount_opt_validate, help = "mount(s) for exec")]
    mount: Vec<(String, String, bool)>,

    #[arg(long, help = "user id for exec", default_value = "1000")]
    uid: u32,

    #[arg(long, help = "group id for exec", default_value = "1000")]
    gid: u32,

    #[arg(long, help = "make container writable")]
    rw: bool,

    #[arg(help = "command(s) to execute")]
    command: Vec<String>,
}

const DEFAULT_PACKAGES: &'static [&'static str] = &[
    "which",
    "wget",
    "curl",
    "git",
    "python",
    "make",
    "patch",
    "bison",
    "diffutils",
    "docbook-xsl",
    "flex",
    "gettext",
    "inetutils",
    "libtool",
    "libxslt",
    "m4",
    "perl",
    "texinfo",
    "w3m",
    "xmlto",
];

fn keyvalue_opt_validate(s: &str) -> Result<(String, String), String> {
    match s.split_once("=") {
        None => Err(format!("`{s}` is not a key value pair")),
        Some((key, value)) => Ok((key.to_string(), value.to_string())),
    }
}

fn mount_opt_validate(s: &str) -> Result<(String, String, bool), String> {
    let (mounts, is_read_only) = match s.split_once(":") {
        None => (s, false),
        Some((mounts, attr)) => (mounts, attr == "ro"),
    };

    match mounts.split_once("=") {
        None => Err(format!("`{s}` is not a valid mount")),
        Some((from, to)) => Ok((from.to_string(), to.to_string(), is_read_only)),
    }
}

struct ChariotLogStyle;

impl CologStyle for ChariotLogStyle {
    fn prefix_token(&self, level: &log::Level) -> String {
        format!("::: {}", self.level_color(level, self.level_token(level)))
    }

    fn level_token(&self, level: &log::Level) -> &str {
        match *level {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARN",
            log::Level::Info => "INFO",
            log::Level::Debug => "DEBUG",
            log::Level::Trace => "TRACE",
        }
    }
}

extern "C" fn handle_sigint(_: libc::c_int) {
    info!("Terminated chariot process ({})", Pid::this());
    kill(Pid::from_raw(0), SIGKILL).expect("Failed to kill process group");
    exit(0)
}

fn main() {
    if let Err(err) = run_main() {
        error!("{}", err);
        error!("Caused by:");
        for (i, sub_error) in err.chain().skip(1).enumerate() {
            error!("  {}: {}", i, sub_error)
        }

        std::process::exit(1);
    }
}

fn run_main() -> Result<()> {
    let opts = ChariotOptions::parse();

    colog::default_builder().format(colog::formatter(ChariotLogStyle)).init();

    let handler = SigHandler::Handler(handle_sigint);
    unsafe { signal::signal(Signal::SIGINT, handler) }.unwrap();

    // Ensure cache
    if opts.wipe_cache {
        clean(&opts.cache).context("Failed to wipe chariot cache")?;
    }
    create_dir_all(&opts.cache).context("Failed to ensure chariot cache")?;

    // Ensure lockfile
    let lockfile_path = Path::new(&opts.cache).join("chariot.lock");
    if !exists(&lockfile_path)? {
        File::create(&lockfile_path).context("Failed to create lockfile")?;
    }

    // Acquire lockfile
    let mut lock = None;
    if !opts.no_lockfile {
        lock = match Flock::lock(File::open(lockfile_path)?, FlockArg::LockExclusiveNonblock) {
            Err(err) => bail!("Failed to acquire lockfile: {}", err.1.to_string()),
            Ok(lock) => Some(lock),
        };
    };

    // Setup container
    let container = Container::init(
        Path::new(&opts.cache).join("container"),
        opts.rootfs_version.clone(),
        DEFAULT_PACKAGES.into_iter().map(|s| s.to_string()).collect(),
    )
    .context("Failed to initialize container")?;

    // Run
    match &opts.command {
        Command::Exec(exec_opts) => exec(container, &opts, exec_opts)?,
        Command::Build(build_opts) => build(container, &opts, build_opts)?,
    }

    // Release lockfile
    if let Some(lock) = lock {
        if let Err(err) = lock.unlock() {
            bail!("Failed to unlock lockfile: {}", err.1.to_string())
        }
    }

    Ok(())
}

fn exec(container: Rc<Container>, _: &ChariotOptions, exec_opts: &ExecOptions) -> Result<()> {
    let cmd = exec_opts.command.join(" ");
    let mut runtime_config = RuntimeConfig::default(&container.get_set(&exec_opts.package)?)
        .set_read_only(!exec_opts.rw)
        .set_uid(Uid::from(exec_opts.uid))
        .set_gid(Gid::from(exec_opts.gid));

    for e in exec_opts.env.iter() {
        runtime_config.env.push(EnvVar::new(e.0.to_string(), e.1.to_string()));
    }

    for mount in exec_opts.mount.iter() {
        runtime_config.mounts.push(Mount {
            from: mount.0.to_string(),
            dest: mount.1.to_string(),
            read_only: mount.2,
            is_file: false,
        });
    }

    runtime_config
        .run_shell(cmd.as_str())
        .with_context(|| format!("Failed to execute command `{}`", cmd))?;

    Ok(())
}

fn build(container: Rc<Container>, opts: &ChariotOptions, build_opts: &BuildOptions) -> Result<()> {
    let config_dir = Path::new(&opts.config).canonicalize().context("Failed to canonicalize config path")?;
    match config_dir.parent() {
        None => bail!("Failed to resolve config directory"),
        Some(config_dir) => chdir(config_dir).with_context(|| format!("Failed to chdir into config directory `{}`", config_dir.to_str().unwrap()))?,
    }

    let config = config::parse(Path::new(&opts.config).to_path_buf()).context("Failed to parse chariot config")?;

    // Resolve recipe IDs
    let mut chosen_recipes: Vec<RecipeId> = Vec::new();
    for recipe in &build_opts.recipes {
        match recipe.split_once("/") {
            Some((namespace, name)) => {
                let recipe = config.recipes.iter().find_map(|recipe| {
                    if match recipe.1.kind {
                        recipe::Kind::Source(_) => "source",
                        recipe::Kind::Bare(_) => "bare",
                        recipe::Kind::Tool(_) => "tool",
                        recipe::Kind::Package(_) => "package",
                    } != namespace
                    {
                        return None;
                    }

                    if recipe.1.name != name {
                        return None;
                    }

                    return Some(recipe);
                });

                match recipe {
                    Some(recipe) => chosen_recipes.push(recipe.1.id),
                    None => warn!("Unknown recipe `{}/{}` ignoring...", namespace, name),
                }
            }
            None => warn!("Invalid recipe `{}` ignoring...", recipe),
        }
    }

    // Build pipeline
    let pipeline = Pipeline::new(
        Path::new(&opts.cache),
        container,
        PipelineOptions {
            prefix: build_opts.prefix.clone(),
            thread_count: build_opts.thread_count,
            quiet: !build_opts.verbose,
        },
        config,
    );
    for recipe_id in chosen_recipes {
        pipeline.invalidate_recipe(recipe_id).context("Failed to invalidate recipe")?;
    }

    // Execute
    pipeline.execute().context("Pipeline failed")?;

    Ok(())
}
