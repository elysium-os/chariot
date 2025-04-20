use std::{
    cell::RefCell,
    fs::{create_dir_all, exists, remove_dir_all, write},
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{bail, Context, Result};
use log::info;

use crate::{
    config::Config,
    container::{
        runtime::{EnvVar, Mount, RuntimeConfig},
        Container,
    },
    recipe::{Kind, Recipe, RecipeDependency, RecipeId, RecipeState, SourceKind},
    util::{copy_recursive, get_timestamp},
};

pub struct Pipeline {
    options: PipelineOptions,
    cache_path: PathBuf,
    container: Rc<Container>,
    config: Config,

    invalidated_recipes: RefCell<Vec<RecipeId>>,
    attempted_recipes: RefCell<Vec<RecipeId>>,
}

pub struct PipelineOptions {
    pub prefix: String,
    pub thread_count: u32,
    pub quiet: bool,
}

impl Pipeline {
    pub fn new(cache_path: impl AsRef<Path>, container: Rc<Container>, options: PipelineOptions, config: Config) -> Pipeline {
        Pipeline {
            options,
            config,
            cache_path: cache_path.as_ref().to_path_buf(),
            container,
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
        let recipe = &self.config.recipes[&recipe_id];

        self.invalidated_recipes.borrow_mut().push(recipe.id);

        if exists(recipe.path(&self.recipes_path()))? {
            recipe.state_invalidate(&self.recipes_path())?;
        }
        Ok(())
    }

    pub fn execute(self) -> Result<()> {
        self.invalidated_recipes.borrow_mut().dedup();

        for recipe_id in self.invalidated_recipes.borrow().iter() {
            let recipe = &self.config.recipes[recipe_id];

            self.process_recipe(recipe, Vec::new())
                .with_context(|| format!("Failed to process recipe {}", recipe))?;

            if self.attempted_recipes.borrow().contains(&recipe.id) {
                continue;
            }

            self.process_recipe(recipe, Vec::new())
                .with_context(|| format!("Failed to process recipe {}", recipe))?;
        }

        Ok(())
    }

    fn process_recipe(&self, recipe: &Recipe, mut in_flight: Vec<RecipeId>) -> Result<u64> {
        in_flight.push(recipe.id);

        let mut latest_recipe: u64 = 0;
        for dependency in self.config.dependency_map[&recipe.id].iter() {
            let dependency_recipe = &self.config.recipes[&dependency.recipe_id];

            if in_flight.contains(&dependency_recipe.id) {
                bail!("Recursive dependency {}", dependency_recipe)
            }

            let timestamp = self
                .process_recipe(dependency_recipe, in_flight.clone())
                .with_context(|| format!("Broken dependency {}", dependency_recipe))?;

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

        info!("Processing {}", recipe);

        // Lets not attempt recipes multiple times during the same pipeline
        if self.attempted_recipes.borrow().contains(&recipe.id) {
            bail!("Already attempted to process recipe {}", recipe);
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
        let mut source_dependency_custom: Vec<Mount> = Vec::new();

        let mut installed: Vec<RecipeId> = Vec::new();
        for dependency in &self.config.dependency_map[&recipe.id] {
            self.install_dependency(
                dependency,
                &mut installed,
                &mut source_dependency_mounts,
                &mut source_dependency_custom,
                recipe.mutable_sources,
            )
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
                    .add_mount(Mount::new(recipe_path.to_str().unwrap(), "/chariot/source"));

                runtime_config.set_output(None, self.options.quiet);

                match &src.kind {
                    SourceKind::Local => {
                        if !exists(&src.url)? {
                            bail!("Local directory `{}` not found", src.url);
                        }
                        copy_recursive(Path::new(&src.url), &src_path).context("recursive copy failed")?;
                    }
                    SourceKind::Git(revision) => {
                        runtime_config
                            .run_shell(format!("git clone --depth=1 {} /chariot/source/src", &src.url))
                            .context("git clone failed")?;
                        runtime_config
                            .run_shell(format!("git -C /chariot/source/src fetch --depth=1 origin {}", revision))
                            .context("git fetch failed")?;
                        runtime_config
                            .run_shell(format!("git -C /chariot/source/src checkout FETCH_HEAD"))
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
                    runtime_config.mounts.append(&mut source_dependency_custom);

                    for env in &self.config.global_env {
                        runtime_config.env.push(EnvVar::new(env.0, env.1));
                    }
                    runtime_config.env.push(EnvVar::new("SOURCES_DIR", "/chariot/sources"));
                    runtime_config.env.push(EnvVar::new("CUSTOMS_DIR", "/chariot/custom"));
                    runtime_config.env.push(EnvVar::new("SYSROOT_DIR", "/chariot/sysroot"));

                    match regenerate.lang.as_str() {
                        "bash" | "sh" => runtime_config.run_shell(&regenerate.code).context("Failed to run shell regenerate")?,
                        "python" | "py" => runtime_config.run_python(&regenerate.code).context("Failed to run python regenerate")?,
                        lang => bail!("unsupported language embed `{}`", lang),
                    }
                }
            }
            Kind::Tool(common) | Kind::Custom(common) | Kind::Package(common) => {
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
                    .add_mount(host_dependency_mount);

                runtime_config.mounts.append(&mut source_dependency_mounts);
                runtime_config.mounts.append(&mut source_dependency_custom);

                let mut prefix = self.options.prefix.clone();
                if matches!(recipe.kind, Kind::Tool(_)) {
                    prefix = String::from("/usr/local");
                }

                for stage in [
                    ("configure", "Configuring", &common.configure, vec![]),
                    ("build", "Building", &common.build, vec![]),
                    (
                        "install",
                        "Installing",
                        &common.install,
                        vec![EnvVar::new("INSTALL_DIR", "/chariot/install")],
                    ),
                ] {
                    info!("{} {}", stage.1, recipe);

                    let code_block = match stage.2 {
                        Some(v) => v,
                        None => continue,
                    };

                    runtime_config.env.clear();
                    for env in &self.config.global_env {
                        runtime_config.env.push(EnvVar::new(env.0, env.1));
                    }
                    runtime_config.env.push(EnvVar::new("SOURCES_DIR", String::from("/chariot/sources")));
                    runtime_config.env.push(EnvVar::new("CUSTOMS_DIR", "/chariot/custom"));
                    runtime_config.env.push(EnvVar::new("SYSROOT_DIR", String::from("/chariot/sysroot")));
                    runtime_config.env.push(EnvVar::new("CACHE_DIR", String::from("/chariot/cache")));
                    runtime_config.env.push(EnvVar::new("BUILD_DIR", String::from("/chariot/build")));
                    runtime_config
                        .env
                        .push(EnvVar::new("THREAD_COUNT", self.options.thread_count.to_string()));

                    runtime_config.env.push(EnvVar::new("PREFIX", prefix.clone()));

                    for var in stage.3 {
                        runtime_config.env.push(var);
                    }

                    runtime_config.set_output(Some(logs_path.join(stage.0.to_owned() + ".log")), self.options.quiet);

                    match code_block.lang.as_str() {
                        "bash" | "sh" => runtime_config
                            .run_shell(&code_block.code)
                            .with_context(|| format!("Failed to run shell `{}` for `{}`", stage.0, recipe))?,
                        "python" | "py" => runtime_config
                            .run_python(&code_block.code)
                            .with_context(|| format!("Failed to run python `{}` for `{}`", stage.0, recipe))?,
                        lang => bail!("unsupported language embed `{}`", lang),
                    };
                }
            }
            Kind::Collection => {}
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

    fn install_dependency(
        &self,
        dependency: &RecipeDependency,
        installed: &mut Vec<RecipeId>,
        source_mounts: &mut Vec<Mount>,
        custom_mounts: &mut Vec<Mount>,
        mutable_sources: bool,
    ) -> Result<()> {
        if installed.contains(&dependency.recipe_id) {
            return Ok(());
        }
        installed.push(dependency.recipe_id);

        let recipe = &self.config.recipes[&dependency.recipe_id];
        match &recipe.kind {
            Kind::Source(_) => {
                let mut mount = Mount::new(
                    recipe.path(&self.recipes_path()).join("src").to_str().unwrap(),
                    Path::new("/chariot/sources").join(Path::new(&recipe.name)).to_str().unwrap(),
                );
                if !mutable_sources {
                    mount = mount.read_only();
                }
                source_mounts.push(mount);
            }
            Kind::Package(_) => copy_recursive(recipe.path(&self.recipes_path()).join("install"), self.target_dependencies_path())
                .context("Failed to copy package to target deps dir")?,
            Kind::Tool(_) => copy_recursive(
                recipe.path(&self.recipes_path()).join("install").join("usr").join("local"),
                self.host_dependencies_path(),
            )
            .context("Failed to copy tool to host deps dir")?,
            Kind::Custom(_) => custom_mounts.push(
                Mount::new(
                    recipe.path(&self.recipes_path()).join("install").to_str().unwrap(),
                    Path::new("/chariot/custom").join(Path::new(&recipe.name)).to_str().unwrap(),
                )
                .read_only(),
            ),
            Kind::Collection => {}
        }

        for dependency in &self.config.dependency_map[&recipe.id] {
            if !dependency.runtime {
                continue;
            }

            self.install_dependency(dependency, installed, source_mounts, custom_mounts, mutable_sources)
                .context("Broken dependency install")?;
        }
        Ok(())
    }
}
