use std::collections::VecDeque;

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::engine::{Edge, Engine, Event};
use crate::keys;

#[derive(Debug, Default)]
pub struct SimBackend {
    input: VecDeque<Event>,
    trace: Vec<(Event, Vec<Event>)>,
    final_enabled: Option<bool>,
}

impl SimBackend {
    pub fn from_script(script: &str) -> Result<Self> {
        let mut input = VecDeque::new();
        for raw_line in script.lines() {
            let line = raw_line.split('#').next().unwrap_or_default();
            for token in line
                .split([',', ' ', '\t'])
                .filter(|token| !token.is_empty())
            {
                input.push_back(parse_token(token)?);
            }
        }
        Ok(Self {
            input,
            trace: Vec::new(),
            final_enabled: None,
        })
    }

    pub fn trace(&self) -> &[(Event, Vec<Event>)] {
        &self.trace
    }

    pub fn outputs(&self) -> Vec<Event> {
        self.trace
            .iter()
            .flat_map(|(_, out)| out.iter().copied())
            .collect()
    }

    pub const fn final_enabled(&self) -> Option<bool> {
        self.final_enabled
    }
}

fn parse_token(token: &str) -> Result<Event> {
    let (name, edge) = match token.strip_suffix('+') {
        Some(name) => (name, Edge::Press),
        None => match token.strip_suffix('-') {
            Some(name) => (name, Edge::Release),
            None => bail!("event `{token}` must end with `+` (press) or `-` (release)"),
        },
    };
    let key = keys::by_name(name).with_context(|| format!("unknown key `{name}`"))?;
    Ok(Event::new(key, edge))
}

impl Backend for SimBackend {
    fn run(&mut self, mut engine: Engine) -> Result<()> {
        while let Some(event) = self.input.pop_front() {
            let out = engine.handle(event);
            self.trace.push((event, out));
        }
        self.final_enabled = Some(engine.enabled());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_script_with_comments_and_separators() {
        let sim = SimBackend::from_script("A+ D-  W+ # comment\n S-").unwrap();
        assert_eq!(sim.input.len(), 4);
    }

    #[test]
    fn rejects_malformed_tokens() {
        assert!(SimBackend::from_script("A").is_err());
        assert!(SimBackend::from_script("A+ Zzz-").is_err());
    }
}
