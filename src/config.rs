use std::fs;
use std::io;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::engine::Group;
use crate::keys::{self, Key};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default = "default_toggle")]
    pub toggle: String,
    #[serde(default = "default_sticky")]
    pub sticky: bool,
    #[serde(default = "default_groups")]
    pub groups: Vec<Vec<String>>,
}

fn default_toggle() -> String {
    "F8".to_string()
}

const fn default_sticky() -> bool {
    true
}

fn default_groups() -> Vec<Vec<String>> {
    vec![
        vec!["A".to_string(), "D".to_string()],
        vec!["W".to_string(), "S".to_string()],
    ]
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            toggle: default_toggle(),
            sticky: default_sticky(),
            groups: default_groups(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub toggle: Option<Key>,
    pub sticky: bool,
    pub groups: Vec<Group>,
}

impl FileConfig {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("failed to parse config `{}`", path.display())),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => {
                Err(err).with_context(|| format!("failed to read config `{}`", path.display()))
            }
        }
    }

    pub fn into_config(self) -> Result<Config> {
        let toggle = parse_toggle(&self.toggle)?;
        let mut groups = Vec::with_capacity(self.groups.len());
        for (index, pair) in self.groups.iter().enumerate() {
            if pair.len() != 2 {
                bail!(
                    "group #{} must contain exactly two keys, got {}",
                    index + 1,
                    pair.len()
                );
            }
            let first = keys::by_name(&pair[0])
                .with_context(|| format!("group #{}: unknown key `{}`", index + 1, pair[0]))?;
            let second = keys::by_name(&pair[1])
                .with_context(|| format!("group #{}: unknown key `{}`", index + 1, pair[1]))?;
            if first == second {
                bail!("group #{} contains `{first}` twice", index + 1);
            }
            groups.push(Group::new(first, second));
        }
        Ok(Config {
            toggle,
            sticky: self.sticky,
            groups,
        })
    }
}

pub fn parse_toggle(name: &str) -> Result<Option<Key>> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    Ok(Some(keys::by_name(trimmed).with_context(|| {
        format!("unknown toggle key `{trimmed}`")
    })?))
}

pub fn parse_groups(spec: &str) -> Result<Vec<Group>> {
    let mut groups = Vec::new();
    for (index, chunk) in spec
        .split(';')
        .map(str::trim)
        .filter(|chunk| !chunk.is_empty())
        .enumerate()
    {
        let names: Vec<&str> = chunk
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        if names.len() != 2 {
            bail!(
                "group #{} (`{chunk}`) must be two comma-separated keys",
                index + 1
            );
        }
        let first = keys::by_name(names[0])
            .with_context(|| format!("group #{}: unknown key `{}`", index + 1, names[0]))?;
        let second = keys::by_name(names[1])
            .with_context(|| format!("group #{}: unknown key `{}`", index + 1, names[1]))?;
        if first == second {
            bail!("group #{} contains `{first}` twice", index + 1);
        }
        groups.push(Group::new(first, second));
    }
    if groups.is_empty() {
        bail!("no groups in `{spec}`");
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config_with_groups_and_overrides() {
        let parsed: FileConfig = toml::from_str(
            r#"
            toggle = "F9"
            sticky = false
            groups = [["A", "D"], ["Up", "Down"]]
            "#,
        )
        .unwrap();
        let config = parsed.into_config().unwrap();
        assert_eq!(config.toggle.unwrap().name, "F9");
        assert!(!config.sticky);
        assert_eq!(config.groups.len(), 2);
        assert_eq!(config.groups[1].keys[1].name, "Down");
    }

    #[test]
    fn defaults_are_ad_ws_f8_sticky() {
        let config = FileConfig::default().into_config().unwrap();
        assert_eq!(config.toggle.unwrap().name, "F8");
        assert!(config.sticky);
        assert_eq!(config.groups[0].keys[0].name, "A");
        assert_eq!(config.groups[1].keys[1].name, "S");
    }

    #[test]
    fn rejects_unknown_keys_and_fields() {
        let parsed: FileConfig = toml::from_str(r#"groups = [["A", "Zzz"]]"#).unwrap();
        assert!(parsed.into_config().is_err());
        assert!(toml::from_str::<FileConfig>("wat = 1").is_err());
    }

    #[test]
    fn rejects_malformed_groups() {
        let parsed: FileConfig = toml::from_str(r#"groups = [["A", "D", "W"]]"#).unwrap();
        assert!(parsed.into_config().is_err());
        assert!(parse_groups("A,D;W").is_err());
        assert!(parse_groups("").is_err());
    }

    #[test]
    fn parses_cli_group_spec() {
        let groups = parse_groups("A,D; W , S").unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1].keys[0].name, "W");
    }

    #[test]
    fn none_disables_the_toggle_key() {
        assert!(parse_toggle("none").unwrap().is_none());
        assert!(parse_toggle("").unwrap().is_none());
    }
}
