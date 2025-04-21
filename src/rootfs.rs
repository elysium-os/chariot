use std::{
    collections::BTreeSet,
    fs::{create_dir_all, exists, read_to_string, write},
    path::PathBuf,
    process::Command,
    rc::Rc,
};

use anyhow::{Context, Result, bail};
use log::{info, warn};

use crate::{
    cache::Cache,
    runtime::{OutputConfig, RuntimeConfig},
    util::{clean, link_recursive},
};

pub const DEFAULT_PACKAGES: &'static [&'static str] = &[
    "autopoint",
    "bash",
    "fakeroot",
    "file",
    "doxygen",
    "bzip2",
    "findutils",
    "gawk",
    "bison",
    "curl",
    "diffutils",
    "docbook-xsl",
    "flex",
    "gettext",
    "grep",
    "gzip",
    "xsltproc",
    "libarchive13",
    "libssl3t64",
    "m4",
    "make",
    "patch",
    "perl",
    "python3",
    "sed",
    "tar",
    "texinfo",
    "w3m",
    "which",
    "xmlto",
    "xz-utils",
    "zlib1g",
    "zstd",
    "git",
    "wget",
];

pub const CURRENT_VERSION: &'static str = "20250401T023134Z";

pub struct RootFS {
    cache: Rc<Cache>,
    root_packages: BTreeSet<String>,
}

impl Cache {
    pub fn rootfs_init(self: Rc<Cache>, version: String, root_packages: BTreeSet<String>) -> Result<RootFS> {
        let mut reset = true;
        let state_path = self.path_rootfs().join("state.toml");
        if exists(&state_path)? {
            let state_data = read_to_string(&state_path).context("Failed to read rootfs state")?;
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
                    warn!("Resetting rootfs due to mismatch in root packages");
                }
                if version != current_version {
                    warn!("Resetting rootfs due to different rootfs version");
                }

                if version == current_version && packages_match {
                    reset = false;
                }
            }
        }

        if reset {
            info!("Initializing rootfs");

            clean(self.path_rootfs()).context("Failed to clean rootfs")?;
            create_dir_all(self.path_rootfs()).context("Failed to create rootfs directory")?;

            info!("Fetching rootfs");
            let archive_path = self.path_rootfs().join("rootfs.tar.zst");
            let res = Command::new("wget")
                .args([
                    "-O",
                    archive_path.to_str().unwrap(),
                    format!("https://github.com/mintsuki/debian-rootfs/releases/download/{}/debian-rootfs-amd64.tar.xz", version).as_str(),
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
            let rootfs_path = self.path_rootfs().join("rootfs");
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
            let runtime_config = RuntimeConfig::new(rootfs_path).root_user().rw().set_output_config(crate::runtime::OutputConfig {
                log_path: Some(self.path_rootfs().join("init.log")),
                quiet: true,
            });

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
            runtime_config.run_shell(String::from("apt-get install -y ") + root_packages.iter().cloned().collect::<Vec<String>>().join(" ").as_str())?;

            let mut state_table = toml::Table::new();
            state_table.insert(String::from("intact"), toml::Value::Boolean(true));
            state_table.insert(String::from("version"), toml::Value::String(version));
            state_table.insert(
                String::from("root_pkgs"),
                toml::Value::Array(root_packages.iter().map(|v| toml::Value::String(v.clone())).collect()),
            );

            write(&state_path, toml::to_string(&state_table).context("Failed to serialize rootfs state")?).context("Failed to write rootfs state")?;

            info!("Rootfs OK");
        }

        Ok(RootFS { cache: self, root_packages })
    }
}

impl RootFS {
    pub fn subset(&self, packages: BTreeSet<String>) -> Result<PathBuf> {
        let packages = packages.difference(&self.root_packages);

        let mut current_path = self.cache.path_rootfs();
        for pkg in packages {
            let next_path = current_path.join("subset").join(pkg);

            let src_rootfs_path = current_path.join("rootfs");
            let dest_rootfs_path = next_path.join("rootfs");
            let state_path = next_path.join("state.toml");

            let mut intact = false;
            if exists(&state_path)? {
                let data = read_to_string(&state_path).context("Failed to read subset state")?;
                let table = data.parse::<toml::Table>().context("Failed to parse subset state")?;
                intact = table["intact"].as_bool().unwrap_or(false);
            }

            if !exists(&dest_rootfs_path)? || !intact {
                clean(&dest_rootfs_path).context("Failed to clean subset")?;
                create_dir_all(&dest_rootfs_path).context("Failed to create subset dir")?;

                let mut info_table = toml::Table::new();
                info_table.insert(String::from("intact"), toml::Value::Boolean(false));
                write(&state_path, toml::to_string(&info_table).context("Failed to serialize subset state")?).context("Failed to write subset state")?;

                link_recursive(&src_rootfs_path, &dest_rootfs_path).context("Failed to link new rootfs for subset")?;

                RuntimeConfig::new(dest_rootfs_path)
                    .root_user()
                    .rw()
                    .set_output_config(OutputConfig { quiet: true, log_path: None })
                    .run_shell(format!("apt-get install -y {}", pkg))
                    .context("Failed to run install cmd")?;

                info_table.clear();
                info_table.insert(String::from("intact"), toml::Value::Boolean(true));
                write(&state_path, toml::to_string(&info_table).context("Failed to serialize subset state")?).context("Failed to write subset state")?;
            }

            current_path = next_path;
        }

        Ok(current_path.join("rootfs"))
    }
}
