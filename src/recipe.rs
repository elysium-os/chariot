use std::{
    collections::BTreeSet,
    fs::{create_dir_all, exists, read_to_string, write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use log::info;

use crate::{
    config::{ConfigNamespace, ConfigRecipeDependency, ConfigRecipeId, ConfigSourceKind},
    runtime::{Mount, OutputConfig, RuntimeConfig},
    util::{clean, clean_within, copy_recursive, get_timestamp},
    ChariotBuildContext, ChariotContext,
};

struct RecipeState {
    intact: bool,
    invalidated: bool,
    timestamp: u64,
}

impl ChariotBuildContext {
    pub fn recipe_process(
        &self,
        mut in_flight: Vec<ConfigRecipeId>,
        attempted_recipes: &mut Vec<ConfigRecipeId>,
        invalidated_recipes: &Vec<ConfigRecipeId>,
        recipe_id: ConfigRecipeId,
    ) -> Result<u64> {
        in_flight.push(recipe_id);

        // Process dependencies
        let mut latest_recipe_timestamp: u64 = 0;
        for dependency in &self.common.config.dependency_map[&recipe_id] {
            let recipe = &self.common.config.recipes[&dependency.recipe_id];

            if in_flight.contains(&recipe.id) {
                bail!("Recursive dependency `{}`", recipe);
            }

            let timestamp = self
                .recipe_process(in_flight.clone(), attempted_recipes, invalidated_recipes, recipe.id)
                .with_context(|| format!("Broken dependency `{}`", recipe))?;

            if timestamp > latest_recipe_timestamp {
                latest_recipe_timestamp = timestamp;
            }
        }

        // Check invalidation status
        let state = self.common.recipe_state_parse(recipe_id).context("Failed to parse recipe state")?;
        if let Some(state) = state {
            if state.intact && !state.invalidated && state.timestamp >= latest_recipe_timestamp {
                return Ok(state.timestamp);
            }
        }

        let recipe = &self.common.config.recipes[&recipe_id];

        // Avoid attempting recipes multiple times
        if attempted_recipes.contains(&recipe.id) {
            bail!("Already attempted to process recipe `{}`", recipe);
        }

        // Process recipe
        info!("Processing recipe `{}`", recipe);

        let logs_path = self.common.path_recipe(recipe.id).join("logs");
        clean(&logs_path).context("Failed to clean logs dir")?;
        create_dir_all(&logs_path).context("Failed to create recipe logs dir")?;

        match &recipe.namespace {
            ConfigNamespace::Source(src) => {
                let src_dir = self.common.path_recipe(recipe.id).join("src");
                clean_within(&src_dir).context("Failed to clean source recipe src dir")?;
                create_dir_all(&src_dir).context("Failed to create source recipe src dir")?;

                let aux_dir = self.common.path_recipe(recipe.id).join("aux");
                clean(&aux_dir).context("Failed to clean source recipe auxiliary dir")?;
                create_dir_all(&aux_dir).context("Failed to create source recipe auxiliary dir")?;

                let mut runtime_config = RuntimeConfig::new(self.common.rootfs.root())
                    .set_cwd("/chariot/source")
                    .add_mount(Mount::new(self.common.path_recipe(recipe.id), "/chariot/source"))
                    .set_output_config(OutputConfig {
                        quiet: !self.common.verbose,
                        log_path: Some(logs_path.join("fetch.log")),
                    });

                match &src.kind {
                    ConfigSourceKind::Local => {
                        if !exists(&src.url)? {
                            bail!("Local directory `{}` not found", src.url);
                        }

                        copy_recursive(Path::new(&src.url), &src_dir).context("Failed to copy local source")?;
                    }
                    ConfigSourceKind::Git(revision) => {
                        runtime_config
                            .run_shell(format!("git clone --depth=1 {} /chariot/source/src", &src.url))
                            .context("Git clone failed for git source")?;
                        runtime_config
                            .run_shell(format!("git -C /chariot/source/src fetch --depth=1 origin {}", revision))
                            .context("Git fetch failed for git source")?;
                        runtime_config
                            .run_shell(format!("git -C /chariot/source/src checkout FETCH_HEAD"))
                            .context("Git checkout failed for git source")?;
                    }
                    ConfigSourceKind::TarGz(b2sum) | ConfigSourceKind::TarXz(b2sum) => {
                        write(
                            self.common.path_recipe(recipe.id).join("aux").join("b2sums.txt"),
                            format!("{} /chariot/source/aux/archive", b2sum),
                        )
                        .context("Failed to write b2sums.txt")?;

                        runtime_config
                            .run_shell(format!("wget --no-hsts -qO /chariot/source/aux/archive {}", src.url))
                            .context("Failed to fetch (wget) tar source")?;

                        runtime_config
                            .run_shell("b2sum --check /chariot/source/aux/b2sums.txt")
                            .context("b2sums failed for tar source")?;

                        let tar_type = match &src.kind {
                            ConfigSourceKind::TarGz(_) => "--gzip",
                            ConfigSourceKind::TarXz(_) => "--zstd",
                            _ => bail!("Unknown tar type"),
                        };

                        runtime_config
                            .run_shell(format!(
                                "tar --no-same-owner --no-same-permissions --strip-components 1 -x {} -C /chariot/source/src -f /chariot/source/aux/archive",
                                &tar_type
                            ))
                            .context("Failed to extract tar source")?;
                    }
                }

                if let Some(patch) = &src.patch {
                    if !exists(patch)? {
                        bail!("Failed to locate patch file");
                    }

                    runtime_config.output_config = Some(OutputConfig {
                        quiet: !self.common.verbose,
                        log_path: Some(logs_path.join("patch.log")),
                    });
                    runtime_config.cwd = Path::new("/chariot/source/src").to_path_buf();
                    runtime_config.mounts.push(Mount::new(patch, "/chariot/patch").is_file().read_only());
                    runtime_config.run_shell("patch -p1 -i /chariot/patch").context("Failed to apply patch")?;
                }

                if let Some(regenerate) = &src.regenerate {
                    self.common
                        .recipe_setup_context(recipe.id, None)
                        .context("Failed to setup recipe context")?
                        .set_output_config(OutputConfig {
                            quiet: !self.common.verbose,
                            log_path: Some(logs_path.join("regenerate.log")),
                        })
                        .add_env_var(String::from("PARALLELISM"), self.parallelism.to_string())
                        .run_script(regenerate.lang.as_str(), &regenerate.code)
                        .context("Failed to run regenerate")?;
                }
            }
            ConfigNamespace::Package(common) | ConfigNamespace::Tool(common) | ConfigNamespace::Custom(common) => {
                if common.always_clean || self.clean_build {
                    clean_within(self.common.path_recipe(recipe.id).join("build")).context("Failed to clean recipe build dir")?;
                }
                clean_within(self.common.path_recipe(recipe.id).join("install")).context("Failed to clean recipe install dir")?;

                let mut prefix = self.prefix.clone();
                if matches!(recipe.namespace, ConfigNamespace::Tool(_)) {
                    prefix = String::from("/usr/local");
                }

                let mut runtime_config = self
                    .common
                    .recipe_setup_context(recipe.id, None)
                    .context("Failed to setup recipe context")?
                    .add_env_var(String::from("PREFIX"), prefix)
                    .add_env_var(String::from("PARALLELISM"), self.parallelism.to_string());

                for stage in [("configure", &common.configure), ("build", &common.build), ("install", &common.install)] {
                    let code_block = match stage.1 {
                        Some(v) => v,
                        None => continue,
                    };

                    runtime_config.output_config = Some(OutputConfig {
                        quiet: !self.common.verbose,
                        log_path: Some(logs_path.join(stage.0.to_owned() + ".log")),
                    });

                    runtime_config
                        .run_script(&code_block.lang, &code_block.code)
                        .with_context(|| format!("Failed to run {}", stage.0))?;
                }
            }
        }

        let timestamp = get_timestamp()?;
        self.common.recipe_state_write(
            recipe.id,
            RecipeState {
                intact: true,
                invalidated: false,
                timestamp,
            },
        )?;

        Ok(timestamp)
    }
}

impl ChariotContext {
    pub fn path_recipe(&self, recipe_id: ConfigRecipeId) -> PathBuf {
        let recipe = &self.config.recipes[&recipe_id];
        let mut recipe_path = self.cache.path_recipes().join(recipe.namespace.to_string()).join(recipe.name.clone());
        for opt in &recipe.used_options {
            recipe_path = recipe_path.join("opt").join(opt).join(&self.effective_options[opt]);
        }
        recipe_path
    }

    pub fn recipe_wipe(&self, recipe_id: ConfigRecipeId) -> Result<()> {
        clean(self.path_recipe(recipe_id))
    }

    fn path_recipe_state(&self, recipe_id: ConfigRecipeId) -> PathBuf {
        self.path_recipe(recipe_id).join("state.toml")
    }

    fn recipe_state_parse(&self, recipe_id: ConfigRecipeId) -> Result<Option<RecipeState>> {
        let path = self.path_recipe_state(recipe_id);
        if !exists(&path)? {
            return Ok(None);
        }

        let data = read_to_string(&path).context("Failed to read recipe state")?;
        let table = data.parse::<toml::Table>().context("Failed to parse recipe state")?;
        let intact = table["intact"].as_bool().unwrap_or(false);
        let invalidated = table["invalidated"].as_bool().unwrap_or(false);
        let timestamp = table["timestamp"].as_integer().unwrap_or(0) as u64;
        Ok(Some(RecipeState { intact, invalidated, timestamp }))
    }

    fn recipe_state_write(&self, recipe_id: ConfigRecipeId, state: RecipeState) -> Result<()> {
        let path = self.path_recipe_state(recipe_id);

        let mut state_table = toml::Table::new();
        state_table.insert(String::from("intact"), toml::Value::Boolean(state.intact));
        state_table.insert(String::from("invalidated"), toml::Value::Boolean(state.invalidated));
        state_table.insert(String::from("timestamp"), toml::Value::Integer(state.timestamp as i64));
        write(&path, toml::to_string(&state_table).context("Failed to serialize recipe state")?).context("Failed to write recipe state")
    }

    pub fn recipe_invalidate(&self, recipe_id: ConfigRecipeId) -> Result<()> {
        if !exists(self.path_recipe(recipe_id))? {
            return Ok(());
        }

        let mut new_state = RecipeState {
            intact: false,
            invalidated: true,
            timestamp: get_timestamp()?,
        };
        if let Some(state) = self.recipe_state_parse(recipe_id)? {
            new_state.intact = state.intact;
        }
        self.recipe_state_write(recipe_id, new_state)
    }

    pub fn recipe_setup_context(&self, recipe_id: ConfigRecipeId, extra_packages: Option<Vec<String>>) -> Result<RuntimeConfig> {
        let recipe = &self.config.recipes[&recipe_id];

        let mut image_packages: BTreeSet<String> = BTreeSet::new();
        for dependency in &self.config.recipes[&recipe.id].image_dependencies {
            image_packages.insert(dependency.package.clone());
        }
        if let Some(extra_packages) = extra_packages {
            image_packages.append(&mut BTreeSet::from_iter(extra_packages.into_iter()));
        }

        let mut mounts: Vec<Mount> = Vec::new();

        clean(self.cache.path_dependency_cache_sources()).context("Failed to clean sources depcache")?;
        clean(self.cache.path_dependency_cache_packages()).context("Failed to clean package depcache")?;
        clean(self.cache.path_dependency_cache_tools()).context("Failed to clean tool depcache")?;
        create_dir_all(self.cache.path_dependency_cache_sources()).context("Failed to create sources depcache")?;
        create_dir_all(self.cache.path_dependency_cache_packages()).context("Failed to create package depcache")?;
        create_dir_all(self.cache.path_dependency_cache_tools()).context("Failed to create tool depcache")?;

        let mut installed: Vec<ConfigRecipeId> = Vec::new();
        for dependency in &self.config.dependency_map[&recipe.id] {
            self.install_dependency(&mut mounts, &mut image_packages, &mut installed, dependency)
                .context("Failed to install dependency")?;
        }

        let mut runtime_config = RuntimeConfig::new(self.rootfs.subset(image_packages).context("Failed to get rootfs subset")?);
        for mount in mounts {
            runtime_config.mounts.push(mount);
        }

        for opt in &recipe.used_options {
            runtime_config.environment.insert(format!("OPTION_{}", opt), self.effective_options[opt].clone());
        }

        runtime_config
            .mounts
            .push(Mount::new(self.cache.path_dependency_cache_packages(), "/chariot/sysroot").read_only());

        runtime_config.mounts.push(Mount::new(self.cache.path_dependency_cache_tools(), "/usr/local").read_only());

        for env in &self.config.global_env {
            runtime_config.environment.insert(env.0.clone(), env.1.clone());
        }

        runtime_config.environment.insert(String::from("SOURCES_DIR"), String::from("/chariot/sources"));
        runtime_config.environment.insert(String::from("CUSTOM_DIR"), String::from("/chariot/custom"));
        runtime_config.environment.insert(String::from("SYSROOT_DIR"), String::from("/chariot/sysroot"));

        match recipe.namespace {
            ConfigNamespace::Source(_) => {
                let src_path = self.path_recipe(recipe.id).join("src");

                create_dir_all(&src_path)?;

                runtime_config.cwd = Path::new("/chariot/source").to_path_buf();
                runtime_config.mounts.push(Mount::new(src_path, "/chariot/source"));
            }
            ConfigNamespace::Package(_) | ConfigNamespace::Tool(_) | ConfigNamespace::Custom(_) => {
                let build_path = self.path_recipe(recipe.id).join("build");
                let install_path = self.path_recipe(recipe.id).join("install");

                create_dir_all(&build_path).context("Failed to create build path")?;
                create_dir_all(&install_path).context("Failed to create install path")?;

                runtime_config.cwd = Path::new("/chariot/build").to_path_buf();
                runtime_config.mounts.push(Mount::new(build_path, Path::new("/chariot/build")));
                runtime_config.mounts.push(Mount::new(install_path, Path::new("/chariot/install")));

                runtime_config.environment.insert(String::from("BUILD_DIR"), String::from("/chariot/build"));
                runtime_config.environment.insert(String::from("INSTALL_DIR"), String::from("/chariot/install"));
            }
        }

        Ok(runtime_config)
    }

    fn install_dependency(
        &self,
        mounts: &mut Vec<Mount>,
        image_packages: &mut BTreeSet<String>,
        installed: &mut Vec<ConfigRecipeId>,
        dependency: &ConfigRecipeDependency,
    ) -> Result<()> {
        let recipe = &self.config.recipes[&dependency.recipe_id];
        if !installed.contains(&dependency.recipe_id) {
            installed.push(recipe.id);

            match &recipe.namespace {
                ConfigNamespace::Source(_) => {
                    let src_path = self.path_recipe(recipe.id).join("src");
                    let mount_to = Path::new("/chariot/sources").join(&recipe.name);
                    if dependency.mutable {
                        let sources_depcache_path = self.cache.path_dependency_cache_sources();
                        create_dir_all(&sources_depcache_path.join(&recipe.name)).context("Failed to create sources depcache")?;
                        copy_recursive(src_path, &sources_depcache_path.join(&recipe.name)).with_context(|| format!("Failed to copy source `{}` to depcache", recipe.name))?;
                        mounts.push(Mount::new(&sources_depcache_path.join(&recipe.name), mount_to));
                    } else {
                        mounts.push(Mount::new(src_path, mount_to).read_only());
                    }
                }
                ConfigNamespace::Package(_) => {
                    let package_depcache_path = self.cache.path_dependency_cache_packages();
                    create_dir_all(&package_depcache_path).context("Failed to create package depcache")?;
                    copy_recursive(self.path_recipe(recipe.id).join("install"), &package_depcache_path).context("Failed to copy package to package depcache dir")?;
                }
                ConfigNamespace::Tool(_) => {
                    let tool_depcache_path = self.cache.path_dependency_cache_tools();
                    create_dir_all(&tool_depcache_path).context("Failed to create tool depcache")?;
                    copy_recursive(self.path_recipe(recipe.id).join("install").join("usr").join("local"), &tool_depcache_path)
                        .context("Failed to copy tool to tool depcache dir")?;
                }
                ConfigNamespace::Custom(_) => {
                    mounts.push(Mount::new(self.path_recipe(recipe.id).join("install"), Path::new("/chariot/custom").join(&recipe.name)).read_only())
                }
            }
        }

        for image_dep in &recipe.image_dependencies {
            if !image_dep.runtime {
                continue;
            }

            image_packages.insert(image_dep.package.clone());
        }

        for dependency in &self.config.dependency_map[&dependency.recipe_id] {
            if !dependency.runtime {
                continue;
            }

            self.install_dependency(mounts, image_packages, installed, &dependency)
                .context("Broken dependency install")?;
        }
        Ok(())
    }
}
