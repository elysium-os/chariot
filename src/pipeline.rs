use std::{
    cell::RefCell,
    collections::HashMap,
    fs::{create_dir_all, exists, remove_dir_all, write},
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context, Result, bail};
use log::info;

use crate::{
    container::{
        Container,
        runtime::{EnvVar, Mount, RuntimeConfig},
    },
    recipe::{Kind, Recipe, RecipeDependency, RecipeId, RecipeState, SourceKind},
    util::{copy_recursive, get_timestamp},
};

pub struct Pipeline {
    config: PipelineConfig,
    cache_path: PathBuf,
    container: Rc<Container>,

    recipes: HashMap<RecipeId, Recipe>,
    dependencies: HashMap<RecipeId, Vec<RecipeDependency>>,

    invalidated_recipes: RefCell<Vec<RecipeId>>,
    attempted_recipes: RefCell<Vec<RecipeId>>,
}

pub struct PipelineConfig {
    pub prefix: String,
    pub thread_count: u32,
    pub stdout_quiet: bool,
    pub stderr_quiet: bool,
}

impl Pipeline {
    pub fn new(
        cache_path: impl AsRef<Path>,
        container: Rc<Container>,
        recipes: HashMap<RecipeId, Recipe>,
        dependencies: HashMap<RecipeId, Vec<RecipeDependency>>,
        config: PipelineConfig,
    ) -> Pipeline {
        Pipeline {
            config,
            cache_path: cache_path.as_ref().to_path_buf(),
            container,
            recipes,
            dependencies,
            invalidated_recipes: RefCell::new(Vec::new()),
            attempted_recipes: RefCell::new(Vec::new()),
        }
    }

    fn recipes_path(&self) -> PathBuf {
        self.cache_path.join("recipes")
    }

    fn dependencies_path(&self) -> PathBuf {
        self.cache_path.join("dependencies")
    }

    fn host_dependencies_path(&self) -> PathBuf {
        self.dependencies_path().join("host")
    }

    fn target_dependencies_path(&self) -> PathBuf {
        self.dependencies_path().join("target")
    }

    pub fn invalidate_recipe(&self, recipe_id: RecipeId) -> Result<()> {
        let recipe = &self.recipes[&recipe_id];

        self.invalidated_recipes.borrow_mut().push(recipe.id);

        if exists(recipe.path(&self.recipes_path()))? {
            recipe.state_invalidate(&self.recipes_path())?;
        }
        Ok(())
    }

    pub fn execute(self) -> Result<()> {
        self.invalidated_recipes.borrow_mut().dedup();

        for recipe_id in self.invalidated_recipes.borrow().iter() {
            let recipe = &self.recipes[recipe_id];

            self.process_recipe(recipe, Vec::new())
                .context(format!("Failed to process recipe {}/{}", recipe.namespace_string(), recipe.name))?;

            if self.attempted_recipes.borrow().contains(&recipe.id) {
                continue;
            }

            self.process_recipe(recipe, Vec::new())
                .context(format!("Failed to process recipe {}/{}", recipe.namespace_string(), recipe.name))?;
        }

        Ok(())
    }

