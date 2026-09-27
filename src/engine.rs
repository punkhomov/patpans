use std::collections::HashMap;

use crate::keys::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Press,
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub key: Key,
    pub edge: Edge,
}

impl Event {
    pub const fn new(key: Key, edge: Edge) -> Self {
        Self { key, edge }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Group {
    pub keys: [Key; 2],
}

impl Group {
    pub const fn new(first: Key, second: Key) -> Self {
        Self {
            keys: [first, second],
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct GroupState {
    held: [bool; 2],
    active: Option<usize>,
    last_pressed: Option<usize>,
}

#[derive(Debug)]
pub struct Engine {
    groups: Vec<Group>,
    states: Vec<GroupState>,
    index: HashMap<Key, Vec<usize>>,
    toggle: Option<Key>,
    toggle_held: bool,
    sticky: bool,
    enabled: bool,
}

impl Engine {
    pub fn new(groups: Vec<Group>, toggle: Option<Key>, sticky: bool) -> Self {
        let mut index: HashMap<Key, Vec<usize>> = HashMap::new();
        for (group_index, group) in groups.iter().enumerate() {
            for key in group.keys {
                index.entry(key).or_default().push(group_index);
            }
        }
        let states = vec![GroupState::default(); groups.len()];
        Self {
            groups,
            states,
            index,
            toggle,
            toggle_held: false,
            sticky,
            enabled: true,
        }
    }

    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub const fn toggle(&self) -> Option<Key> {
        self.toggle
    }

    pub const fn sticky(&self) -> bool {
        self.sticky
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Vec<Event> {
        if enabled == self.enabled {
            return Vec::new();
        }
        self.enabled = enabled;
        if enabled {
            self.resync_active()
        } else {
            self.restore_physical()
        }
    }

    pub fn handle(&mut self, event: Event) -> Vec<Event> {
        if self.toggle == Some(event.key) {
            return self.handle_toggle(event);
        }
        let Some(group_ids) = self.index.get(&event.key).cloned() else {
            return vec![event];
        };
        if !self.enabled {
            for group_id in group_ids {
                self.track_physical(group_id, event);
            }
            return vec![event];
        }
        let mut out = Vec::new();
        for group_id in group_ids {
            let Some(slot) = self.group_slot(group_id, event.key) else {
                continue;
            };
            match event.edge {
                Edge::Press => self.on_press(group_id, slot, &mut out),
                Edge::Release => self.on_release(group_id, slot, &mut out),
            }
        }
        out
    }

    fn handle_toggle(&mut self, event: Event) -> Vec<Event> {
        match event.edge {
            Edge::Press if !self.toggle_held => {
                self.toggle_held = true;
                self.set_enabled(!self.enabled)
            }
            Edge::Press => Vec::new(),
            Edge::Release => {
                self.toggle_held = false;
                Vec::new()
            }
        }
    }

    fn group_slot(&self, group_id: usize, key: Key) -> Option<usize> {
        let keys = self.groups[group_id].keys;
        if keys[0] == key {
            Some(0)
        } else if keys[1] == key {
            Some(1)
        } else {
            None
        }
    }

    fn on_press(&mut self, group_id: usize, slot: usize, out: &mut Vec<Event>) {
        let key = self.groups[group_id].keys[slot];
        let state = &mut self.states[group_id];
        if state.held[slot] {
            if state.active == Some(slot) {
                out.push(Event::new(key, Edge::Press));
            }
            return;
        }
        state.held[slot] = true;
        state.last_pressed = Some(slot);
        if state.active == Some(slot) {
            out.push(Event::new(key, Edge::Press));
            return;
        }
        if let Some(active) = state.active {
            out.push(Event::new(
                self.groups[group_id].keys[active],
                Edge::Release,
            ));
        }
        state.active = Some(slot);
        out.push(Event::new(key, Edge::Press));
    }

    fn on_release(&mut self, group_id: usize, slot: usize, out: &mut Vec<Event>) {
        let key = self.groups[group_id].keys[slot];
        let state = &mut self.states[group_id];
        if !state.held[slot] {
            out.push(Event::new(key, Edge::Release));
            return;
        }
        state.held[slot] = false;
        if state.last_pressed == Some(slot) {
            let other = 1 - slot;
            state.last_pressed = if state.held[other] { Some(other) } else { None };
        }
        if state.active != Some(slot) {
            return;
        }
        let other = 1 - slot;
        if self.sticky && state.held[other] {
            state.active = Some(other);
            out.push(Event::new(key, Edge::Release));
            out.push(Event::new(self.groups[group_id].keys[other], Edge::Press));
        } else {
            state.active = None;
            out.push(Event::new(key, Edge::Release));
        }
    }

    fn track_physical(&mut self, group_id: usize, event: Event) {
        let Some(slot) = self.group_slot(group_id, event.key) else {
            return;
        };
        let state = &mut self.states[group_id];
        match event.edge {
            Edge::Press => {
                state.held[slot] = true;
                state.last_pressed = Some(slot);
            }
            Edge::Release => {
                state.held[slot] = false;
                if state.last_pressed == Some(slot) {
                    let other = 1 - slot;
                    state.last_pressed = if state.held[other] { Some(other) } else { None };
                }
            }
        }
    }

    fn resync_active(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        for (group_id, state) in self.states.iter_mut().enumerate() {
            state.active = match state.last_pressed {
                Some(slot) if state.held[slot] => Some(slot),
                _ => (0..2).find(|&slot| state.held[slot]),
            };
            if let Some(active) = state.active {
                for slot in 0..2 {
                    if state.held[slot] && slot != active {
                        out.push(Event::new(self.groups[group_id].keys[slot], Edge::Release));
                    }
                }
            }
        }
        out
    }

    fn restore_physical(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        for (group_id, state) in self.states.iter_mut().enumerate() {
            if let Some(active) = state.active {
                for slot in 0..2 {
                    if state.held[slot] && slot != active {
                        out.push(Event::new(self.groups[group_id].keys[slot], Edge::Press));
                    }
                }
            }
            state.active = None;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;

    fn key(name: &str) -> Key {
        keys::by_name(name).unwrap()
    }

    #[test]
    fn unmanaged_keys_are_returned_unchanged() {
        let mut engine = Engine::new(Vec::new(), None, true);
        let event = Event::new(key("P"), Edge::Press);
        assert_eq!(engine.handle(event), vec![event]);
    }

    #[test]
    fn set_enabled_is_idempotent() {
        let mut engine = Engine::new(Vec::new(), None, true);
        assert_eq!(engine.set_enabled(true), Vec::<Event>::new());
        assert!(engine.enabled());
        assert_eq!(engine.set_enabled(false), Vec::<Event>::new());
        assert!(!engine.enabled());
        assert_eq!(engine.set_enabled(false), Vec::<Event>::new());
    }

    #[test]
    fn sustained_events_keep_state_bounded() {
        let mut engine = Engine::new(vec![Group::new(key("A"), key("D"))], None, true);
        let mut produced = 0;
        for _ in 0..20_000 {
            for (name, edge) in [
                ("A", Edge::Press),
                ("D", Edge::Press),
                ("D", Edge::Release),
                ("A", Edge::Release),
            ] {
                let out = engine.handle(Event::new(key(name), edge));
                assert!(out.len() <= 2, "per-event output must stay bounded");
                produced += out.len();
            }
        }
        assert_eq!(produced, 120_000);
    }
}
