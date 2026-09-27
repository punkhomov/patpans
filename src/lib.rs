#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

pub mod backend;
pub mod config;
pub mod engine;
pub mod keys;
mod tray;

pub use config::Config;
pub use engine::{Edge, Engine, Event, Group};
pub use keys::Key;

pub const BUILTIN_SCENARIOS: &[(&str, &str)] = &[
    ("cross", "A+ D+ A- D-"),
    ("sticky", "A+ D+ D- A-"),
    ("taps", "A+ D+ D- D+ D- A-"),
    ("groups", "A+ W+ S+ W- A-"),
    ("repeat", "A+ A+ D+ A+ D- A-"),
    ("passthrough", "Q+ Q-"),
    ("toggle", "A+ D+ F8+ F8- F8+ F8- A- D-"),
];

pub fn scenario_names() -> String {
    BUILTIN_SCENARIOS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}
