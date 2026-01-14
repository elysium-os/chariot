use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    env::vars,
    fs::{exists, read_dir, read_to_string, remove_dir},
    io,
    num::NonZero,
    path::Path,
    process::exit,
    rc::Rc,
    thread::available_parallelism,
};

use anyhow::{bail, Context, Result};
use bytesize::ByteSize;
use chrono::DateTime;
use clap::{value_parser, Args, CommandFactory, Parser, Subcommand};
use clap_complete::aot::{generate, Shell};
use log::{error, info, warn, Level, LevelFilter, Log};
use nix::{
    sys::signal::{
        kill, signal, SigHandler,
        Signal::{self, SIGKILL},
    },
    unistd::{chdir, Gid, Pid, Uid},
};
use owo_colors::{OwoColorize, Style};
use which::which;

use cache::Cache;
use config::{Config, ConfigNamespace, ConfigRecipeId};
use pipeline::Pipeline;
use rootfs::RootFS;
use runtime::{Mount, RuntimeConfig};
use util::clean;

use crate::{recipe::RecipeState, util::clean_within};

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

    #[arg(long, short, help = "log verbose output in realtime", global = true)]
    verbose: bool,

    #[arg(long = "option", short = 'o', value_parser = keyvalue_opt_validate, help = "user defined options", global = true)]
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

    #[command(about = "purge recipes no longer in config")]
    Purge,

    #[command(about = "list recipes in cache")]
    List,

    #[command(about = "wipe (delete) various parts of the chariot cache")]
    Wipe {
        #[command(subcommand)]
        kind: WipeKind,
    },

    #[command(about = "return a path to recipe output")]
    Path {
        #[arg(help = "recipe to return path for")]
        recipe: String,
    },

    #[command(about = "print logs")]
    Logs {
        #[arg(help = "recipe whos logs to print")]
        recipe: String,

        #[arg(help = "logs to print (eg. configure, build)", default_value_t = String::from("build"))]
        kind: String,
    },

    #[command(about = "generate shell completions for chariot")]
    Completions {
        #[arg(help = "shell to generate completions for", value_parser = value_parser!(Shell))]
        shell: Shell,
    },
}

#[derive(Args)]
struct BuildOptions {
    #[arg(long, short = 'j', help = "threads of parallelism", default_value_t = available_parallelism().unwrap())]
    parallelism: NonZero<usize>,

    #[arg(long, short = 'w', help = "perform a clean build (wipe build dir)")]
    clean: bool,

    #[arg(long, help = "package/sysroot prefix", default_value = "/usr")]
    prefix: String,

    #[arg(help = "recipes to process")]
    recipes: Vec<String>,
}

#[derive(Args)]
struct ExecOptions {
    #[arg(long, help = "use a recipe context")]
    recipe_context: Option<String>,

    #[arg(long, short, help = "package(s) for exec")]
    package: Vec<String>,

    #[arg(long, short, help = "dependencies for exec")]
    dependency: Vec<String>,

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
    pub clean_build: bool,
}

struct ChariotLogger;

impl Log for ChariotLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= Level::Error
    }

    fn log(&self, record: &log::Record) {
        let level_style = match record.level() {
            Level::Trace => Style::new().black(),
            Level::Debug => Style::new().blue(),
            Level::Info => Style::new().green(),
            Level::Warn => Style::new().yellow(),
            Level::Error => Style::new().red(),
        }
        .bold();

        println!("{} | {}", record.level().style(level_style), record.args());
    }

    fn flush(&self) {}
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

static LOGGER: ChariotLogger = ChariotLogger;

