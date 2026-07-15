use std::{
    collections::{BTreeSet, HashMap},
    io::{self},
    path::PathBuf,
    process::exit,
};

use anyhow::{Context, bail};
use chariot_rootfs::{CachedPkgSet, DEFAULT_MANIFESTS_URL, GetPkgSetError, ManifestFetchSpec, PkgSetState, RootFS};
use clap::{Parser, Subcommand};
use log::info;

#[derive(Parser)]
#[command(name = "rootfs", about = "Manage a chariot rootfs")]
struct Cli {
    #[command(subcommand)]
    command: MainCommand,

    #[arg(long, default_value = ".chariot-rootfs")]
    path: PathBuf,
}

#[derive(Subcommand)]
enum MainCommand {
    Init {
        #[arg(long, default_value = DEFAULT_MANIFESTS_URL)]
        url: String,
        version: String,
        hash: String,
    },
    Status,
    #[command(subcommand)]
    Pkgset(PkgsetCommand),
    Exec {
        #[arg(long = "pkg", value_delimiter = ',')]
        packages: Vec<String>,

        #[arg(long, default_value = "/")]
        cwd: PathBuf,

        #[arg(short, long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,

        #[arg(trailing_var_arg = true, required = true)]
        args: Vec<String>,
    },
    Prune,
}

#[derive(Subcommand)]
enum PkgsetCommand {
    List,
    Cache { packages: Vec<String> },
}

pub fn run_cli() -> Result<(), anyhow::Error> {
    let opts = Cli::parse();

    let mut logger = io::stderr();

    let path = opts.path;

    if let MainCommand::Init { url, version, hash } = opts.command {
        let spec = ManifestFetchSpec { url, version, hash };
        RootFS::init(&path, &spec, &mut logger)?;
        info!("rootfs initialized at {}", path.display());
        return Ok(());
    }

    let rootfs = match RootFS::get(&path)? {
        Some(r) => r,
        None => bail!("no intact rootfs found at {}", path.display()),
    };

    match opts.command {
        MainCommand::Init { .. } => {}
        MainCommand::Status => {
            let spec = rootfs.handle.get_manifest_spec();
            info!("path:    {}", path.display());
            info!("url:     {}", spec.url);
            info!("version: {}", spec.version);
            info!("hash:    {}", spec.hash);
        }
        MainCommand::Pkgset(PkgsetCommand::List) => {
            let pkgsets = rootfs.list_pkgsets()?;
            if pkgsets.is_empty() {
                info!("no cached package sets");
                return Ok(());
            }
            info!("{:<6} {:<12} {:<6} {:<6} {:<12} packages", "id", "state", "base", "depth", "size");
            info!("{}", "-".repeat(60));
            for ps in pkgsets {
                let state = match ps.state {
                    PkgSetState::Unknown => "unknown",
                    PkgSetState::Cached => "cached",
                    PkgSetState::Deduplicated => "deduped",
                };
                info!(
                    "{:<6} {:<12} {:<6} {:<6} {:<12} {}",
                    ps.id,
                    state,
                    ps.base.map(|id| id.to_string()).unwrap_or(String::new()),
                    ps.base_depth,
                    format_size(ps.size),
                    ps.packages.iter().map(|str| str.as_ref()).collect::<Vec<_>>().join(", "),
                );
            }
        }
        MainCommand::Pkgset(PkgsetCommand::Cache { packages }) => {
            CachedPkgSet::get(&rootfs, None, &BTreeSet::from_iter(packages.iter().map(|s| s.as_str())), &mut logger)
                .context("failed to get cached package set")?;
        }
        MainCommand::Exec { packages, cwd, env, args } => {
            let environment: HashMap<String, String> = env
                .into_iter()
                .filter_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    Some((k.to_string(), v.to_string()))
                })
                .collect();

            let pkgset = if packages.is_empty() {
                None
            } else {
                let set: BTreeSet<&str> = packages.iter().map(|s| s.as_str()).collect();
                match CachedPkgSet::get(&rootfs, None, &set, &mut logger) {
                    Ok(ps) => ps,
                    Err(GetPkgSetError::DownloadPackageError { name }) => bail!("failed to download package: {name}"),
                    Err(GetPkgSetError::InstallPackageError { name }) => bail!("failed to install package: {name}"),
                    Err(err) => return Err(err.into()),
                }
            };

            let exit_code = rootfs.handle.exec(
                &cwd,
                &vec![],
                &environment,
                &mut logger,
                args.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                pkgset.as_deref(),
            )?;

            exit(exit_code);
        }
        MainCommand::Prune => {
            let (total, removed, in_use) = rootfs.prune_pkgsets(|_| true)?;
            info!("pruned {removed}/{total} package sets ({in_use} in use, skipped)");
        }
    }

    Ok(())
}

fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = UNITS[0];
    for &u in &UNITS[1..] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = u;
    }
    if unit == "B" { format!("{bytes}B") } else { format!("{value:.1}{unit}") }
}