    fn process_recipe(&self, recipe: &Recipe, mut in_flight: Vec<RecipeId>) -> Result<u64> {
        in_flight.push(recipe.id);

        let mut latest_recipe: u64 = 0;
        for dependency in self.dependencies[&recipe.id].iter() {
            let dependency_recipe = &self.recipes[&dependency.recipe_id];

            if in_flight.contains(&dependency_recipe.id) {
                bail!("Recursive dependency {}/{}", dependency_recipe.namespace_string(), dependency_recipe.name)
            }

            let timestamp = self.process_recipe(dependency_recipe, in_flight.clone()).context(format!(
                "Broken dependency {}/{}",
                dependency_recipe.namespace_string(),
                dependency_recipe.name
            ))?;

            if timestamp > latest_recipe {
                latest_recipe = timestamp;
            }
        }

        let recipe_path = recipe.path(&self.recipes_path());

        // Check whether this recipe is invalidated
        let state = recipe.state_parse(&self.recipes_path()).context("Failed to parse recipe state")?;
        if let Some(state) = state {
            if state.intact && !state.invalidated && state.timestamp >= latest_recipe {
                return Ok(state.timestamp);
            }
        }

        info!("Processing {}/{}", recipe.namespace_string(), recipe.name);

        // Lets not attempt recipes multiple times during the same pipeline
        if self.attempted_recipes.borrow().contains(&recipe.id) {
            bail!("Already attempted to process recipe {}/{}", recipe.namespace_string(), recipe.name);
        }
        self.attempted_recipes.borrow_mut().push(recipe.id);

        // Setup recipe directory
        if exists(&recipe_path)? {
            remove_dir_all(&recipe_path).context("Failed to clean recipe dir")?;
        }
        create_dir_all(&recipe_path).context("Failed to create recipe dir")?;
        recipe.state_write(
            &self.recipes_path(),
            RecipeState {
                intact: false,
                invalidated: false,
                timestamp: get_timestamp()?,
            },
        )?;

        // Install dependencies
        if exists(self.dependencies_path())? {
            remove_dir_all(self.dependencies_path()).context("Failed to clean dependencies dir")?;
        }
        create_dir_all(self.host_dependencies_path()).context("Failed to create host dependencies dir")?;
        create_dir_all(self.target_dependencies_path()).context("Failed to create target dependencies dir")?;

        let mut source_dependency_mounts: Vec<Mount> = Vec::new();
        for dependency in &self.dependencies[&recipe.id] {
            self.install_dependency(dependency, &mut Vec::new(), &mut source_dependency_mounts)
                .context("Failed to install dependency")?;
        }

        let target_dependency_mount = Mount::new(self.target_dependencies_path().to_str().unwrap(), "/chariot/sysroot").read_only();
        let host_dependency_mount = Mount::new(self.host_dependencies_path().to_str().unwrap(), "/usr/local").read_only();

        // Image dependencies
        let set = self
            .container
            .get_set(&recipe.image_dependencies)
            .context("Failed to get container set")?;

        // Process
        match &recipe.kind {
            Kind::Source(src) => {
                let src_path = recipe_path.join("src");
                create_dir_all(&src_path)?;

                let mut runtime_config = RuntimeConfig::default(&set)
                    .set_cwd("/chariot/source")
                    .set_quiet(self.config.stdout_quiet, self.config.stderr_quiet)
                    .add_mount(Mount::new(recipe_path.to_str().unwrap(), "/chariot/source"));

                match &src.kind {
                    SourceKind::Local => {
                        if !exists(&src.url)? {
                            bail!("Local directory `{}` not found", src.url);
                        }
                        copy_recursive(Path::new(&src.url), &src_path).context("recursive copy failed")?;
                    }
                    SourceKind::Git(reference) => {
                        runtime_config
                            .run_shell(format!("git clone --depth=1 {} /chariot/source/src", &src.url))
                            .context("git clone failed")?;
                        runtime_config
                            .run_shell(format!("git -C /chariot/source/src fetch --depth=1 origin {}", reference))
                            .context("git fetch failed")?;
                        runtime_config
                            .run_shell(format!("git -C /chariot/source/src checkout {}", reference))
                            .context("git checkout failed")?;
                    }
                    SourceKind::TarGz(b2sum) | SourceKind::TarXz(b2sum) => {
                        write(recipe_path.join("b2sums.txt"), format!("{} /chariot/source/archive", b2sum)).context("Failed to write b2sums.txt")?;
                        runtime_config
                            .run_shell(format!("wget --no-hsts -qO /chariot/source/archive {}", src.url))
                            .context("wget failed")?;
                        runtime_config
                            .run_shell("b2sum --check /chariot/source/b2sums.txt")
                            .context("b2sums failed for source")?;

                        let tar_type = match &src.kind {
                            SourceKind::TarGz(_) => "--gzip",
                            SourceKind::TarXz(_) => "--zstd",
                            _ => bail!("invalid tar type"),
                        };

                        runtime_config.run_shell(format!("tar --no-same-owner --no-same-permissions --strip-components 1 -x {} -C /chariot/source/src -f /chariot/source/archive", &tar_type)).context("context")?;
                    }
                };

                if let Some(patch) = &src.patch {
                    if !exists(patch)? {
                        bail!("Failed to locate patch file");
                    }

                    runtime_config.mounts.clear();
                    runtime_config.mounts.push(Mount::new(src_path.to_str().unwrap(), "/chariot/source"));
                    runtime_config.mounts.push(Mount::new(patch, "/chariot/patch").is_file().read_only());

                    runtime_config.run_shell("patch -p1 -i /chariot/patch").context("Failed to apply patch")?;
                }

                if let Some(regenerate) = &src.regenerate {
                    runtime_config.mounts.clear();
                    runtime_config.mounts.push(Mount::new(src_path.to_str().unwrap(), "/chariot/source"));
                    runtime_config.mounts.push(target_dependency_mount);
                    runtime_config.mounts.push(host_dependency_mount);
                    runtime_config.mounts.append(&mut source_dependency_mounts);

                    runtime_config.env.push(EnvVar::new("SOURCES_DIR", "/chariot/sources"));
                    runtime_config.env.push(EnvVar::new("SYSROOT_DIR", "/chariot/sysroot"));

                    match regenerate.lang.as_str() {
                        "bash" | "sh" => {
                            runtime_config.run_shell(&regenerate.code).context("Failed to run shell regenerate")?;
                        }
                        "python" | "py" => {
                            runtime_config.run_python(&regenerate.code).context("Failed to run python regenerate")?;
                        }
                        lang => bail!("unsupported language embed `{}`", lang),
                    }
                }
            }
            Kind::Tool(common) | Kind::Bare(common) | Kind::Package(common) => {
                let cache_path = recipe_path.join("cache");
                let build_path = recipe_path.join("build");
                let install_path = recipe_path.join("install");
                let logs_path = recipe_path.join("logs");

                create_dir_all(&cache_path).context("Failed to create cache path")?;
                create_dir_all(&build_path).context("Failed to create build path")?;
                create_dir_all(&install_path).context("Failed to create install path")?;
                create_dir_all(&logs_path).context("Failed to create logs path")?;

                let mut runtime_config = RuntimeConfig::default(&set)
                    .set_cwd("/chariot/build")
                    .add_mount(Mount::new(cache_path.to_str().unwrap(), "/chariot/cache"))
                    .add_mount(Mount::new(build_path.to_str().unwrap(), "/chariot/build"))
                    .add_mount(Mount::new(install_path.to_str().unwrap(), "/chariot/install"))
                    .add_mount(target_dependency_mount)
                    .add_mount(host_dependency_mount)
                    .set_quiet(self.config.stdout_quiet, self.config.stderr_quiet);

                runtime_config.mounts.append(&mut source_dependency_mounts);

                let mut prefix = self.config.prefix.clone();
                if matches!(recipe.kind, Kind::Tool(_)) {
                    prefix = String::from("/usr/local");
                }

                for stage in [
                    ("configure", &common.configure, vec![]),
                    ("build", &common.build, vec![]),
                    ("install", &common.install, vec![EnvVar::new("INSTALL_DIR", "/chariot/install")]),
                ] {
                    let code_block = match stage.1 {
                        Some(v) => v,
                        None => continue,
                    };

                    runtime_config.env.clear();
                    runtime_config.env.push(EnvVar::new("SOURCES_DIR", String::from("/chariot/sources")));
                    runtime_config.env.push(EnvVar::new("SYSROOT_DIR", String::from("/chariot/sysroot")));
                    runtime_config.env.push(EnvVar::new("CACHE_DIR", String::from("/chariot/cache")));
                    runtime_config.env.push(EnvVar::new("BUILD_DIR", String::from("/chariot/build")));
                    runtime_config.env.push(EnvVar::new("PREFIX", prefix.clone()));
                    runtime_config.env.push(EnvVar::new("THREAD_COUNT", self.config.thread_count.to_string()));

                    runtime_config.set_log_file(
                        Some(logs_path.join(stage.0.to_owned() + ".stdout.log")),
                        Some(logs_path.join(stage.0.to_owned() + ".stderr.log")),
                    );

                    for var in stage.2 {
                        runtime_config.env.push(var);
                    }

                    match code_block.lang.as_str() {
                        "bash" | "sh" => {
                            runtime_config
                                .run_shell(&code_block.code)
                                .context(format!("Failed to run shell `{}` recipe", stage.0))?;
                        }
                        "python" | "py" => {
                            runtime_config
                                .run_python(&code_block.code)
                                .context(format!("Failed to run python `{}` recipe", stage.0))?;
                        }
                        lang => bail!("unsupported language embed `{}`", lang),
                    }
                }
            }
        }

        let timestamp = get_timestamp()?;
        recipe.state_write(
            &self.recipes_path(),
            RecipeState {
                intact: true,
                invalidated: false,
                timestamp,
            },
        )?;

        Ok(timestamp)
    }

