use anathema::{core::Color, values::StateValue};

#[derive(Debug, anathema::values::State)]
pub struct DisplayChannel {
    pub status: StateValue<Color>,
    pub name: StateValue<String>,
}

impl DisplayChannel {
    const ACTIVE: Color = Color::Yellow;
    const INACTIVE: Color = Color::Grey;
    const UNREAD: Color = Color::Blue;
    const MENTIONS: Color = Color::Green;

    pub fn new(name: impl ToString) -> Self {
        Self {
            status: StateValue::new(Self::ACTIVE),
            name: StateValue::new(name.to_string()),
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(*self.status, Self::ACTIVE)
    }

    pub fn is_inactive(&self) -> bool {
        matches!(*self.status, Self::INACTIVE)
    }

    pub fn set_inactive(&mut self) {
        *self.status = Self::INACTIVE
    }

    pub fn set_active(&mut self) {
        *self.status = Self::ACTIVE
    }

    pub fn set_unread_messages(&mut self) {
        *self.status = Self::UNREAD
    }

    pub fn set_unread_mentions(&mut self) {
        *self.status = Self::MENTIONS
    }
}
