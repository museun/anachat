use crate::model;

#[derive(Copy, Clone, Debug)]
enum ChannelState {
    Active,
    Inactive,
    UnreadMessages,
    UnreadMentions,
}

#[derive(Debug)]
pub struct Channel {
    pub name: String,
    pub buffer: Option<String>,
    pub messages: Vec<model::AnaMessage>,
    state: ChannelState,
}

impl Channel {
    pub fn new(name: impl ToString) -> Self {
        Self {
            name: name.to_string(),
            buffer: None,
            messages: Vec::new(),
            state: ChannelState::Active,
        }
    }

    pub fn set_inactive(&mut self) {
        self.state = ChannelState::Inactive
    }

    pub fn set_active(&mut self) {
        self.state = ChannelState::Active
    }

    pub fn set_unread_messages(&mut self) {
        self.state = ChannelState::UnreadMessages
    }

    pub fn set_unread_mentions(&mut self) {
        self.state = ChannelState::UnreadMentions
    }

    pub fn push_message(&mut self, msg: impl Into<model::AnaMessage>) {
        self.messages.push(msg.into())
    }
}
