use std::collections::VecDeque;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::control::{Command, Control};
use crate::engine::{Edge, Engine, Event};
use crate::keys;

#[derive(Default)]
pub struct SimBackend {
    input: VecDeque<Event>,
    trace: Vec<(Event, Vec<Event>)>,
    final_enabled: Option<bool>,
    control: Option<Control>,
    commands: Option<mpsc::Receiver<Command>>,
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
            control: None,
            commands: None,
        })
    }

    #[must_use]
    pub fn with_control(mut self, control: Control, commands: mpsc::Receiver<Command>) -> Self {
        self.control = Some(control);
        self.commands = Some(commands);
        self
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
        loop {
            while let Some(event) = self.input.pop_front() {
                let out = engine.handle(event);
                self.trace.push((event, out));
            }
            let (Some(control), Some(commands)) = (self.control.as_ref(), self.commands.as_ref())
            else {
                break;
            };
            let mut stop = false;
            while let Ok(command) = commands.try_recv() {
                match command {
                    Command::Toggle(reply) => {
                        let target = !engine.enabled();
                        engine.set_enabled(target);
                        control.set_status(engine.enabled());
                        let _ = reply.send(());
                    }
                    Command::SetEnabled(value, reply) => {
                        engine.set_enabled(value);
                        control.set_status(engine.enabled());
                        let _ = reply.send(());
                    }
                    Command::Replace(config, reply) => {
                        let held = engine.held_keys();
                        let mut next =
                            Engine::new(config.groups.clone(), config.toggle, config.sticky);
                        next.resync_held(&held);
                        engine = next;
                        control.set_status(engine.enabled());
                        let _ = reply.send(());
                    }
                    Command::Capture(reply) => {
                        let _ = reply.send(None);
                    }
                    Command::CaptureCancel => {}
                    Command::Stop => stop = true,
                }
            }
            if stop {
                break;
            }
            thread::sleep(Duration::from_millis(10));
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
