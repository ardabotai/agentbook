use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Debug, Clone)]
pub struct ServerConfig {
    pub socket_path: PathBuf,
    #[serde(default = "default_buffer_size")]
    #[allow(dead_code)]
    pub default_buffer_size: usize,
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
        dirs_path().join("config.toml")
    }

    pub fn default_socket_path() -> PathBuf {
        if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
            PathBuf::from(runtime_dir).join("tmax.sock")
        } else {
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/tmp/tmax-{uid}.sock"))
        }
    }

    pub fn pid_file_path() -> PathBuf {
        dirs_path().join("tmax.pid")
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            socket_path: Self::default_socket_path(),
            default_buffer_size: 10_000,
        }
    }
}

fn dirs_path() -> PathBuf {
    if let Ok(config_dir) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(config_dir).join("tmax")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config").join("tmax")
    } else {
        PathBuf::from("/tmp/tmax")
    }
}

fn default_buffer_size() -> usize {
    10_000
}
