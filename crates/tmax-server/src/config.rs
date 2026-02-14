use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Debug, Clone)]
pub struct ServerConfig {
    pub socket_path: PathBuf,
}

impl ServerConfig {
    pub fn load() -> anyhow::Result<Self> {
        // Try to load from config file, fall back to defaults
        let config_path = Self::config_path();
        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)?;
            Ok(toml::from_str(&contents)?)
        } else {
            Ok(Self::default())
        }
    }

    pub fn config_path() -> PathBuf {
        tmax_protocol::paths::config_path()
    }

    pub fn default_socket_path() -> PathBuf {
        tmax_protocol::paths::default_socket_path()
    }

    pub fn pid_file_path() -> PathBuf {
        tmax_protocol::paths::pid_file_path()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            socket_path: Self::default_socket_path(),
        }
    }
}
