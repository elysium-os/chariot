use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{exists, read_dir},
    num::NonZero,
    path::Path,
    process::exit,
    rc::Rc,
    thread::available_parallelism,
};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use colog::format::CologStyle;
use log::{error, info, warn};
use nix::{
    sys::signal::{
        kill, signal, SigHandler,
        Signal::{self, SIGKILL},
    },
    unistd::{chdir, Gid, Pid, Uid},
};
use which::which;

use cache::Cache;
use config::{Config, ConfigNamespace, ConfigRecipeId};
use pipeline::Pipeline;
use rootfs::RootFS;
use runtime::{Mount, RuntimeConfig};
use util::clean;

mod cache;
mod config;
mod pipeline;
mod recipe;
mod rootfs;
mod runtime;
mod util;

#[derive(Parser)]
#[command(version, next_line_help = true)]
struct ChariotOptions {
    #[arg(long, help = "path to chariot config", default_value = "config.chariot")]
    config: String,

    #[arg(long, help = "path to chariot cache", default_value = ".chariot-cache")]
    cache: String,

    #[arg(long, help = "override default rootfs version", default_value = "20250401T023134Z")]
    rootfs_version: String,

    #[arg(long, help = "dont acquire lockfile, use with care")]
    no_lockfile: bool,

    #[arg(long, short, help = "log verbose output in realtime")]
    verbose: bool,

    #[arg(long = "option", short = 'o', value_parser = keyvalue_opt_validate, help = "user defined options")]
    option: Vec<(String, String)>,

    #[command(subcommand)]
    command: MainCommand,
}

#[derive(Subcommand)]
enum MainCommand {
    #[command(about = "build recipe(s)")]
    Build(BuildOptions),

    #[command(about = "execute a command within the container")]
    Exec(ExecOptions),

    #[command(about = "cleanup recipes no longer in config")]
    Cleanup,

    #[command(about = "wipe (delete) various parts of the chariot cache")]
    Wipe {
        #[command(subcommand)]
        kind: WipeKind,
    },

    #[command(about = "return a path into cache install")]
    Path {
        #[arg(help = "recipe to return path for")]
        recipe: String,
    },
}

#[derive(Args)]
struct BuildOptions {
    #[arg(long, short = 'j', help = "threads of parallelism", default_value_t = available_parallelism().unwrap())]
    parallelism: NonZero<usize>,

    #[arg(long, help = "package/sysroot prefix", default_value = "/usr")]
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

    #[arg(long, help = "set current working directory")]
    cwd: Option<String>,

    #[arg(help = "command(s) to execute")]
    command: Vec<String>,
}

#[derive(Subcommand)]
enum WipeKind {
    #[command(about = "wipe the entire chariot cache")]
    Cache,

    #[command(about = "wipe the rootfs")]
    Rootfs,

    #[command(about = "wipe the proc cache")]
    ProcCache,

    #[command(about = "wipe recipe(s)")]
    Recipe {
        #[arg(long, help = "wipe all recipes")]
        all: bool,

        #[arg(help = "recipe(s) to wipe")]
        recipes: Vec<String>,
    },
}

pub struct ChariotContext {
    pub cache: Rc<Cache>,
    pub rootfs: Rc<RootFS>,
    pub config: Rc<Config>,
    pub effective_options: BTreeMap<String, String>,
    pub verbose: bool,
}

pub struct ChariotBuildContext {
    pub common: ChariotContext,
    pub prefix: String,
    pub parallelism: NonZero<usize>,
    pub recipes: Vec<String>,
}

struct ChariotLogStyle;

