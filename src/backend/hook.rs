use crate::engine::{Edge, Engine, Event};
use crate::keys::{self, Key};

pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookInput {
    pub message: u32,
    pub vk: u32,
    pub injected: bool,
    pub tag: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Pass,
    Swallow,
    Inject(Vec<Event>),
}

pub const fn edge_from_message(message: u32) -> Option<Edge> {
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN => Some(Edge::Press),
        WM_KEYUP | WM_SYSKEYUP => Some(Edge::Release),
        _ => None,
    }
}

pub fn relevant(input: &HookInput, test_tag: Option<usize>) -> Option<(Key, Edge)> {
    if input.injected && test_tag != Some(input.tag) {
        return None;
    }
    let edge = edge_from_message(input.message)?;
    let vk = u16::try_from(input.vk).ok()?;
    let key = keys::by_windows_vk(vk)?;
    Some((key, edge))
}

pub fn decide(engine: &mut Engine, key: Key, edge: Edge) -> Decision {
    let event = Event::new(key, edge);
    let out = engine.handle(event);
    if out.is_empty() {
        Decision::Swallow
    } else if out.len() == 1 && out[0] == event {
        Decision::Pass
    } else {
        Decision::Inject(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    fn key(name: &str) -> Key {
        keys::by_name(name).unwrap()
    }

    fn input(message: u32, name: &str, injected: bool, tag: usize) -> HookInput {
        HookInput {
            message,
            vk: u32::from(key(name).windows_vk),
            injected,
            tag,
        }
    }

    fn engine() -> Engine {
        Engine::new(
            vec![crate::engine::Group::new(key("A"), key("D"))],
            None,
            true,
        )
    }

    #[test]
    fn maps_key_messages_to_edges() {
        assert_eq!(edge_from_message(WM_KEYDOWN), Some(Edge::Press));
        assert_eq!(edge_from_message(WM_SYSKEYDOWN), Some(Edge::Press));
        assert_eq!(edge_from_message(WM_KEYUP), Some(Edge::Release));
        assert_eq!(edge_from_message(WM_SYSKEYUP), Some(Edge::Release));
        assert_eq!(edge_from_message(0x0200), None);
    }

    #[test]
    fn injected_events_are_ignored_unless_tagged_for_tests() {
        assert_eq!(relevant(&input(WM_KEYDOWN, "A", true, 0), None), None);
        assert_eq!(
            relevant(&input(WM_KEYDOWN, "A", true, 0), Some(0x1234)),
            None
        );
        assert_eq!(
            relevant(&input(WM_KEYDOWN, "A", true, 0x1234), Some(0x1234)),
            Some((key("A"), Edge::Press))
        );
        assert_eq!(
            relevant(&input(WM_KEYDOWN, "A", false, 0), None),
            Some((key("A"), Edge::Press))
        );
    }

    #[test]
    fn unsupported_messages_and_unknown_keys_are_ignored() {
        assert_eq!(relevant(&input(0x0200, "A", false, 0), None), None);
        let unknown = HookInput {
            message: WM_KEYDOWN,
            vk: 0xFFFF,
            injected: false,
            tag: 0,
        };
        assert_eq!(relevant(&unknown, None), None);
    }

    #[test]
    fn single_unchanged_event_is_passed_through() {
        let mut engine = engine();
        assert_eq!(decide(&mut engine, key("A"), Edge::Press), Decision::Pass);
    }

    #[test]
    fn override_is_injected_and_suppressed_release_is_swallowed() {
        let mut engine = engine();
        assert_eq!(decide(&mut engine, key("A"), Edge::Press), Decision::Pass);
        let decision = decide(&mut engine, key("D"), Edge::Press);
        assert_eq!(
            decision,
            Decision::Inject(vec![
                Event::new(key("A"), Edge::Release),
                Event::new(key("D"), Edge::Press),
            ])
        );
        assert_eq!(
            decide(&mut engine, key("A"), Edge::Release),
            Decision::Swallow
        );
        assert_eq!(decide(&mut engine, key("D"), Edge::Release), Decision::Pass);
    }

    #[test]
    fn only_the_override_produces_injections() {
        let mut engine = engine();
        let script = [
            ("A", Edge::Press),
            ("D", Edge::Press),
            ("A", Edge::Release),
            ("D", Edge::Release),
        ];
        let injected: usize = script
            .iter()
            .filter_map(|(name, edge)| match decide(&mut engine, key(name), *edge) {
                Decision::Inject(events) => Some(events.len()),
                Decision::Pass | Decision::Swallow => None,
            })
            .sum();
        assert_eq!(injected, 2);
    }

    #[test]
    fn unmanaged_key_is_passed_through() {
        let mut engine = engine();
        assert_eq!(decide(&mut engine, key("Q"), Edge::Press), Decision::Pass);
        assert_eq!(decide(&mut engine, key("Q"), Edge::Release), Decision::Pass);
    }
}
