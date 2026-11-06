//! The settings, kept in `~/.config/erisautocaptions/config.toml`.

use std::path::PathBuf;
use std::{fs, io};

use serde::{Deserialize, Serialize};

use crate::models::Tier;

/// Where the captions listen. A window is its app's audio, since macOS mixes sound per process.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind")]
pub enum Source {
    /// Everything the Mac plays.
    #[default]
    System,
    /// An input device, by its Core Audio UID.
    Device { uid: String, name: String },
    /// An app, with its helper processes, by bundle identifier.
    App { bundle: String, name: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    pub model: Tier,
    pub language: String,
    /// Where the overlay's panel goes, from the bottom left of the screens; left out, it's placed
    /// low in the middle of the main screen.
    pub position: Option<(f64, f64)>,
    pub source: Source,
    /// The body's size, as left by dragging its handle.
    pub width: f64,
    pub lines: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            model: Tier::default(),
            language: "en".to_owned(),
            position: None,
            source: Source::default(),
            width: 680.0,
            lines: 2,
        }
    }
}

fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("erisautocaptions").join("config.toml")
}

/// Reads the settings; a missing or broken file gives the defaults, with a word about the latter.
pub fn load() -> Config {
    let path = path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!("ErisAutoCaptions: {}: {error}", path.display());
            }
            return Config::default();
        }
    };
    toml::from_str(&text).unwrap_or_else(|error| {
        eprintln!("ErisAutoCaptions: {}: {error}", path.display());
        Config::default()
    })
}

pub fn save(config: &Config) {
    let path = path();
    let result = toml::to_string(config).map_err(|error| error.to_string()).and_then(|text| {
        fs::create_dir_all(path.parent().unwrap()).map_err(|error| error.to_string())?;
        fs::write(&path, text).map_err(|error| error.to_string())
    });
    if let Err(error) = result {
        eprintln!("ErisAutoCaptions: {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_source_survives_the_file() {
        for source in [
            Source::System,
            Source::Device { uid: "BuiltInMicrophoneDevice".to_owned(), name: "MacBook Pro Microphone".to_owned() },
            Source::App { bundle: "com.google.Chrome".to_owned(), name: "Google Chrome".to_owned() },
        ] {
            let config = Config { position: Some((12.0, 34.0)), source, ..Config::default() };
            assert_eq!(toml::from_str::<Config>(&toml::to_string(&config).unwrap()).unwrap(), config);
        }
    }

    #[test]
    fn missing_settings_take_their_defaults() {
        let config: Config = toml::from_str("language = \"es\"").unwrap();
        assert_eq!(config, Config { language: "es".to_owned(), ..Config::default() });
    }
}
