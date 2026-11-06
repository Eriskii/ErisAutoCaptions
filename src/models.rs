//! The models on offer, and fetching them into Application Support the first time they're used.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::{fs, thread};

use serde::{Deserialize, Serialize};

use crate::moonshine::{self, ARCH_MEDIUM_STREAMING, ARCH_SMALL_STREAMING, ARCH_TINY_STREAMING};

/// The two sizes picked from the menu. A language without a model of the big size gets the largest
/// it has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Small,
    #[default]
    Big,
}

pub struct Language {
    pub code: &'static str,
    pub name: &'static str,
    /// The largest streaming model published for it.
    largest: u32,
}

pub const LANGUAGES: [Language; 8] = [
    Language { code: "en", name: "English", largest: ARCH_MEDIUM_STREAMING },
    Language { code: "ar", name: "Arabic", largest: ARCH_TINY_STREAMING },
    Language { code: "zh", name: "Chinese", largest: ARCH_TINY_STREAMING },
    Language { code: "de", name: "German", largest: ARCH_SMALL_STREAMING },
    Language { code: "ja", name: "Japanese", largest: ARCH_SMALL_STREAMING },
    Language { code: "es", name: "Spanish", largest: ARCH_SMALL_STREAMING },
    Language { code: "tl", name: "Tagalog", largest: ARCH_TINY_STREAMING },
    Language { code: "vi", name: "Vietnamese", largest: ARCH_TINY_STREAMING },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model {
    pub language: &'static str,
    pub arch: u32,
}

impl Model {
    /// Unknown languages get English, so a hand-edited settings file still does something.
    pub fn choose(tier: Tier, language: &str) -> Self {
        let language = LANGUAGES.iter().find(|known| known.code == language).unwrap_or(&LANGUAGES[0]);
        let arch = match tier {
            Tier::Small => ARCH_TINY_STREAMING,
            Tier::Big => language.largest,
        };
        Self { language: language.code, arch }
    }

    fn size_name(self) -> &'static str {
        match self.arch {
            ARCH_MEDIUM_STREAMING => "medium",
            ARCH_SMALL_STREAMING => "small",
            _ => "tiny",
        }
    }

    fn directory(self) -> PathBuf {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        home.join("Library/Application Support/ErisAutoCaptions/models").join(format!(
            "{}-{}",
            self.size_name(),
            self.language
        ))
    }

    /// The model's directory, downloading whatever isn't there. `progress` hears fractions of the
    /// download; a model already here doesn't call it.
    pub fn fetch(self, mut progress: impl FnMut(f64)) -> Result<PathBuf, String> {
        let files = files(&moonshine::manifest(self.language, self.arch)?)?;
        let directory = self.directory();
        let total: u64 = files.iter().map(|file| file.size).sum();
        let mut done = 0;
        for file in &files {
            let path = directory.join(&file.name);
            if !fs::metadata(&path).is_ok_and(|metadata| metadata.len() == file.size) {
                let parent = path.parent().unwrap_or(&directory);
                fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
                let partial = path.with_extension("part");
                let mut curl = Command::new("curl")
                    .args(["-fsSL", "--retry", "3", "-o"])
                    .arg(&partial)
                    .arg(&file.url)
                    .spawn()
                    .map_err(|error| format!("curl: {error}"))?;
                let status = loop {
                    if let Some(status) = curl.try_wait().map_err(|error| format!("curl: {error}"))? {
                        break status;
                    }
                    let fetched = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
                    progress((done + fetched) as f64 / total.max(1) as f64);
                    thread::sleep(Duration::from_millis(200));
                };
                let size = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
                if !status.success() || size != file.size {
                    let _ = fs::remove_file(&partial);
                    return Err(format!("Couldn't download {}", file.name));
                }
                fs::rename(&partial, &path).map_err(|error| format!("{}: {error}", path.display()))?;
            }
            done += file.size;
        }
        Ok(directory)
    }
}

#[derive(Debug, Deserialize, PartialEq)]
struct File {
    name: String,
    url: String,
    size: u64,
}

fn files(manifest: &str) -> Result<Vec<File>, String> {
    #[derive(Deserialize)]
    struct Manifest {
        groups: Vec<Group>,
    }
    #[derive(Deserialize)]
    struct Group {
        files: Vec<File>,
    }
    let manifest: Manifest = serde_json::from_str(manifest).map_err(|error| error.to_string())?;
    Ok(manifest.groups.into_iter().flat_map(|group| group.files).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tiers_pick_the_small_and_the_largest_models() {
        assert_eq!(Model::choose(Tier::Small, "en").arch, ARCH_TINY_STREAMING);
        assert_eq!(Model::choose(Tier::Big, "en").arch, ARCH_MEDIUM_STREAMING);
        assert_eq!(Model::choose(Tier::Big, "es").arch, ARCH_SMALL_STREAMING);
        assert_eq!(Model::choose(Tier::Big, "vi").arch, ARCH_TINY_STREAMING);
    }

    #[test]
    fn an_unknown_language_falls_back_to_english() {
        assert_eq!(Model::choose(Tier::Big, "xx"), Model { language: "en", arch: ARCH_MEDIUM_STREAMING });
    }

    #[test]
    fn every_offered_model_is_published_and_the_tiers_fit_their_budgets() {
        const MB: u64 = 1_000_000;
        for language in &LANGUAGES {
            for tier in [Tier::Small, Tier::Big] {
                let model = Model::choose(tier, language.code);
                let files = files(&moonshine::manifest(model.language, model.arch).unwrap()).unwrap();
                let size: u64 = files.iter().map(|file| file.size).sum();
                let limit = if tier == Tier::Small { 100 * MB } else { 1000 * MB };
                assert!(size > 0 && size < limit, "{model:?}: {size}");
            }
        }
    }
}