impl CologStyle for ChariotLogStyle {
    fn prefix_token(&self, level: &log::Level) -> String {
        format!("{} |", self.level_color(level, self.level_token(level)))
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

extern "C" fn handle_sigint(_: nix::libc::c_int) {
    info!("Terminated chariot process ({})", Pid::this());
    kill(Pid::from_raw(0), SIGKILL).expect("Failed to kill process group");
    exit(0)
}

fn main() {
    unsafe { signal(Signal::SIGINT, SigHandler::Handler(handle_sigint)) }.unwrap();

    colog::default_builder().format(colog::formatter(ChariotLogStyle)).init();

    if let Err(err) = run_main() {
        error!("{}", err);
        if err.chain().len() > 1 {
            error!("Caused by:");
            for (i, sub_error) in err.chain().skip(1).enumerate() {
                error!("  {}: {}", i, sub_error)
            }
        }

        exit(1);
    }
}

fn run_main() -> Result<()> {
    let opts = ChariotOptions::parse();

    // Ensure program dependencies
    which("wget").context("Chariot requires wget")?;
    which("bsdtar").context("Chariot requires bsdtar")?;

    // Parse config
    let config_dir = Path::new(&opts.config).canonicalize().context("Failed to canonicalize config path")?;
    match config_dir.parent() {
        None => bail!("Failed to resolve config directory"),
        Some(config_dir) => chdir(config_dir).with_context(|| format!("Failed to chdir into config directory `{}`", config_dir.to_str().unwrap()))?,
    }

    let config = match config_dir.file_name() {
        None => bail!("Failed to resolve config filename"),
        Some(name) => Config::parse(Path::new(name)).context("Failed to parse chariot config")?,
    };

    // Parse options
    let mut effective_options: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in opts.option {
        if !config.options.contains_key(&key) {
            bail!("User option `{}` is not defined in the config", key);
        }

        let allowed_values = &config.options[&key];
        if !allowed_values.contains(&value) {
            bail!("User option `{}` does not allow the value `{}`. List of allowed values: {:?}", key, value, allowed_values)
        }

        effective_options.insert(key, value);
    }

    // Initialize cache
    let cache = Cache::init(opts.cache, !opts.no_lockfile).context("Failed to initialize chariot cache")?;

    // Initialize RootFS
    let mut global_packages = config.global_pkgs.clone();
    global_packages.append(&mut Vec::from_iter(rootfs::DEFAULT_PACKAGES.iter().map(|pkg| pkg.to_string())));

    let rootfs = cache
        .clone()
        .rootfs_init(String::from(opts.rootfs_version), BTreeSet::from_iter(global_packages), opts.verbose)
        .context("Failed to initialize rootfs")?;

    // Setup context
    let context = ChariotContext {
        cache,
        config,
        rootfs,
        verbose: opts.verbose,
        effective_options,
    };

    // Subcommands
    match opts.command {
        MainCommand::Exec(exec_opts) => exec(context, exec_opts),
        MainCommand::Build(build_opts) => build(ChariotBuildContext {
            common: context,
            prefix: build_opts.prefix,
            parallelism: build_opts.parallelism,
            recipes: build_opts.recipes,
        }),
        MainCommand::Cleanup => cleanup(context),
        MainCommand::Wipe { kind } => wipe(context, kind),
        MainCommand::Path { recipe } => path(context, recipe),
    }
}

fn resolve_recipe(config: &Config, recipe_selector: &String) -> Option<ConfigRecipeId> {
    match recipe_selector.split_once("/") {
        Some((namespace, name)) => {
            let recipe = config.recipes.iter().find_map(|recipe| {
                if match recipe.1.namespace {
                    ConfigNamespace::Source(_) => "source",
                    ConfigNamespace::Custom(_) => "custom",
                    ConfigNamespace::Tool(_) => "tool",
                    ConfigNamespace::Package(_) => "package",
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
                Some(recipe) => return Some(recipe.1.id),
                None => warn!("Unknown recipe `{}/{}` ignoring...", namespace, name),
            }
        }
        None => warn!("Invalid recipe `{}` ignoring...", recipe_selector),
    }
    None
}

fn exec(context: ChariotContext, exec_opts: ExecOptions) -> Result<()> {
    let cmd = exec_opts.command.join(" ");
    let mut runtime_config = RuntimeConfig::new(context.rootfs.subset(BTreeSet::from_iter(exec_opts.package))?)
        .set_read_only(!exec_opts.rw)
        .set_uid(Uid::from(exec_opts.uid))
        .set_gid(Gid::from(exec_opts.gid));

    if let Some(cwd) = &exec_opts.cwd {
        runtime_config.cwd = Path::new(cwd).to_path_buf();
    }

    for env in exec_opts.env.iter() {
        runtime_config.environment.insert(env.0.clone(), env.1.clone());
    }

    for mount in exec_opts.mount.iter() {
        runtime_config.mounts.push(Mount {
            from: Path::new(&mount.0).to_path_buf(),
            to: Path::new(&mount.1).to_path_buf(),
            read_only: mount.2,
            is_file: false,
        });
    }

    runtime_config.run_shell(cmd.as_str()).with_context(|| format!("Failed to execute command `{}`", cmd))
}

fn build(context: ChariotBuildContext) -> Result<()> {
    // Resolve recipe IDs
    let mut chosen_recipes: Vec<ConfigRecipeId> = Vec::new();
    for recipe in &context.recipes {
        if let Some(recipe_id) = resolve_recipe(&context.common.config, recipe) {
            chosen_recipes.push(recipe_id);
        }
    }

    // Build pipeline
    let pipeline = Pipeline::new(context);
    for recipe_id in chosen_recipes {
        pipeline.invalidate_recipe(recipe_id).context("Failed to invalidate recipe")?;
    }

    // Execute
    pipeline.execute().context("Pipeline failed")
}

fn cleanup(context: ChariotContext) -> Result<()> {
    for namespace in ["source", "package", "tool", "custom"] {
        let path = context.cache.path_recipes().join(namespace);
        if !exists(&path)? {
            continue;
        }

        for recipe_dir in read_dir(&path)? {
            let name = recipe_dir.as_ref().unwrap().file_name();

            let mut found = false;
            for (_, recipe) in &context.config.recipes {
                if recipe.namespace.to_string() != namespace {
                    continue;
                }

                if recipe.name != name.to_str().unwrap() {
                    continue;
                }

                found = true;
                break;
            }

            if !found {
                warn!(
                    "Cleaning up cached recipe `{}/{}` because it was not found in the config",
                    namespace,
                    name.to_str().unwrap()
                );

                clean(recipe_dir.unwrap().path()).context("Failed to cleanup recipe")?;
            }
        }
    }
    Ok(())
}

fn wipe(context: ChariotContext, kind: WipeKind) -> Result<()> {
    match kind {
        WipeKind::Cache => clean(context.cache.path()).context("Failed to wipe cache")?,
        WipeKind::Rootfs => context.cache.rootfs_wipe().context("Failed to wipe rootfs")?,
        WipeKind::ProcCache => clean(context.cache.path_proc_caches()).context("Failed to wipe proc cache")?,
        WipeKind::Recipe { recipes, all } => {
            if all {
                clean(context.cache.path_recipes()).context("Failed to wipe all recipes")?;
                return Ok(());
            }
            for recipe_selector in recipes {
                let recipe_id = resolve_recipe(&context.config, &recipe_selector);
                match recipe_id {
                    Some(recipe_id) => context
                        .recipe_wipe(recipe_id)
                        .with_context(|| format!("Failed to wipe recipe `{}`", context.config.recipes[&recipe_id]))?,
                    None => continue,
                }
            }
        }
    }

    Ok(())
}

fn path(context: ChariotContext, recipe: String) -> Result<()> {
    match resolve_recipe(&context.config, &recipe) {
        Some(recipe_id) => {
            let recipe_path = match context.config.recipes[&recipe_id].namespace {
                ConfigNamespace::Source(_) => context.path_recipe(recipe_id).join("src"),
                ConfigNamespace::Package(_) | ConfigNamespace::Tool(_) | ConfigNamespace::Custom(_) => context.path_recipe(recipe_id).join("install"),
            };
            print!("{}", recipe_path.canonicalize().context("Failed to canonicalize recipe path")?.to_str().unwrap());
            Ok(())
        }
        None => bail!("Unknown recipe `{}`", recipe),
    }
}
