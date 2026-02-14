use std::path::PathBuf;

/// Returns the default socket path for the tmax server.
pub fn default_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        PathBuf::from(runtime_dir).join("tmax.sock")
    } else {
        // SAFETY: getuid() is always safe to call and has no preconditions
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/tmax-{uid}.sock"))
    }
}

/// Returns the config/data directory path for tmax.
pub fn dirs_path() -> PathBuf {
    if let Ok(config_dir) = std::env::var("XDG_CONFIG_HOME") {
        PathBuf::from(config_dir).join("tmax")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".config").join("tmax")
    } else {
        PathBuf::from("/tmp/tmax")
    }
}

/// Returns the default PID file path for the tmax server.
pub fn pid_file_path() -> PathBuf {
    dirs_path().join("tmax.pid")
}

/// Returns the config file path for the tmax server.
pub fn config_path() -> PathBuf {
    dirs_path().join("config.toml")
}