fn main() {
    unsafe { signal(Signal::SIGINT, SigHandler::Handler(handle_sigint)) }.unwrap();

    log::set_logger(&LOGGER).map(|_| log::set_max_level(LevelFilter::Info)).expect("Failed to initialize logger");

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

    if let MainCommand::Completions { shell } = opts.command {
        generate(shell, &mut ChariotOptions::command(), "chariot".to_string(), &mut io::stdout());
        return Ok(());
    }

    // Ensure program dependencies
    which("wget").context("Chariot requires wget")?;
    which("bsdtar").context("Chariot requires bsdtar")?;

    // Determine config directory
    let config_file = Path::new(&opts.config).canonicalize().context("Failed to canonicalize config path")?;
    let config_dir = match config_file.parent() {
        None => bail!("Failed to resolve config directory"),
        Some(config_dir) => config_dir,
    };

    // Change directory to config directory
    chdir(config_dir).with_context(|| format!("Failed to chdir into config directory `{}`", config_dir.to_str().unwrap()))?;

    // Parse development overrides
    let mut overrides = HashMap::new();
    let overrides_path = config_dir.join(".chariot-overrides");
    if overrides_path.exists() {
        let overrides_data: String = read_to_string(&overrides_path).with_context(|| format!("Failed to read overrides from `{}`", overrides_path.to_str().unwrap()))?;
        for line in overrides_data.lines() {
            let parts: Vec<&str> = line.split(":").collect();
            if parts.len() != 2 {
                bail!("Invalid dev override `{}`", line);
            }
            overrides.insert(String::from(parts[0]), String::from(parts[1]));
        }
    }

    // Parse config
    let config = match config_file.file_name() {
        None => bail!("Failed to resolve config filename"),
        Some(name) => Config::parse(Path::new(name), overrides).context("Failed to parse chariot config")?,
    };

    // Parse options
    let mut raw_options: Vec<(String, String)> = opts.option;
    for var in vars() {
        match var.0.strip_prefix("OPTION_") {
            None => continue,
            Some(key) => raw_options.push((key.to_string(), var.1)),
        };
    }

    let mut effective_options: BTreeMap<String, String> = BTreeMap::new();
    for (key, value) in raw_options {
        if !config.options.contains_key(&key) {
            bail!("User option `{}` is not defined in the config", key);
        }

        let allowed_values = &config.options[&key];
        if !allowed_values.contains(&value) {
            bail!("User option `{}` does not allow the value `{}`. List of allowed values: {:?}", key, value, allowed_values)
        }

        effective_options.insert(key, value);
    }

    for (key, values) in &config.options {
        if effective_options.contains_key(key) {
            continue;
        }

        effective_options.insert(key.clone(), values[0].clone());
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
            clean_build: build_opts.clean,
        }),
        MainCommand::Purge => purge(context),
        MainCommand::List => list(context),
        MainCommand::Wipe { kind } => wipe(context, kind),
        MainCommand::Path { recipe } => path(context, recipe),
        MainCommand::Logs { recipe, kind } => logs(context, recipe, kind),
        MainCommand::Completions { shell: _ } => Ok(()),
    }
}

fn resolve_recipe(config: &Config, namespace: &str, name: &str) -> Option<ConfigRecipeId> {
    for (_, recipe) in &config.recipes {
        if recipe.namespace.to_string() != namespace {
            continue;
        }

        if recipe.name != name {
            continue;
        }

        return Some(recipe.id);
    }
    return None;
}

fn resolve_recipe_from_selector(config: &Config, recipe_selector: &String) -> Option<ConfigRecipeId> {
    let (namespace, name) = match recipe_selector.split_once("/") {
        None => return None,
        Some(selector) => selector,
    };

    resolve_recipe(config, namespace, name)
}

fn walk_cached_recipes(context: &ChariotContext, callback: impl Fn(&str, &str, &BTreeMap<&str, &str>, &RecipeState) -> Result<bool>) -> Result<()> {
    fn walk_recipe(
        context: &ChariotContext,
        namespace: &str,
        name: &str,
        options: BTreeMap<&str, &str>,
        path: &Path,
        callback: &impl Fn(&str, &str, &BTreeMap<&str, &str>, &RecipeState) -> Result<bool>,
    ) -> Result<()> {
        if let Some(state) = RecipeState::read(path).context("Failed tor read recipe state")? {
            if callback(namespace, name, &options, &state)? {
                return Ok(());
            }
        }

        let options_dir = path.join("opt");
        if !exists(&options_dir)? {
            return Ok(());
        }

        for option_dir in read_dir(&options_dir)? {
            let option_dir = option_dir.unwrap();

            for value_dir in read_dir(option_dir.path())? {
                let value_dir = value_dir.unwrap();

                let option = option_dir.file_name().to_string_lossy().to_string();
                let value = value_dir.file_name().to_string_lossy().to_string();
                let mut options = options.clone();
                options.insert(option.as_str(), value.as_str());

                walk_recipe(context, namespace, name, options, &value_dir.path(), callback)?;
            }
        }

        Ok(())
    }

    for namespace in ["source", "package", "tool", "custom"] {
        let path = context.cache.path_recipes().join(namespace);
        if !exists(&path)? {
            continue;
        }

        for recipe_dir in read_dir(&path)? {
            let recipe_dir = recipe_dir.unwrap();
            let name = recipe_dir.file_name().to_string_lossy().to_string();

            walk_recipe(context, namespace, name.as_str(), BTreeMap::new(), &recipe_dir.path(), &callback)?;
        }
    }

    Ok(())
}

