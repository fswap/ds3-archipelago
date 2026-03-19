use std::{fs, io, path::PathBuf, marker::PhantomData};

use anyhow::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::{utils, Game};

/// The configuration file for the Archipelago connection.
#[derive(Deserialize, Serialize)]
pub struct Config<G: Game> {
    url: String,
    slot: String,
    seed: String,
    client_version: Option<String>,
    password: Option<String>,
    #[serde(skip)]
    _marker: PhantomData<G>,
}

impl<G: Game> Config<G> {
    /// Loads the config from disk.
    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        match fs::read_to_string(&path) {
            Ok(text) => json::from_str(&text).map_err(|err| {
                Error::from(err).context(format!(
                    "Failed to parse config file {}",
                    path.to_string_lossy()
                ))
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                Err(Error::from(err).context(format!(
                    "{} doesn't exist. Have you run randomizer\\{}?",
                    path.to_string_lossy(),
                    G::TYPE.static_randomizer_basename()
                )))
            }
            Err(err) => Err(Error::from(err).context(format!(
                "Failed to load config file {}",
                path.to_string_lossy()
            ))),
        }
    }

    /// Saves the config file to disk.
    pub fn save(&self) -> Result<()> {
        Ok(fs::write(Self::path()?, json::to_string(self)?)?)
    }

    /// The path to the configuration file.
    fn path() -> Result<PathBuf> {
        Ok(utils::mod_directory()?.join("apconfig.json"))
    }

    /// Returns the Archipelago server URL defined in the config, or None if it
    /// doesn't contain a URL.
    pub fn url(&self) -> &str {
        self.url.as_str()
    }

    /// Sets the Archipelago server URL in the config file.
    pub fn set_url(&mut self, url: impl AsRef<str>) {
        self.url = url.as_ref().to_string()
    }

    /// Returns the slot that the config was created with.
    pub fn slot(&self) -> &str {
        self.slot.as_str()
    }

    /// Returns the seed that the config was created with.
    pub fn seed(&self) -> &str {
        self.seed.as_str()
    }

    /// Returns the version of the static randomizer that the config was created
    /// with, or None if it doesn't contain a version (such as for a local
    /// randomizer build).
    pub fn client_version(&self) -> Option<&str> {
        self.client_version.as_deref()
    }

    /// Returns the password that the config was created with, or None if it
    /// doesn't contain a password.
    pub fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }
}
