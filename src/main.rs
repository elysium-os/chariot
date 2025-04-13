use std::{fs::File, path::Path, process::exit, rc::Rc};

use anyhow::{Context, Result, bail};
use colog::format::CologStyle;
use container::{Container, clean, runtime::RuntimeConfig};
use gumdrop::Options;
use log::{info, warn};
use nix::{
    fcntl::{Flock, FlockArg},
    libc,
    sys::signal::{
        self, SigHandler,
        Signal::{self, SIGKILL},
        kill,
    },
    unistd::{Gid, Pid, Uid, chdir},
};
use pipeline::{Pipeline, PipelineConfig};
use recipe::RecipeId;
use std::fs::{create_dir_all, exists};

mod config;
mod container;
mod pipeline;
mod recipe;
mod util;

#[derive(Options)]
struct ChariotOptions {
    #[options(no_short, help = "path to chariot config", default = "config.chariot")]
    config: String,

    #[options(no_short, help = "path to chariot cache", default = ".chariot-cache")]
    cache: String,

    #[options(no_short, help = "wipe chariot cache")]
    wipe_cache: bool,

    #[options(help = "additional logs")]
    verbose: bool,

    #[options(help = "less logs")]
    quiet: bool,

    #[options(help = "override default rootfs version", default = "2024.09.01")]
    rootfs_version: String,

    #[options(command, required)]
    command: Option<Command>,

    #[options(help = "print this help message")]
    help: bool,
}

#[derive(Options)]
enum Command {
    #[options(help = "build recipe(s)")]
    Build(BuildOptions),

    #[options(help = "execute command in container")]
    Exec(ExecOptions),
}

#[derive(Options)]
struct BuildOptions {
    #[options(no_short, help = "threads of parallelism", default = "8")]
    thread_count: u32,

    #[options(no_short, help = "target prefix", default = "/usr")]
    prefix: String,

    #[options(free, help = "recipes to process")]
    recipes: Vec<String>,
}

#[derive(Options)]
struct ExecOptions {
    #[options(help = "package(s) for exec")]
    package: Vec<String>,

    #[options(no_short, help = "user id for exec", default = "1000")]
    uid: u32,

    #[options(no_short, help = "group id for exec", default = "1000")]
    gid: u32,

    #[options(no_short, help = "make container writable")]
    rw: bool,

    #[options(free, help = "command(s) to execute")]
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

fn main() -> Result<()> {
    let opts = ChariotOptions::parse_args_default_or_exit();

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
    let lock = match Flock::lock(File::open(lockfile_path)?, FlockArg::LockExclusiveNonblock) {
        Err(err) => bail!("Failed to acquire lockfile: {}", err.1.to_string()),
        Ok(lock) => lock,
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
        Some(Command::Exec(exec_opts)) => exec(container, &opts, exec_opts)?,
        Some(Command::Build(build_opts)) => build(container, &opts, build_opts)?,
        None => bail!("Missing subcommand"),
    }

    // Release lockfile
    match lock.unlock() {
        Err(err) => bail!("Failed to unlock lockfile: {}", err.1.to_string()),
        Ok(_) => Ok(()),
    }
}

fn exec(container: Rc<Container>, _: &ChariotOptions, exec_opts: &ExecOptions) -> Result<()> {
    let cmd = exec_opts.command.join(" ");
    RuntimeConfig::default(&container.get_set(&exec_opts.package)?)
        .set_read_only(!exec_opts.rw)
        .set_uid(Uid::from(exec_opts.uid))
        .set_gid(Gid::from(exec_opts.gid))
        .no_redirect_std()
        .run_shell(cmd.as_str())
        .context(format!("Failed to execute command `{}`", cmd))?;
    Ok(())
}

fn build(container: Rc<Container>, opts: &ChariotOptions, build_opts: &BuildOptions) -> Result<()> {
    // Find config directory
    let config_dir = Path::new(&opts.config).canonicalize().context("Failed to canonicalize config path")?;
    match config_dir.parent() {
        None => bail!("Failed to resolve config directory"),
        Some(config_dir) => chdir(config_dir).context(format!("Failed to chdir into config directory `{}`", config_dir.to_str().unwrap(),))?,
    }

    let (recipes, dependencies) = config::parse(Path::new(&opts.config).to_path_buf()).context("Failed to parse chariot config")?;

    let mut chosen_recipes: Vec<RecipeId> = Vec::new();
    for recipe in &build_opts.recipes {
        match recipe.split_once("/") {
            Some((namespace, name)) => {
                let recipe = recipes.iter().find_map(|recipe| {
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

    let pipeline = Pipeline::new(
        Path::new(&opts.cache),
        container,
        recipes,
        dependencies,
        PipelineConfig {
            prefix: build_opts.prefix.clone(),
            thread_count: build_opts.thread_count,
            stdout_quiet: !opts.verbose,
            stderr_quiet: opts.quiet,
        },
    );
    for recipe_id in chosen_recipes {
        pipeline.invalidate_recipe(recipe_id).context("Failed to invalidate recipe")?;
    }

    pipeline.execute().context("Pipeline failed")?;

    Ok(())
}
