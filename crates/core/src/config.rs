//! The fast-aws config file, `~/.config/fast-aws/config.toml` (or under
//! `$XDG_CONFIG_HOME`). Every setting is optional; a missing file means defaults.
//!
//! ```toml
//! read_only = true             # every profile
//! metrics = false              # no sparklines (GetMetricData is billed)
//!
//! [profiles.generex-qa-ro]
//! read_only = true
//!
//! [profiles.prod-admin]
//! prod = true                  # red header; read-only unless unlocked
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::resource::Safety;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Read-only for every profile.
    #[serde(default)]
    pub read_only: bool,
    /// Sparklines from CloudWatch (`GetMetricData`, billed per metric).
    /// On unless set to `false`.
    #[serde(default)]
    pub metrics: Option<bool>,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfileConfig {
    #[serde(default)]
    pub read_only: bool,
    /// Marks a production profile: shown in red, and read-only unless unlocked.
    #[serde(default)]
    pub prod: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("couldn't read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl Config {
    /// The config file's location, if a home or config directory is known.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("fast-aws").join("config.toml"))
    }

    /// Loads the config file; a missing file is the default config.
    pub fn load() -> Result<Self, ConfigError> {
        match Self::path() {
            Some(path) => Self::load_from(&path),
            None => Ok(Self::default()),
        }
    }

    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    pub fn profile(&self, name: &str) -> ProfileConfig {
        self.profiles.get(name).cloned().unwrap_or_default()
    }
}

/// Whether actions that change things are allowed, for one profile in one
/// session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guard {
    /// No mutating or destructive actions.
    pub read_only: bool,
    /// A production profile (red header); `side-effect` actions need `:unlock`.
    pub prod: bool,
    /// `:unlock` was typed for this profile in this session.
    pub unlocked: bool,
}

impl Guard {
    /// The guard for `profile`: read-only if the command line, the config file or
    /// the profile says so. A prod profile is read-only until unlocked.
    pub fn new(config: &Config, profile: &str, read_only_flag: bool) -> Self {
        let p = config.profile(profile);
        Self {
            read_only: read_only_flag || config.read_only || p.read_only,
            prod: p.prod,
            unlocked: false,
        }
    }

    /// Whether an action of class `safety` may be offered right now.
    pub fn allows(&self, safety: Safety) -> bool {
        match safety {
            Safety::Safe => true,
            Safety::SideEffect => !self.prod || self.unlocked,
            Safety::Mutating | Safety::Destructive => {
                !self.read_only && (!self.prod || self.unlocked)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn missing_file_is_the_default() {
        let config = Config::load_from(Path::new("/nonexistent/fast-aws/config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("readonly = true").is_err());
    }

    #[test]
    fn read_only_comes_from_flag_file_or_profile() {
        let config = parse("[profiles.qa-ro]\nread_only = true\n[profiles.prod]\nprod = true\n");
        assert!(Guard::new(&config, "qa-ro", false).read_only);
        assert!(!Guard::new(&config, "dev", false).read_only);
        assert!(Guard::new(&config, "dev", true).read_only);
        assert!(Guard::new(&parse("read_only = true"), "dev", false).read_only);
    }

    #[test]
    fn what_each_guard_allows() {
        let config = parse("[profiles.qa-ro]\nread_only = true\n[profiles.prod]\nprod = true\n");
        let ro = Guard::new(&config, "qa-ro", false);
        assert!(ro.allows(Safety::Safe));
        assert!(ro.allows(Safety::SideEffect));
        assert!(!ro.allows(Safety::Mutating));
        assert!(!ro.allows(Safety::Destructive));

        let mut prod = Guard::new(&config, "prod", false);
        assert!(!prod.allows(Safety::SideEffect));
        assert!(!prod.allows(Safety::Mutating));
        prod.unlocked = true;
        assert!(prod.allows(Safety::SideEffect));
        assert!(prod.allows(Safety::Mutating));

        let dev = Guard::new(&config, "dev", false);
        assert!(dev.allows(Safety::Destructive));
    }
}
