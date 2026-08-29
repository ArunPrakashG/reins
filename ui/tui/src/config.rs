use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_animations")]
    pub animations: bool,
}

fn default_animations() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self { animations: true }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

pub fn load() -> Config {
    let Ok(path) = proto::config_file_path() else {
        return Config::default();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Config::default();
    };
    toml::from_str(&content).unwrap_or_default()
}

pub fn save(config: &Config) -> Result<(), ConfigError> {
    let path = proto::config_file_path()?;
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    fn test_mutex() -> &'static Mutex<()> {
        static MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        MUTEX.get_or_init(|| Mutex::new(()))
    }

    fn temp_config_dir() -> PathBuf {
        let thread_id = std::thread::current().id();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir()
            .join(format!("reins-config-test-{:?}-{}", thread_id, timestamp));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp test dir");
        dir
    }

    #[test]
    fn load_nonexistent_file_returns_default() {
        let _guard = test_mutex().lock().unwrap();
        let old_home = std::env::var("HOME").ok();
        let old_xdg = std::env::var("XDG_CONFIG_HOME").ok();

        let temp_home = temp_config_dir();
        std::env::set_var("HOME", &temp_home);
        std::env::remove_var("XDG_CONFIG_HOME");

        let config = load();
        assert_eq!(config.animations, true);

        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        }
        if let Some(x) = old_xdg {
            std::env::set_var("XDG_CONFIG_HOME", x);
        }
        let _ = fs::remove_dir_all(&temp_home);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let _guard = test_mutex().lock().unwrap();
        let old_home = std::env::var("HOME").ok();
        let old_xdg = std::env::var("XDG_CONFIG_HOME").ok();

        let temp_home = temp_config_dir();
        std::env::set_var("HOME", &temp_home);
        std::env::remove_var("XDG_CONFIG_HOME");

        // Ensure config directory exists
        let config_dir = temp_home.join(".config/reins");
        fs::create_dir_all(&config_dir).expect("create config dir");

        let original = Config { animations: false };
        save(&original).expect("save config");

        let loaded = load();
        assert_eq!(loaded.animations, false);

        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        }
        if let Some(x) = old_xdg {
            std::env::set_var("XDG_CONFIG_HOME", x);
        }
        let _ = fs::remove_dir_all(&temp_home);
    }

    #[test]
    fn load_malformed_toml_falls_back_to_default() {
        let _guard = test_mutex().lock().unwrap();
        let old_home = std::env::var("HOME").ok();
        let old_xdg = std::env::var("XDG_CONFIG_HOME").ok();

        let temp_home = temp_config_dir();
        std::env::set_var("HOME", &temp_home);
        std::env::remove_var("XDG_CONFIG_HOME");

        // Create config directory and write malformed TOML
        let config_dir = temp_home.join(".config/reins");
        fs::create_dir_all(&config_dir).expect("create config dir");
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, "invalid toml content [[[").expect("write malformed TOML");

        let config = load();
        assert_eq!(config.animations, true);

        if let Some(h) = old_home {
            std::env::set_var("HOME", h);
        }
        if let Some(x) = old_xdg {
            std::env::set_var("XDG_CONFIG_HOME", x);
        }
        let _ = fs::remove_dir_all(&temp_home);
    }
}
