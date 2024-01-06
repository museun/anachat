#![cfg_attr(debug_assertions, allow(dead_code, unused_variables,))]

use anathema::values::{List, StateValue};

mod geometry;
mod tab;
mod twitch;

mod model;

#[derive(Debug, Default)]
struct Tabs {
    channels: Vec<Channel>,
    active: usize,
}

impl Tabs {
    fn active(&self) -> Option<&Channel> {
        self.channels.get(self.active)
    }

    fn active_mut(&mut self) -> Option<&mut Channel> {
        self.channels.get_mut(self.active)
    }

    fn next_channel(&mut self, display: &mut List<DisplayChannel>) {
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

    fn previous_channel(&mut self, display: &mut List<DisplayChannel>) {
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

    fn switch_to_channel(&mut self, n: usize, display: &mut List<DisplayChannel>) {
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

    fn join_channel(&mut self, channel: &str, state: &mut root_view::RootState) {
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

    fn part_channel(&mut self, channel: &str, state: &mut root_view::RootState) {
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

        while let Some(..) = state.output.pop_front() {}
        self.synchronize_input_buffer(state);
    }

    fn find_index_by_name(&self, name: &str) -> Option<usize> {
        self.channels.iter().position(|c| c.name == name)
    }

    fn redraw_messages(&mut self, old: usize, state: &mut root_view::RootState) {
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

    fn synchronize_input_buffer(&mut self, state: &mut root_view::RootState) {
        if let Some(active) = self.active_mut() {
            *state.input = active.buffer.take().unwrap_or_default();
            for msg in active.messages.drain(..) {
                state.output.push_back(msg);
            }
        }
    }
}

#[derive(Debug)]
struct Channel {
    name: String,
    buffer: Option<String>,
    messages: Vec<model::AnaMessage>,
    state: ChannelState,
}

#[derive(Copy, Clone, Debug)]
enum ChannelState {
    Active,
    Inactive,
    UnreadMessages,
    UnreadMentions,
}

impl Channel {
    fn new(name: impl ToString) -> Self {
        Self {
            name: name.to_string(),
            buffer: None,
            messages: Vec::new(),
            state: ChannelState::Active,
        }
    }

    fn set_inactive(&mut self) {
        self.state = ChannelState::Inactive
    }

    fn set_active(&mut self) {
        self.state = ChannelState::Active
    }

    fn set_unread_messages(&mut self) {
        self.state = ChannelState::UnreadMessages
    }

    fn set_unread_mentions(&mut self) {
        self.state = ChannelState::UnreadMentions
    }

    fn push_message(&mut self, msg: impl Into<model::AnaMessage>) {
        self.messages.push(msg.into())
    }
}

#[derive(Debug, anathema::values::State)]
pub struct DisplayChannel {
    pub status: StateValue<anathema::core::Color>,
    pub name: StateValue<String>,
}

impl DisplayChannel {
    const ACTIVE: anathema::core::Color = anathema::core::Color::Yellow;
    const INACTIVE: anathema::core::Color = anathema::core::Color::Grey;
    const UNREAD: anathema::core::Color = anathema::core::Color::Blue;
    const MENTIONS: anathema::core::Color = anathema::core::Color::Green;

    pub fn new(name: impl ToString) -> Self {
        Self {
            status: StateValue::new(Self::ACTIVE),
            name: StateValue::new(name.to_string()),
        }
    }

    fn is_active(&self) -> bool {
        matches!(*self.status, Self::ACTIVE)
    }

    fn is_inactive(&self) -> bool {
        matches!(*self.status, Self::INACTIVE)
    }

    fn set_inactive(&mut self) {
        *self.status = Self::INACTIVE
    }

    fn set_active(&mut self) {
        *self.status = Self::ACTIVE
    }

    fn set_unread_messages(&mut self) {
        *self.status = Self::UNREAD
    }

    fn set_unread_mentions(&mut self) {
        *self.status = Self::MENTIONS
    }
}

mod root_view;

fn main() -> anyhow::Result<()> {
    simple_env_load::load_env_from([".secrets.env", ".dev.env"]);

    let config = twitch::Config::from_env()?;

    anathema::core::Factory::register("tab", tab::TabFactory)?;

    let (req_tx, req_rx) = smol::channel::unbounded();
    let (resp_tx, resp_rx) = smol::channel::unbounded();

    let handle = std::thread::spawn(move || twitch::connect(config, req_rx, resp_tx));

    let root_view = root_view::RootView {
        state: root_view::RootState {
            status: StateValue::default(),
            our_user: StateValue::default(),
            input: StateValue::default(),
            channels: List::empty(),
            output: List::empty(),
        },
        tabs: Tabs::default(),
        feed: resp_rx,
        send: req_tx.clone(),
    };

    let template = std::fs::read_to_string("templates/root.aml")?;
    let mut templates = anathema::vm::Templates::new(template, root_view);
    let templates = templates.compile()?;

    let mut runtime = anathema::runtime::Runtime::new(&templates)?;
    runtime.enable_alt_screen = false;

    runtime.run()?;

    // lets ensure the thread ends, we don't care if we can't send to it
    let _ = req_tx.send_blocking(twitch::Request::Disconnect { reconnect: false });

    handle.join().unwrap()
}
