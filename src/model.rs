use anathema::values::StateValue;

use crate::twitch;

#[derive(Debug, Default, anathema::values::State)]
pub struct AnaMessage {
    pub sender: StateValue<AnaUser>,
    pub channel: StateValue<String>,
    pub data: StateValue<String>,
}

impl From<twitch::Message> for AnaMessage {
    fn from(value: twitch::Message) -> Self {
        Self {
            sender: StateValue::new(value.sender.into()),
            channel: StateValue::new(value.channel),
            data: StateValue::new(value.data),
        }
    }
}

const fn map_color(color: twitch_message::Color) -> anathema::core::Color {
    let twitch_message::Color(r, g, b) = color;
    anathema::core::Color::Rgb { r, g, b }
}

#[derive(Debug, anathema::values::State)]
pub struct AnaUser {
    pub color: StateValue<anathema::core::Color>,
    pub user_id: StateValue<String>,
    pub name: StateValue<String>,
}

impl From<twitch::User> for AnaUser {
    fn from(value: twitch::User) -> Self {
        Self {
            color: StateValue::new(map_color(value.color)),
            user_id: StateValue::new(value.user_id),
            name: StateValue::new(value.name),
        }
    }
}

impl Default for AnaUser {
    fn default() -> Self {
        Self {
            color: StateValue::new(anathema::core::Color::White),
            user_id: Default::default(),
            name: Default::default(),
        }
    }
}
