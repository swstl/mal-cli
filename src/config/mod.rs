pub mod logging;
pub mod navigation;
pub mod network;
pub mod player;
pub mod theme;

use logging::Logging;
use navigation::Navigation;
use network::Network;
use player::Player;
use theme::Theme;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

static CONFIG: OnceLock<Config> = OnceLock::new();
const CONFIG_FILE: &str = "config.toml";

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Config {
    #[serde(default = "Navigation::default")]
    pub navigation: Navigation,

    #[serde(default = "Network::default")]
    pub network: Network,

    #[serde(default = "Player::default")]
    pub player: Player,

    #[serde(default = "Theme::default")]
    pub theme: Theme,

    #[serde(default = "Logging::default")]
    pub logging: Logging,

    pub allow_nsfw: Option<bool>,

    /// Show titles in romaji (MAL's main title) instead of English.
    pub prefer_romaji: Option<bool>,

    pub dub: Option<bool>,
}

impl Config {
    // initialize the global config done before the app even runs 
    pub fn init() -> &'static Config {
        CONFIG.get_or_init(Self::read_from_file)
    }

    // get the global config
    pub fn global() -> &'static Config {
        CONFIG.get().expect("Config not initialized - call Config::init() first")
    }

    // get the global config without panicking if it hasn't been initialized yet
    pub fn try_global() -> Option<&'static Config> {
        CONFIG.get()
    }

    pub fn default() -> Self {
        Self {
            navigation: Navigation::default(),
            network: Network::default(),
            player: Player::default(),
            theme: Theme::default(),
            logging: Logging::default(),
            allow_nsfw: Some(true),
            prefer_romaji: Some(false),
            dub: Some(false),
        }
    }

    /// Whether titles should be shown in romaji instead of English.
    pub fn prefer_romaji(&self) -> bool {
        self.prefer_romaji.unwrap_or(false)
    }


    // where configs are stored (default)
    pub fn config_dir() -> PathBuf {
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".config/mal-tui"))
            .expect("Failed to get app directory")
    }


    // where logging and such is saved (episodes watched)
    pub fn data_dir() -> PathBuf {
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".local/share/mal-tui"))
            .expect("Failed to get app directory")
    }

    pub fn migrate_from_mal_cli() {
        let old = std::env::var("HOME")
            .map(|home| PathBuf::from(home).join(".local/share/mal-cli"))
            .unwrap_or_default();
        let new = Self::data_dir();

        if old.exists() && !new.exists() {
            let _ = std::fs::create_dir_all(&new);
            for entry in [".mal", "watch_history"] {
                let src = old.join(entry);
                if src.exists() {
                    let dst = new.join(entry);
                    if src.is_dir() {
                        let _ = copy_dir(&src, &dst);
                    } else {
                        let _ = std::fs::copy(&src, &dst);
                    }
                }
            }
        }
    }


    // used to update the config file with new configs
    pub fn save_to_file(config: &Config) {
        let config_path = Self::config_dir();
        if !config_path.exists() {
            std::fs::create_dir_all(&config_path).unwrap_or_else(|_| {
                eprintln!("Failed to create config directory");
            });
        }
        let config_file_path = config_path.join(CONFIG_FILE);
        let toml = toml::to_string(&config)
            .map_err(|e| {
                eprintln!("Failed to serialize config: {}", e);
            })
            .unwrap_or_default();

        std::fs::write(&config_file_path, toml).unwrap_or_else(|_| {
            eprintln!("Failed to write config file");
        });
    }


    // creates default configs if no file exists already
    fn create_if_not_exists() {
        let config_path = Self::config_dir().join(CONFIG_FILE);
        if !config_path.exists() {
            Self::save_to_file(&Config::default());
        }
    }


    // edit the configs
    pub fn open_in_editor() {
        Self::create_if_not_exists();

        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or("nano".to_string());

        let config_path = Self::config_dir().join(CONFIG_FILE);

        Command::new(editor)
            .arg(config_path)
            .status()
            .map_err(|e| {
                eprintln!(
                    "Failed to open editor: {} try edit manually: ~/.config/mal-tui/config.toml",
                    e
                );
            })
            .ok();
    }


    // read the configs
    pub fn read_from_file() -> Config {
        // in case file generation fails use default
        let config_path = Self::config_dir().join(CONFIG_FILE);
        if !config_path.exists() {
            return Config::default();
        }

        // println!("Loading config from: {:?}", config_path);

        let contents = std::fs::read_to_string(&config_path).unwrap_or_else(|_| {
            eprintln!("Failed to read config file, using default");
            String::new()
        });

        let config: Config = toml::from_str(&contents).unwrap_or_else(|_| {
            eprintln!("Failed to parse config file, using default");
            Config::default()
        });

        config
    }
}

fn copy_dir(src: &PathBuf, dst: &PathBuf) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), dst_path)?;
        }
    }
    Ok(())
}
