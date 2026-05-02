use std::{
    collections::HashSet,
    fmt::{self, Display, Formatter},
    io::{self, Read},
    str::FromStr,
    time::Duration,
};

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

pub const ROOTFS_MANIFEST_VERSION: i64 = 1;

const MANIFEST_URL_VERSION_PLACEHOLDER: &str = "@VERSION@";
const MANIFEST_KEY_VERSION: &str = "manifest_version";

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct ManifestFetchSpec {
    pub url: String,
    pub version: String,
    pub hash: String,
}

#[derive(Deserialize)]
pub struct ManifestCommands {
    pub setup: String,
    pub pkg_download: String,
    pub pkg_install: String,
}

#[derive(Deserialize)]
pub struct ManifestRootFS {
    pub url: String,
    pub hash: String,
}

#[derive(Deserialize)]
pub struct ManifestIDs {
    pub root_uid: u32,
    pub root_gid: u32,
    pub user_uid: u32,
    pub user_gid: u32,
}

#[derive(Deserialize)]
pub struct ManifestPackages {
    pub root: HashSet<String>,
    pub bsdtar: String,
    pub git: String,
    pub patch: String,
}

#[derive(Deserialize)]
pub struct Manifest {
    pub rootfs: ManifestRootFS,
    pub commands: ManifestCommands,
    pub ids: ManifestIDs,
    pub packages: ManifestPackages,
}

#[derive(Error, Debug)]
pub enum ManifestFetchError {
    #[error(transparent)]
    Request(#[from] reqwest::Error),

    #[error("Manifest URL `{}` is invalid", url)]
    InvalidManifestURL { url: String, source: url::ParseError },

    #[error("Manifest URL `{}` is invalid (no version placeholder)", url)]
    InvalidManifestURLMissingVersion { url: String },

    #[error("Manifest is missing or has an invalid version")]
    InvalidManifestVersion,

    #[error("Malformed rootfs manifest")]
    MalformedManifest(#[source] toml::de::Error),

    #[error("Manifest version does not match current supported version, expected `{}`, got `{}`", expected, found)]
    ManifestVersionMismatch { expected: i64, found: i64 },

    #[error("Manifest hash does not match, expected `{}`, got `{}`", expected, found)]
    HashMismatch { expected: String, found: String },

    #[error("Malformed rootfs manifest data")]
    MalformedData(#[source] io::Error),
}

impl Display for ManifestFetchSpec {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Manifest {{ origin: `{}`, version: `{}`, hash: `{}` }}",
            self.url, self.version, self.hash
        )
    }
}

impl Manifest {
    pub fn fetch(spec: &ManifestFetchSpec) -> Result<Manifest, ManifestFetchError> {
        if !spec.url.contains(MANIFEST_URL_VERSION_PLACEHOLDER) {
            return Err(ManifestFetchError::InvalidManifestURLMissingVersion { url: spec.url.clone() });
        }
        let url = Url::from_str(&spec.url.replace(MANIFEST_URL_VERSION_PLACEHOLDER, &spec.version)).map_err(|err| {
            ManifestFetchError::InvalidManifestURL {
                url: spec.url.clone(),
                source: err,
            }
        })?;

        let client = Client::builder().connect_timeout(Duration::from_secs(10)).build()?;
        let manifest_data = {
            let mut str = String::new();
            client
                .get(url)
                .send()?
                .error_for_status()?
                .read_to_string(&mut str)
                .map_err(|err| ManifestFetchError::MalformedData(err))?;
            str
        };

        let manifest_table = manifest_data
            .parse::<toml::Table>()
            .map_err(|err| ManifestFetchError::MalformedManifest(err))?;

        let manifest_version = match manifest_table.get(MANIFEST_KEY_VERSION).and_then(|err| err.as_integer()) {
            None => return Err(ManifestFetchError::InvalidManifestVersion),
            Some(version) => version,
        };

        if ROOTFS_MANIFEST_VERSION != manifest_version {
            return Err(ManifestFetchError::ManifestVersionMismatch {
                expected: ROOTFS_MANIFEST_VERSION,
                found: manifest_version,
            });
        }

        let found_hash = {
            let mut hasher = Sha256::new();
            hasher.update(&manifest_data);
            hex::encode(hasher.finalize())
        };

        if spec.hash != found_hash {
            return Err(ManifestFetchError::HashMismatch {
                expected: spec.hash.clone(),
                found: found_hash,
            });
        }

        Ok(toml::from_str::<Manifest>(&manifest_data).map_err(|err| ManifestFetchError::MalformedManifest(err))?)
    }
}
