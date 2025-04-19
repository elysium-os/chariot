use std::{
    fs::{create_dir_all, exists, read_to_string, write},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
};

use anyhow::{bail, Context, Result};
use log::{info, warn};
use runtime::RuntimeConfig;
use util::link_recursive;

pub use util::clean;

pub mod runtime;
mod util;

pub struct Container {
    cache_path: PathBuf,
    root_packages: Vec<String>,
}

pub enum ContainerSet {
    Root(Rc<Container>),
    Subset(ContainerSubset),
}

pub struct ContainerSubset {
    container: Rc<Container>,
    superset: Rc<ContainerSet>,
    package: String,
}

impl Container {
    pub fn init(cache_path: impl AsRef<Path>, version: String, root_packages: Vec<String>) -> Result<Rc<Container>> {
        let mut reset = true;
        let rootfs_state_path = cache_path.as_ref().join("rootfs.toml");
        if exists(&rootfs_state_path)? {
            let state_data = read_to_string(&rootfs_state_path).context("Failed to read rootfs state")?;
            let table = state_data.parse::<toml::Table>().context("Failed to parse rootfs state")?;

            let intact = table["intact"].as_bool().unwrap_or(false);
            let current_version = table["version"].as_str().unwrap_or("");
            let current_root_packages = table["root_pkgs"].as_array();

            if intact {
                let packages_match = match current_root_packages {
                    Some(packages) => {
                        let mut ok = true;
                        if packages.len() != root_packages.len() {
                            ok = false;
                        } else {
                            for package in packages {
                                let package = package.as_str().unwrap_or("");

                                if root_packages.contains(&String::from(package)) {
                                    continue;
                                }

                                ok = false;
                                break;
                            }
                        }
                        ok
                    }
                    None => false,
                };

                if !packages_match {
                    warn!("Resetting container cache due to mismatch in root packages");
                }
                if version != current_version {
                    warn!("Resetting container cache due to different rootfs version");
                }

                if version == current_version && packages_match {
                    reset = false;
                }
            }
        }

        if reset {
            info!("Initializing container cache");

            clean(cache_path.as_ref()).context("Failed to clean container cache")?;
            create_dir_all(cache_path.as_ref()).context("Failed to create container cache dir")?;

            info!("Fetching rootfs");
            let archive_path = cache_path.as_ref().join("rootfs.tar.zst");
            let res = Command::new("wget")
                .args([
                    "-O",
                    archive_path.to_str().unwrap(),
                    format!(
                        "https://github.com/mintsuki/debian-rootfs/releases/download/{}/debian-rootfs-amd64.tar.xz",
                        version
                    )
                    .as_str(),
                ])
                .output()
                .context("Failed to wget rootfs archive")?;
            if !res.status.success() {
                bail!(
                    "Failed to wget rootfs archive: {}",
                    String::from_utf8(res.stderr).unwrap_or(String::from("Failed to parse stderr"))
                );
            }

            info!("Extracting rootfs");
            let rootfs_path = cache_path.as_ref().join("rootfs");
            create_dir_all(&rootfs_path).context("Failed to create rootfs dir")?;
            let res = Command::new("bsdtar")
                .args([
                    "--strip-components",
                    "1",
                    "-x",
                    "--zstd",
                    "-C",
                    rootfs_path.to_str().unwrap(),
                    "-f",
                    archive_path.to_str().unwrap(),
                ])
                .output()
                .context("Failed to extract root archive")?;
            if !res.status.success() {
                bail!(
                    "Failed to extract root archive: {}",
                    String::from_utf8(res.stderr).unwrap_or(String::from("Failed to parse stderr"))
                );
            }

            info!("Initializing rootfs");
            let mut runtime_config = RuntimeConfig::default_rootfs(rootfs_path).as_root().rw();
            runtime_config.set_output(Some(cache_path.as_ref().join("container_init.log")), true);

            //runtime_config.run_shell("ln -s /proc/self/fd /dev/fd")?;
            runtime_config.run_shell("echo 'en_US.UTF-8 UTF-8' > /etc/locale.gen")?;
            runtime_config.run_shell(
                "echo '
                APT::Install-Suggests \"0\";
                APT::Install-Recommends \"0\";
                APT::Sandbox::User \"root\";
                Acquire::Check-Valid-Until \"0\";
                ' > /etc/apt/apt.conf",
            )?;

            runtime_config.run_shell("apt-get update")?;
            runtime_config.run_shell("apt-get install -y locales")?;
            runtime_config.run_shell("locale-gen")?;
            runtime_config.run_shell(String::from("apt-get install -y ") + root_packages.join(" ").as_str())?;

            let mut state_table = toml::Table::new();
            state_table.insert(String::from("intact"), toml::Value::Boolean(true));
            state_table.insert(String::from("version"), toml::Value::String(version));
            state_table.insert(
                String::from("root_pkgs"),
                toml::Value::Array(root_packages.iter().map(|v| toml::Value::String(v.clone())).collect()),
            );

            write(
                &rootfs_state_path,
                toml::to_string(&state_table).context("Failed to serialize rootfs state")?,
            )
            .context("Failed to write rootfs state")?;
        }

        info!("Container OK");

        Ok(Rc::new(Container {
            cache_path: cache_path.as_ref().to_path_buf(),
            root_packages,
        }))
    }