fn options_string(opts: &BTreeMap<&str, &str>) -> Option<String> {
    let mut opt_strings = Vec::new();
    for (k, v) in opts {
        let mut opt_string = String::new();
        opt_string.push_str(k);
        opt_string.push_str(" = ");
        opt_string.push_str(v);
        opt_strings.push(opt_string);
    }

    if opt_strings.len() == 0 {
        return None;
    }

    return Some(opt_strings.join(", "));
}

fn exec(context: ChariotContext, exec_opts: ExecOptions) -> Result<()> {
    let cmd = exec_opts.command.join(" ");

    let mut extra_deps = Vec::new();
    for dep in exec_opts.dependency {
        let recipe_id = match resolve_recipe_from_selector(&context.config, &dep) {
            Some(id) => id,
            None => bail!("Unknown dependency `{}`", &dep),
        };
        extra_deps.push(recipe_id);
    }

    let mut runtime_config = match exec_opts.recipe_context {
        Some(recipe) => match resolve_recipe_from_selector(&context.config, &recipe) {
            Some(recipe_id) => context
                .setup_runtime_config(Some(recipe_id), Some(exec_opts.package), Some(extra_deps))
                .context("Failed to setup recipe context")?,
            None => bail!("Failed to setup recipe context"),
        },
        None => RuntimeConfig::new(context.rootfs.subset(BTreeSet::from_iter(exec_opts.package))?),
    };

    runtime_config.read_only = !exec_opts.rw;
    runtime_config.uid = Uid::from(exec_opts.uid);
    runtime_config.gid = Gid::from(exec_opts.gid);

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
        match resolve_recipe_from_selector(&context.common.config, recipe) {
            None => warn!("Unknown recipe `{}` ignoring...", recipe),
            Some(recipe_id) => chosen_recipes.push(recipe_id),
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

fn list(context: ChariotContext) -> Result<()> {
    info!("Listing all recipes found in cache");
    println!("{} - Recipe in cache", "■".green());
    println!("{} - Recipe in cache but failed to build or invalidated", "■".yellow());
    println!("{} - Recipe in cache but missing from config", "■".red());
    println!("{} - Total size of the recipe (includes build cache + source tars)", "■".blue());
    println!("{} - Timestamp of the last build", "■".magenta());

    walk_cached_recipes(&context, |namespace, name, opts, state| {
        let mut line = String::new();

        let mut id = None;
        for (_, recipe) in &context.config.recipes {
            if recipe.namespace.to_string() != namespace {
                continue;
            }

            if recipe.name != name {
                continue;
            }

            id = Some(recipe.id);
            break;
        }

        let mut recipe_style = Style::new().green();
        if !state.intact || state.invalidated {
            recipe_style = Style::new().yellow();
        }
        match id {
            None => recipe_style = Style::new().red(),
            Some(id) => {
                let recipe_opts = &context.config.options_map[&id];
                let mut matching = 0;
                for (option, _) in opts {
                    if !recipe_opts.contains(&option.to_string()) {
                        break;
                    }

                    matching += 1;
                }

                if matching != opts.len() {
                    recipe_style = Style::new().red();
                }
            }
        }

        line.push_str(format!("{}/{}", namespace.style(recipe_style), name.style(recipe_style)).as_str());

        if let Some(str) = options_string(opts) {
            line.push_str(format!(" [{}]", str).as_str());
        }

        if state.size > 0 {
            line.push_str(format!(" | {}", ByteSize(state.size).to_string().blue().bold()).as_str());
        }

        if let Some(timestamp) = DateTime::from_timestamp_secs(state.timestamp as i64) {
            line.push_str(format!(" | {}", timestamp.format("%y/%m/%d %H:%M:%S").magenta()).as_str());
        }

        println!("{}", line);

        Ok(false)
    })?;

    Ok(())
}

fn purge(context: ChariotContext) -> Result<()> {
    info!("Purging recipes...");

    walk_cached_recipes(&context, |namespace, name, opts, state| {
        let recipe_id = resolve_recipe(&context.config, namespace, name);

        let recipe_path = context.cache.path_recipe(namespace, name, opts);

        let mut size_str = String::new();
        if state.size > 0 {
            size_str = format!("({}) ", ByteSize(state.size).to_string());
        }

        let recipe_id = match recipe_id {
            None => {
                warn!("Purging {}`{}/{}`", size_str, namespace, name);
                clean(recipe_path).context("Failed to purge recipe")?;

                return Ok(true);
            }
            Some(recipe_id) => recipe_id,
        };

        let recipe_opts = &context.config.options_map[&recipe_id];

        let mut matching = 0;
        for (option, _) in opts {
            if !recipe_opts.contains(&option.to_string()) {
                break;
            }

            matching += 1;
        }

        if recipe_opts.len() != matching {
            let mut opts_str = String::new();
            if let Some(str) = options_string(opts) {
                opts_str = format!(" [{}]", str);
            }

            warn!("Purging {}`{}/{}`{}", size_str, namespace, name, opts_str);
            clean_within(&recipe_path, Some(vec!["opts"]))?;

            let mut current_dir = recipe_path;
            while current_dir.read_dir()?.next().is_none() {
                let parent_dir = current_dir.parent();

                remove_dir(&current_dir).context("Failed to purge directory")?;

                match parent_dir {
                    None => break,
                    Some(parent_dir) => current_dir = parent_dir.to_path_buf(),
                }
            }
        }

        Ok(false)
    })?;

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
                let recipe_id = match resolve_recipe_from_selector(&context.config, &recipe_selector) {
                    Some(recipe_id) => recipe_id,
                    None => continue,
                };
                clean(context.path_recipe(recipe_id)).with_context(|| format!("Failed to wipe recipe `{}`", context.config.recipes[&recipe_id]))?;
            }
        }
    }

    Ok(())
}