    fn install_dependency(&self, dependency: &RecipeDependency, installed: &mut Vec<RecipeId>, source_mounts: &mut Vec<Mount>) -> Result<()> {
        if installed.contains(&dependency.recipe_id) {
            return Ok(());
        }
        installed.push(dependency.recipe_id);

        let recipe = &self.recipes[&dependency.recipe_id];
        match &recipe.kind {
            Kind::Source(_) => {
                source_mounts.push(
                    Mount::new(
                        recipe.path(&self.recipes_path()).join("src").to_str().unwrap(),
                        Path::new("/chariot/sources").join(Path::new(&recipe.name)).to_str().unwrap(),
                    )
                    .read_only(),
                );
            }
            Kind::Package(_) => copy_recursive(recipe.path(&self.recipes_path()).join("install"), self.target_dependencies_path())
                .context("Failed to copy package to target deps dir")?,
            Kind::Tool(_) => copy_recursive(
                recipe.path(&self.recipes_path()).join("install").join("usr").join("local"),
                self.host_dependencies_path(),
            )
            .context("Failed to copy tool to host deps dir")?,
            Kind::Bare(_) => {}
        }

        for dependency in &self.dependencies[&recipe.id] {
            if !dependency.runtime {
                continue;
            }

            self.install_dependency(dependency, installed, source_mounts)
                .context("Broken dependency install")?;
        }
        Ok(())
    }
}
