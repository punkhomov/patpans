use patpans::backend::Backend;
use patpans::backend::sim::SimBackend;
use patpans::engine::{Edge, Engine, Event, Group};
use patpans::keys;

fn key(name: &str) -> patpans::Key {
    keys::by_name(name).unwrap_or_else(|| panic!("unknown key `{name}`"))
}

fn default_engine() -> Engine {
    Engine::new(
        vec![
            Group::new(key("A"), key("D")),
            Group::new(key("W"), key("S")),
        ],
        Some(key("F8")),
        true,
    )
}

fn event(name: &str, edge: Edge) -> Event {
    Event::new(key(name), edge)
}

fn replay_with(engine: Engine, script: &str) -> Vec<Event> {
    let mut sim = SimBackend::from_script(script).unwrap();
    sim.run(engine).unwrap();
    sim.outputs()
}

fn replay(script: &str) -> Vec<Event> {
    replay_with(default_engine(), script)
}

#[test]
fn last_pressed_wins_and_previous_key_is_released() {
    assert_eq!(
        replay("A+ D+ A- D-"),
        vec![
            event("A", Edge::Press),
            event("A", Edge::Release),
            event("D", Edge::Press),
            event("D", Edge::Release),
        ]
    );
}

#[test]
fn sticky_restores_a_key_that_is_still_held() {
    assert_eq!(
        replay("A+ D+ D- A-"),
        vec![
            event("A", Edge::Press),
            event("A", Edge::Release),
            event("D", Edge::Press),
            event("D", Edge::Release),
            event("A", Edge::Press),
            event("A", Edge::Release),
        ]
    );
}

#[test]
fn rapid_taps_alternate_between_keys() {
    assert_eq!(
        replay("A+ D+ D- D+ D- A-"),
        vec![
            event("A", Edge::Press),
            event("A", Edge::Release),
            event("D", Edge::Press),
            event("D", Edge::Release),
            event("A", Edge::Press),
            event("A", Edge::Release),
            event("D", Edge::Press),
            event("D", Edge::Release),
            event("A", Edge::Press),
            event("A", Edge::Release),
        ]
    );
}

#[test]
fn groups_do_not_interfere_with_each_other() {
    assert_eq!(
        replay("A+ W+ S+ W- A-"),
        vec![
            event("A", Edge::Press),
            event("W", Edge::Press),
            event("W", Edge::Release),
            event("S", Edge::Press),
            event("A", Edge::Release),
        ]
    );
}

#[test]
fn keyboard_repeat_is_forwarded_only_for_the_active_key() {
    assert_eq!(
        replay("A+ A+ D+ A+ D- A-"),
        vec![
            event("A", Edge::Press),
            event("A", Edge::Press),
            event("A", Edge::Release),
            event("D", Edge::Press),
            event("D", Edge::Release),
            event("A", Edge::Press),
            event("A", Edge::Release),
        ]
    );
}

#[test]
fn keys_outside_groups_pass_through_unchanged() {
    assert_eq!(
        replay("Q+ Q-"),
        vec![event("Q", Edge::Press), event("Q", Edge::Release)]
    );
}

#[test]
fn toggle_disables_and_restores_the_physical_state() {
    assert_eq!(
        replay("A+ D+ F8+ F8- F8+ F8- A- D-"),
        vec![
            event("A", Edge::Press),
            event("A", Edge::Release),
            event("D", Edge::Press),
            event("A", Edge::Press),
            event("D", Edge::Release),
        ]
    );
}

#[test]
fn non_sticky_mode_does_not_restore_the_previous_key() {
    let engine = Engine::new(vec![Group::new(key("A"), key("D"))], None, false);
    assert_eq!(
        replay_with(engine, "A+ D+ D- A-"),
        vec![
            event("A", Edge::Press),
            event("A", Edge::Release),
            event("D", Edge::Press),
            event("D", Edge::Release),
        ]
    );
}

#[test]
fn malformed_scripts_are_rejected() {
    assert!(SimBackend::from_script("A+ Zzz-").is_err());
    assert!(SimBackend::from_script("A").is_err());
}
