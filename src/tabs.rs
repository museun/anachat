use anathema::values::List;

use crate::{channel::Channel, display_channel::DisplayChannel, root_view::RootState};

#[derive(Debug, Default)]
pub struct Tabs {
    pub channels: Vec<Channel>,
    pub active: usize,
}

impl Tabs {
    pub fn active(&self) -> Option<&Channel> {
        self.channels.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut Channel> {
        self.channels.get_mut(self.active)
    }

    pub fn next_channel(&mut self, display: &mut List<DisplayChannel>) {
        if self.channels.is_empty() {
            return;
        }
        let old = self.active;
        self.active = (self.active + 1) % self.channels.len();

        if display[old].is_active() {
            display[old].set_inactive();
        }
        display[self.active].set_active();
    }

    pub fn previous_channel(&mut self, display: &mut List<DisplayChannel>) {
        if self.channels.is_empty() {
            return;
        }

        let old = self.active;

        self.active = (self.active == 0)
            .then_some(self.channels.len())
            .unwrap_or(self.active)
            - 1;

        self.channels[old].set_inactive();
        self.channels[self.active].set_active();

        if display[old].is_active() {
            display[old].set_inactive();
        }
        display[self.active].set_active();
    }

    pub fn switch_to_channel(&mut self, n: usize, display: &mut List<DisplayChannel>) {
        if self.channels.is_empty() || n >= self.channels.len() {
            return;
        }
        let old = self.active;
        self.active = n;

        self.channels[old].set_inactive();
        self.channels[self.active].set_active();

        if display[old].is_active() {
            display[old].set_inactive();
        }
        display[self.active].set_active();
    }

    pub fn join_channel(&mut self, channel: &str, state: &mut RootState) {
        if self.channels.iter().any(|c| c.name == channel) {
            return;
        }
        let old = self.active;
        let len = self.channels.len();
        self.channels.push(Channel::new(channel));
        self.active = len;

        let len = state.channels.len();
        let mut found = false;
        for i in 0..len {
            found &= *state.channels[i].name == channel
        }

        if !found {
            for i in 0..state.channels.len() {
                if state.channels[i].is_active() {
                    state.channels[i].set_inactive();
                }
            }
            state.channels.push_back(DisplayChannel::new(channel));
        }

        self.redraw_messages(old, state);
    }

    pub fn part_channel(&mut self, channel: &str, state: &mut RootState) {
        if let Some(pos) = self.channels.iter().position(|c| c.name == channel) {
            if self.active == pos {
                self.active = self.active.saturating_sub(1);
            }
            self.channels.remove(pos);
        }

        let len = state.channels.len();
        let mut found = <Option<usize>>::None;
        for i in 0..len {
            if *state.channels[i].name == channel {
                found = Some(i);
                break;
            }
        }

        if let Some(found) = found {
            state.channels.remove(found);
            if !state.channels.is_empty() {
                state.channels[self.active].set_active();
            }
        }

        while state.output.pop_front().is_some() {}
        self.synchronize_input_buffer(state);
    }

    pub fn find_index_by_name(&self, name: &str) -> Option<usize> {
        self.channels.iter().position(|c| c.name == name)
    }

    pub fn redraw_messages(&mut self, old: usize, state: &mut RootState) {
        if self.active == old {
            return;
        }

        if let Some(channel) = self.channels.get_mut(old) {
            channel.buffer.replace(std::mem::take(&mut *state.input));
            while let Some(mut msg) = state.output.pop_front() {
                channel.messages.push(std::mem::take(&mut msg))
            }
        }

        self.synchronize_input_buffer(state);
    }

    pub fn synchronize_input_buffer(&mut self, state: &mut RootState) {
        if let Some(active) = self.active_mut() {
            *state.input = active.buffer.take().unwrap_or_default();
            for msg in active.messages.drain(..) {
                state.output.push_back(msg);
            }
        }
    }
}