fn path(context: ChariotContext, recipe: String) -> Result<()> {
    match resolve_recipe_from_selector(&context.config, &recipe) {
        Some(recipe_id) => {
            let recipe_path = context.path_recipe(recipe_id).join(match context.config.recipes[&recipe_id].namespace {
                ConfigNamespace::Source(_) => "src",
                ConfigNamespace::Package(_) | ConfigNamespace::Tool(_) | ConfigNamespace::Custom(_) => "install",
            });
            print!("{}", recipe_path.canonicalize().context("Failed to canonicalize recipe path")?.to_str().unwrap());
            Ok(())
        }
        None => bail!("Unknown recipe `{}`", recipe),
    }
}

fn logs(context: ChariotContext, recipe: String, kind: String) -> Result<()> {
    match resolve_recipe_from_selector(&context.config, &recipe) {
        Some(recipe_id) => {
            let log_path = context.path_recipe(recipe_id).join("logs");
            let log_file = log_path.join(kind.clone() + ".log");

            if !exists(&log_file)? {
                if exists(&log_path)? {
                    info!("Log files found:");
                    for entry in read_dir(log_path)? {
                        let entry = entry?;

                        info!("- {}", entry.file_name().to_str().unwrap());
                    }
                }
                bail!("Unknown log file `{}.log`", kind);
            }

            let log = read_to_string(&log_file).context("Failed to read log file")?;

            print!("{}", log);

            Ok(())
        }
        None => bail!("Unknown recipe `{}`", recipe),
    }
}
