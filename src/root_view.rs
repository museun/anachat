use anathema::{
    core::KeyModifiers,
    values::{List, StateValue},
};
use smol::channel::{Receiver, Sender};

use crate::{geometry::pos2, model, tab, twitch, DisplayChannel, Tabs};

#[derive(Debug, anathema::values::State)]
pub struct RootState {
    pub status: StateValue<String>,
    pub our_user: StateValue<model::AnaUser>,
    pub input: StateValue<String>,
    pub channels: List<DisplayChannel>,
    pub output: List<model::AnaMessage>,
}

pub struct RootView {
    pub state: RootState,
    pub tabs: Tabs,
    pub feed: Receiver<twitch::Response>,
    pub send: Sender<twitch::Request>,
}

impl RootView {
    const CONNECTING: &'static str = "connecting";
    const CONNECTED: &'static str = "connected";
    const RECONNECTING: &'static str = "reconnecting";
    const INVALID_AUTH: &'static str = "invalid_auth";
    const ON_NO_CHANNELS: &'static str = "on_no_channels";
}

impl anathema::core::View for RootView {
    fn on_event(
        &mut self,
        event: anathema::core::Event,
        _: &mut anathema::core::Nodes<'_>,
    ) -> anathema::core::Event {
        match event {
            anathema::core::Event::KeyPress(code, modifiers, _) => match code {
                anathema::core::KeyCode::Char(n) if modifiers == KeyModifiers::CONTROL => {
                    let old = self.tabs.active;

                    if matches!(n, '0'..='9') {
                        let index = (n as u8 - b'0').checked_sub(1).unwrap_or(9) as usize;
                        self.tabs.switch_to_channel(index, &mut self.state.channels);
                    }

                    match n {
                        'f' => self.tabs.next_channel(&mut self.state.channels),
                        'g' => self.tabs.previous_channel(&mut self.state.channels),
                        _ => {}
                    }

                    self.tabs.redraw_messages(old, &mut self.state);
                }

                anathema::core::KeyCode::Char(c) => {
                    self.state.input.push(c);
                }

                anathema::core::KeyCode::Backspace => {
                    let _ = self.state.input.pop();
                }

                anathema::core::KeyCode::Enter => {
                    let data = std::mem::take(&mut *self.state.input);
                    match process_input_for_commands(&data) {
                        Command::Join { channel } => {
                            for channel in channel.split(',') {
                                let _ = self.send.send_blocking(twitch::Request::JoinChannel {
                                    channel: channel.to_string(),
                                });
                            }
                        }
                        Command::Part { channel } => {
                            let _ = self.send.send_blocking(twitch::Request::PartChannel {
                                channel: channel.to_string(),
                            });
                        }

                        Command::PartCurrent => {
                            if let Some(active) = self.tabs.active() {
                                let _ = self.send.send_blocking(twitch::Request::PartChannel {
                                    channel: active.name.clone(),
                                });
                            }
                        }

                        Command::Reconnect => {
                            let _ = self
                                .send
                                .send_blocking(twitch::Request::Disconnect { reconnect: true });
                        }

                        Command::Quit => {
                            let _ = self
                                .send
                                .send_blocking(twitch::Request::Disconnect { reconnect: false });

                            return anathema::core::Event::Stop;
                        }

                        Command::Error { msg } => {
                            // we need a synthetic buffer to show these errors
                        }

                        Command::None => {
                            if let Some(active) = self.tabs.active() {
                                let _ = self.send.send_blocking(twitch::Request::SendMesage {
                                    channel: active.name.clone(),
                                    data,
                                });
                            }
                        }
                    }
                }
                _ => {}
            },

            anathema::core::Event::MouseDown(x, y, _, _) => {
                let old = self.tabs.active;
                if let Some(name) = tab::TabRegions::containing_point(pos2(x, y)) {
                    if let Some(index) = self.tabs.find_index_by_name(&*name) {
                        self.tabs.switch_to_channel(index, &mut self.state.channels);
                        self.tabs.redraw_messages(old, &mut self.state);
                    }
                }
            }
            _ => {}
        }

        event
    }

    fn tick(&mut self) {
        while let Ok(msg) = self.feed.try_recv() {
            match msg {
                twitch::Response::Message { message } => {
                    let channel_pos = self
                        .tabs
                        .channels
                        .iter()
                        .position(|c| c.name == message.channel);

                    if let Some(index) = channel_pos
                        .filter(|_| self.tabs.active().map(|c| &c.name) != Some(&message.channel))
                    {
                        self.tabs.channels[index].push_message(message);
                        if let Some(pos) = channel_pos {
                            self.state.channels[pos].set_unread_messages();
                            self.tabs.channels[pos].set_unread_messages();
                        }
                    } else {
                        self.state.output.push_back(message.into())
                    }
                }

                twitch::Response::Connecting => {
                    *self.state.status = String::from(Self::CONNECTING);
                }

                twitch::Response::Connected { user } => {
                    self.state.our_user = StateValue::new(user.into());
                    let status = if self.state.channels.is_empty() {
                        Self::ON_NO_CHANNELS
                    } else {
                        Self::CONNECTED
                    };
                    *self.state.status = String::from(status);
                }

                twitch::Response::Disconnected => {
                    *self.state.status = String::from(Self::RECONNECTING);
                }

                twitch::Response::AuthenticationFailed => {
                    *self.state.status = String::from(Self::INVALID_AUTH);
                }

                twitch::Response::JoinChannel { channel } => {
                    self.tabs.join_channel(&channel, &mut self.state);
                    let status = if self.state.channels.is_empty() {
                        Self::ON_NO_CHANNELS
                    } else {
                        Self::CONNECTED
                    };
                    *self.state.status = String::from(status);
                }

                twitch::Response::PartChannel { channel } => {
                    self.tabs.part_channel(&channel, &mut self.state);
                    let status = if self.state.channels.is_empty() {
                        Self::ON_NO_CHANNELS
                    } else {
                        Self::CONNECTED
                    };
                    *self.state.status = String::from(status);
                }
            }
        }
    }

    fn state(&self) -> &dyn anathema::values::State {
        &self.state
    }
}

fn process_input_for_commands<'a>(input: &'a str) -> Command<'a> {
    if let Some((key, val)) = input.strip_prefix('/').and_then(|s| {
        s.split_once(' ')
            .map(|(a, b)| (a, Some(b)))
            .or_else(|| Some((s, None)))
    }) {
        match (key, val) {
            ("join", Some(val)) => Command::Join { channel: val },
            ("part", Some(val)) => Command::Part { channel: val },
            ("part", None) => Command::PartCurrent,
            ("reconnect", _) => Command::Reconnect,
            ("quit", _) => Command::Quit,
            _ => Command::Error {
                msg: format!("unknown command: '{key}' (args: [{val:?}]"),
            },
        }
    } else {
        Command::None
    }
}

enum Command<'a> {
    Join { channel: &'a str },
    Part { channel: &'a str },
    PartCurrent,
    Reconnect,
    Quit,
    None,
    Error { msg: String },
}
