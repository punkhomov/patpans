use anyhow::{Result, bail};

use crate::config::{Config, FileConfig};
use crate::engine::Group;
use crate::keys::Key;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub toggle: Option<Key>,
    pub sticky: bool,
    pub tray: bool,
    pub groups: Vec<[Key; 2]>,
}

impl Settings {
    pub fn from_config(config: &Config) -> Self {
        Self {
            toggle: config.toggle,
            sticky: config.sticky,
            tray: config.tray,
            groups: config.groups.iter().map(|group| group.keys).collect(),
        }
    }

    pub fn to_config(&self) -> Config {
        Config {
            toggle: self.toggle,
            sticky: self.sticky,
            tray: self.tray,
            groups: self
                .groups
                .iter()
                .map(|keys| Group::new(keys[0], keys[1]))
                .collect(),
        }
    }

    pub fn to_file(&self) -> FileConfig {
        FileConfig {
            toggle: self
                .toggle
                .map_or_else(|| "none".to_string(), |key| key.name.to_string()),
            sticky: self.sticky,
            tray: self.tray,
            groups: self
                .groups
                .iter()
                .map(|keys| vec![keys[0].name.to_string(), keys[1].name.to_string()])
                .collect(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        for (index, pair) in self.groups.iter().enumerate() {
            if pair[0] == pair[1] {
                bail!("group #{} contains `{}` twice", index + 1, pair[0]);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_file_config() {
        let settings = Settings::from_config(&Config::default());
        let file = settings.to_file();
        let restored = Settings::from_config(&file.into_config().unwrap());
        assert_eq!(settings, restored);
    }

    #[test]
    fn rejects_a_pair_with_the_same_key() {
        let key = crate::keys::by_name("A").unwrap();
        let settings = Settings {
            toggle: None,
            sticky: true,
            tray: true,
            groups: vec![[key, key]],
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn accepts_the_default_settings() {
        assert!(Settings::from_config(&Config::default()).validate().is_ok());
    }
}