    pub fn get_root_set(self: &Rc<Container>) -> ContainerSet {
        ContainerSet::Root(self.clone())
    }

    pub fn get_set(self: &Rc<Container>, packages: &Vec<String>) -> Result<ContainerSet> {
        let mut packages: Vec<String> = packages
            .clone()
            .into_iter()
            .filter(|package| !self.root_packages.contains(package))
            .collect();
        packages.sort();
        packages.dedup();

        let mut set = self.get_root_set();
        for pkg in packages.iter() {
            set = set
                .get_subset(pkg.to_string())
                .with_context(|| format!("Failed to get subset `{}`", pkg))?;
        }

        Ok(set)
    }
}

impl ContainerSet {
    fn path(&self) -> PathBuf {
        match self {
            ContainerSet::Root(container) => container.cache_path.clone(),
            ContainerSet::Subset(subset) => subset.path(),
        }
    }

    pub fn rootfs_path(&self) -> PathBuf {
        match self {
            ContainerSet::Root(_) => self.path().join("rootfs"),
            ContainerSet::Subset(subset) => subset.rootfs_path(),
        }
    }

    fn get_subset(self, package: String) -> Result<ContainerSet> {
        let subset = ContainerSubset {
            container: Rc::clone(match &self {
                ContainerSet::Root(container) => container,
                ContainerSet::Subset(subset) => &subset.container,
            }),
            superset: Rc::new(self),
            package: package.clone(),
        };
        subset.install().context("Failed to install subset")?;
        Ok(ContainerSet::Subset(subset))
    }
}

impl ContainerSubset {
    fn path(&self) -> PathBuf {
        self.superset.path().join("sub").join(&self.package)
    }

    fn rootfs_path(&self) -> PathBuf {
        self.path().join("rootfs")
    }

    fn state_path(&self) -> PathBuf {
        self.path().join("state.toml")
    }

    fn parse_state(&self) -> Result<Option<bool>> {
        let path = self.state_path();
        if !exists(&path)? {
            return Ok(None);
        }

        let data = read_to_string(&path).context("Failed to read subset state")?;
        let table = data.parse::<toml::Table>().context("Failed to parse subset state")?;
        let intact = table["intact"].as_bool().unwrap_or(false);
        Ok(Some(intact))
    }

    fn write_info(&self, intact: bool) -> Result<()> {
        let path = self.state_path();

        let mut info_table = toml::Table::new();
        info_table.insert(String::from("intact"), toml::Value::Boolean(intact));
        write(&path, toml::to_string(&info_table).context("Failed to serialize subset state")?).context("Failed to write subset state")?;
        Ok(())
    }

    fn install(&self) -> Result<()> {
        let src_rootfs_path = self.superset.rootfs_path();

        let dest_rootfs_path = self.rootfs_path();
        let state = self.parse_state()?;

        if !exists(&dest_rootfs_path)? || state.is_none_or(|intact| !intact) {
            clean(&self.path()).context("Failed to clean subset")?;

            create_dir_all(&dest_rootfs_path).context("Failed to create subset dir")?;
            self.write_info(false)?;
            link_recursive(&src_rootfs_path, &dest_rootfs_path).context("Failed to link new rootfs for subset")?;

            RuntimeConfig::default_rootfs(dest_rootfs_path)
                .as_root()
                .rw()
                .quiet()
                .run_shell(format!("apt-get install -y {}", self.package.as_str()).as_str())?;

            self.write_info(true)?;
        }
        Ok(())
    }
}
