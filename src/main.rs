#![cfg_attr(debug_assertions, allow(dead_code, unused_variables,))]

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    hash::Hash,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    task::Poll,
};

use anathema::{
    core::{KeyModifiers, LocalPos, Widget},
    render::Size,
    values::{List, StateValue},
    widgets::layout::text::TextLayout,
};
use smol::{
    channel::{Receiver, Sender},
    future::FutureExt,
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
};
use twitch_message::{encode::Encode, messages::MessageKind};

enum Request {
    JoinChannel { channel: String },
    PartChannel { channel: String },
    SendMesage { channel: String, data: String },
    Disconnect { reconnect: bool },
}

#[derive(Debug)]
enum Response {
    Connecting,
    Connected { user: User },
    JoinChannel { channel: String },
    PartChannel { channel: String },
    Message { message: Message },
    Disconnected,
    AuthenticationFailed,
}

#[derive(Clone, Debug)]
struct Message {
    sender: User,
    channel: String,
    data: String,
}

#[derive(Clone, Debug)]
struct User {
    color: twitch_message::Color,
    user_id: String,
    name: String,
}

fn connect(config: Config, req: Receiver<Request>, resp: Sender<Response>) -> anyhow::Result<()> {
    let addr = twitch_message::TWITCH_IRC_ADDRESS;

    smol::block_on::<anyhow::Result<()>>(async move {
        let mut requested_channels = HashSet::<String>::new();

        'outer: loop {
            if resp.send(Response::Connecting).await.is_err() {
                break 'outer;
            }

            let Ok(stream) = smol::net::TcpStream::connect(addr).await else {
                if resp.send(Response::Disconnected).await.is_err() {
                    break 'outer;
                }

                smol::Timer::after(std::time::Duration::from_secs(3)).await;
                continue 'outer;
            };

            let (read, write) = smol::io::split(stream);

            let mut reader = Reader::new(read);
            let mut encoder = AsyncEncoder::new(write);

            if register(&config, &mut encoder).await.is_err() {
                if resp.send(Response::Disconnected).await.is_err() {
                    break 'outer;
                }

                smol::Timer::after(std::time::Duration::from_secs(3)).await;
                continue 'outer;
            }

            struct PendingMessage {
                user: User,
                data: String,
            }

            let mut pending_messages = <HashMap<String, VecDeque<PendingMessage>>>::new();

            let mut our_name = <Option<String>>::None;
            let mut our_user = <Option<User>>::None;

            'inner: loop {
                let read_line = reader.read_line();
                let recv_req = req.recv();
                let read_line = std::pin::pin!(read_line);
                let recv_req = std::pin::pin!(recv_req);

                let line = match select2(read_line, recv_req).await {
                    Either::Left(Ok(read_line)) => read_line,
                    Either::Right(Ok(recv_req)) => match recv_req {
                        Request::JoinChannel { channel } => {
                            let join = twitch_message::encode::join(&channel);
                            if encoder.encode(join).is_err() {
                                break 'inner;
                            }

                            if encoder.flush().await.is_err() {
                                break 'inner;
                            }

                            continue 'inner;
                        }

                        Request::PartChannel { channel } => {
                            let part = twitch_message::encode::part(&channel);
                            if encoder.encode(part).is_err() {
                                break 'inner;
                            }

                            if encoder.flush().await.is_err() {
                                break 'inner;
                            }

                            continue 'inner;
                        }

                        Request::SendMesage { channel, data } => {
                            let msg = twitch_message::encode::privmsg(&channel, &data);
                            if encoder.encode(msg).is_err() {
                                break 'inner;
                            }

                            if encoder.flush().await.is_err() {
                                break 'inner;
                            }

                            pending_messages.entry(channel).or_default().push_back(
                                PendingMessage {
                                    user: our_user.clone().expect("we must be a user"),
                                    data,
                                },
                            );

                            continue 'inner;
                        }

                        Request::Disconnect { reconnect } => {
                            if encoder.encode(twitch_message::encode::raw("QUIT")).is_ok() {
                                let _ = encoder.flush().await;
                            }

                            if !reconnect {
                                break 'outer;
                            } else {
                                break 'inner;
                            }
                        }
                    },

                    Either::Left(Err(..)) => break 'inner,
                    Either::Right(Err(..)) => break 'outer,
                };

                for msg in twitch_message::parse_many(&line).flatten() {
                    use twitch_message::messages::TwitchMessage as M;
                    match msg.as_enum() {
                        #[allow(deprecated)]
                        M::Notice(msg) if msg.message == "Login authentication failed" => {
                            if resp.send(Response::AuthenticationFailed).await.is_err() {
                                break 'outer;
                            }
                        }

                        M::Reconnect(_) => break 'inner,

                        M::Ping(msg) => {
                            encoder
                                .encode(twitch_message::encode::pong(&msg.token))
                                .expect("identity transformation");
                            if encoder.flush().await.is_err() {
                                break 'inner;
                            }
                        }

                        M::Ready(msg) => {
                            let _ = our_name.replace(msg.name.to_string());
                        }

                        M::UserState(msg) if msg.msg_id().is_some() => {
                            if let Some(data) = twitch_message::parse_many(&msg.raw)
                                .flatten()
                                .next()
                                .and_then(|mut s| s.args.pop())
                            {
                                if let Some(queue) = pending_messages.get_mut(&*data) {
                                    if let Some(msg) = queue.pop_front() {
                                        let message = Message {
                                            sender: msg.user,
                                            channel: data.to_string(),
                                            data: msg.data,
                                        };
                                        if resp.send(Response::Message { message }).await.is_err() {
                                            break 'outer;
                                        }
                                    }
                                }
                            }
                        }

                        M::GlobalUserState(msg) => {
                            for channel in &requested_channels {
                                let join = twitch_message::encode::join(channel);
                                if encoder.encode(join).is_err() {
                                    break 'inner;
                                }
                            }

                            if encoder.flush().await.is_err() {
                                break 'inner;
                            }

                            let user = User {
                                color: msg.color().unwrap_or_default(),
                                user_id: msg.user_id().expect("we must have a user-id").to_string(),
                                name: our_name.clone().expect("we must have a user name"),
                            };

                            our_user.replace(user.clone());

                            if resp.send(Response::Connected { user }).await.is_err() {
                                break 'outer;
                            }
                        }

                        // TODO JOIN
                        // TODO PART
                        M::Privmsg(msg) => {
                            let message = Message {
                                sender: User {
                                    color: msg.color().unwrap_or_default(),
                                    user_id: msg
                                        .user_id()
                                        .expect("user must have a user-id")
                                        .to_string(),
                                    name: msg.sender.to_string(),
                                },
                                channel: msg.channel.to_string(),
                                data: msg.data.to_string(),
                            };

                            if resp.send(Response::Message { message }).await.is_err() {
                                break 'outer;
                            }
                        }

                        M::Message(msg)
                            if matches!(msg.kind, MessageKind::Unknown(Cow::Borrowed("JOIN"))) =>
                        {
                            if msg.prefix.as_name_str() == our_name.as_deref() {
                                if let Some(channel) = msg.args.get(0) {
                                    if requested_channels.insert(channel.to_string()) {
                                        if resp
                                            .send(Response::JoinChannel {
                                                channel: channel.to_string(),
                                            })
                                            .await
                                            .is_err()
                                        {
                                            break 'outer;
                                        }
                                    }
                                }
                            }
                        }

                        M::Message(msg)
                            if matches!(msg.kind, MessageKind::Unknown(Cow::Borrowed("PART"))) =>
                        {
                            if msg.prefix.as_name_str() == our_name.as_deref() {
                                if let Some(channel) = msg.args.get(0) {
                                    if resp
                                        .send(Response::PartChannel {
                                            channel: channel.to_string(),
                                        })
                                        .await
                                        .is_err()
                                    {
                                        break 'outer;
                                    }
                                    requested_channels.remove(&**channel);
                                }
                            }
                        }

                        _ => {}
                    }
                }
            }

            if resp.send(Response::Disconnected).await.is_err() {
                break 'outer;
            }

            smol::Timer::after(std::time::Duration::from_secs(3)).await;
        }

        anyhow::Result::Ok(())
    })
}

struct Config {
    name: String,
    oauth: String,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        fn get(key: &str) -> anyhow::Result<String> {
            std::env::var(key).map_err(|_| anyhow::anyhow!("`{key}` must exist in the environment"))
        }

        Ok(Self {
            name: get("TWITCH_NAME")?,
            oauth: get("TWITCH_OAUTH")?,
        })
    }
}

struct AsyncEncoder<W> {
    buf: Vec<u8>,
    writer: BufWriter<W>,
}

impl<W: AsyncWrite + 'static + Unpin> AsyncEncoder<W> {
    fn new(writer: W) -> Self {
        Self {
            buf: Vec::new(),
            writer: BufWriter::new(writer),
        }
    }

    fn encode(&mut self, msg: impl twitch_message::encode::Encodable) -> anyhow::Result<()> {
        self.buf.encode_msg(msg).map_err(Into::into)
    }

    async fn flush(&mut self) -> anyhow::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }

        let buf = std::mem::take(&mut self.buf);
        self.writer.write_all(&*buf).await?;
        self.writer.flush().await?;
        Ok(())
    }
}

async fn register(
    config: &Config,
    encoder: &mut AsyncEncoder<impl AsyncWrite + 'static + Unpin>,
) -> anyhow::Result<()> {
    let msg = twitch_message::encode::register(
        &config.name,
        &config.oauth,
        twitch_message::encode::ALL_CAPABILITIES,
    );
    encoder.encode(msg)?;
    encoder.flush().await
}

struct Reader<R> {
    buf: String,
    reader: smol::io::BufReader<R>,
}

impl<R: AsyncRead + 'static + Unpin> Reader<R> {
    fn new(read: R) -> Self {
        Self {
            buf: String::with_capacity(1024),
            reader: BufReader::new(read),
        }
    }

    async fn read_line(&mut self) -> anyhow::Result<String> {
        let pos = self.reader.read_line(&mut self.buf).await?;
        anyhow::ensure!(pos != 0, "unexpected EOF");

        let mut buf = std::mem::take(&mut self.buf);
        buf.truncate(pos);
        Ok(buf)
    }
}

pin_project_lite::pin_project! {
    struct Select2<L,R> {
        left: L,
        right: R,
    }
}

impl<L, R> Future for Select2<L, R>
where
    L: Future + Unpin,
    R: Future + Unpin,
{
    type Output = Either<L::Output, R::Output>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();

        macro_rules! poll {
            ($expr:ident => $out:ident) => {
                if let Poll::Ready(t) = this.$expr.poll(cx) {
                    return Poll::Ready(Either::$out(t));
                }
            };
        }

        if fastrand::bool() {
            poll!(left => Left);
            poll!(right => Right);
        } else {
            poll!(right => Right);
            poll!(left => Left);
        }

        Poll::Pending
    }
}

enum Either<L, R> {
    Left(L),
    Right(R),
}

fn select2<L, R>(left: L, right: R) -> Select2<L, R>
where
    L: Future + Unpin,
    R: Future + Unpin,
{
    Select2 { left, right }
}

impl From<Message> for AnaMessage {
    fn from(value: Message) -> Self {
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

#[derive(Debug, Default, anathema::values::State)]
struct AnaMessage {
    sender: StateValue<AnaUser>,
    channel: StateValue<String>,
    data: StateValue<String>,
}

#[derive(Debug, anathema::values::State)]
struct AnaUser {
    color: StateValue<anathema::core::Color>,
    user_id: StateValue<String>,
    name: StateValue<String>,
}

impl From<User> for AnaUser {
    fn from(value: User) -> Self {
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

    fn join_channel(&mut self, channel: &str, state: &mut RootState) {
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

    fn part_channel(&mut self, channel: &str, state: &mut RootState) {
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

    // #[track_caller]
    fn redraw_messages(&mut self, old: usize, state: &mut RootState) {
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

    fn synchronize_input_buffer(&mut self, state: &mut RootState) {
        if let Some(active) = self.active_mut() {
            *state.input = active.buffer.take().unwrap_or_default();
            for msg in active.messages.drain(..) {
                state.output.push_back(msg);
            }
        }
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

#[derive(Debug)]
struct Channel {
    name: String,
    buffer: Option<String>,
    messages: Vec<AnaMessage>,
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

    fn push_message(&mut self, msg: impl Into<AnaMessage>) {
        self.messages.push(msg.into())
    }
}

#[derive(Debug, anathema::values::State)]
struct DisplayChannel {
    status: StateValue<anathema::core::Color>,
    name: StateValue<String>,
}

impl DisplayChannel {
    const ACTIVE: anathema::core::Color = anathema::core::Color::Yellow;
    const INACTIVE: anathema::core::Color = anathema::core::Color::Grey;
    const UNREAD: anathema::core::Color = anathema::core::Color::Blue;
    const MENTIONS: anathema::core::Color = anathema::core::Color::Green;

    fn new(name: impl ToString) -> Self {
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

#[derive(Debug, anathema::values::State)]
struct RootState {
    status: StateValue<String>,
    our_user: StateValue<AnaUser>,
    input: StateValue<String>,
    channels: List<DisplayChannel>,
    output: List<AnaMessage>,
}

struct RootView {
    state: RootState,
    tabs: Tabs,
    feed: Receiver<Response>,
    send: Sender<Request>,
    stop: Arc<AtomicBool>,
}

impl RootView {
    const CONNECTING: &'static str = "connecting";
    const CONNECTED: &'static str = "connected";
    const RECONNECTING: &'static str = "reconnecting";
    const INVALID_AUTH: &'static str = "invalid_auth";
    const ON_NO_CHANNELS: &'static str = "on_no_channels";
}

impl anathema::core::View for RootView {
    fn on_event(&mut self, event: anathema::core::Event, _: &mut anathema::core::Nodes<'_>) {
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
                                let _ = self.send.send_blocking(Request::JoinChannel {
                                    channel: channel.to_string(),
                                });
                            }
                        }
                        Command::Part { channel } => {
                            let _ = self.send.send_blocking(Request::PartChannel {
                                channel: channel.to_string(),
                            });
                        }

                        Command::PartCurrent => {
                            if let Some(active) = self.tabs.active() {
                                let _ = self.send.send_blocking(Request::PartChannel {
                                    channel: active.name.clone(),
                                });
                            }
                        }

                        Command::Reconnect => {
                            let _ = self
                                .send
                                .send_blocking(Request::Disconnect { reconnect: true });
                        }

                        Command::Quit => {
                            let _ = self
                                .send
                                .send_blocking(Request::Disconnect { reconnect: false });

                            self.stop.store(true, Ordering::SeqCst);
                        }

                        Command::Error { msg } => {
                            // we need a synthetic buffer to show these errors
                        }

                        Command::None => {
                            if let Some(active) = self.tabs.active() {
                                let _ = self.send.send_blocking(Request::SendMesage {
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
                let pos = pos2(x, y);
                let old = self.tabs.active;
                if let Some(name) = TabRegions::containing_point(pos) {
                    if let Some(index) = self.tabs.find_index_by_name(&*name) {
                        self.tabs.switch_to_channel(index, &mut self.state.channels);
                        self.tabs.redraw_messages(old, &mut self.state);
                    }
                }
            }
            _ => {}
        }
    }

    fn tick(&mut self) {
        while let Ok(msg) = self.feed.try_recv() {
            match msg {
                Response::Message { message } => {
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

                Response::Connecting => {
                    *self.state.status = String::from(Self::CONNECTING);
                }

                Response::Connected { user } => {
                    self.state.our_user = StateValue::new(user.into());
                    let status = if self.state.channels.is_empty() {
                        Self::ON_NO_CHANNELS
                    } else {
                        Self::CONNECTED
                    };
                    *self.state.status = String::from(status);
                }

                Response::Disconnected => {
                    *self.state.status = String::from(Self::RECONNECTING);
                }

                Response::AuthenticationFailed => {
                    *self.state.status = String::from(Self::INVALID_AUTH);
                }

                Response::JoinChannel { channel } => {
                    self.tabs.join_channel(&channel, &mut self.state);
                    let status = if self.state.channels.is_empty() {
                        Self::ON_NO_CHANNELS
                    } else {
                        Self::CONNECTED
                    };
                    *self.state.status = String::from(status);
                }

                Response::PartChannel { channel } => {
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

#[derive(Debug)]
struct Tab {
    pub text: anathema::values::Value<String>,
    pub style: anathema::core::WidgetStyle,
    layout: TextLayout,
}

impl Tab {
    const KIND: &'static str = "Tab";
}

impl Widget for Tab {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn update(
        &mut self,
        context: &anathema::values::Context<'_, '_>,
        node_id: &anathema::values::NodeId,
    ) {
        self.text.resolve(context, node_id);
        self.style.resolve(context, node_id);
    }

    fn layout(
        &mut self,
        nodes: &mut anathema::core::LayoutNodes<'_, '_, '_>,
    ) -> anathema::core::error::Result<anathema::render::Size> {
        let constraints = nodes.constraints;
        self.layout.reset(
            Size::new(constraints.max_width, constraints.max_height),
            true,
        );
        self.layout.process(self.text.str());
        self.layout.finish();

        let size = self.layout.size();
        Ok(size)
    }

    fn paint(
        &mut self,
        children: &mut anathema::core::Nodes<'_>,
        mut ctx: anathema::core::contexts::PaintCtx<'_, anathema::core::contexts::WithSize>,
    ) {
        let start = ctx.global_pos;
        if let Some(anathema::core::LocalPos { x, y }) =
            ctx.print(self.text.str(), self.style.style(), LocalPos::ZERO)
        {
            TabRegions::insert(
                self.text.str(),
                Rect::from_min_max(
                    pos2(start.x as _, start.y as _),
                    pos2(start.x as u16 + x as u16, start.y as u16 + y as u16),
                ),
            );
        }

        for (widget, children) in children.iter_mut() {
            let ctx = ctx.to_unsized();
            widget.paint(children, ctx);
        }
    }

    fn position(
        &mut self,
        _children: &mut anathema::core::Nodes<'_>,
        _ctx: anathema::core::contexts::PositionCtx,
    ) {
    }
}

struct TabFactory;

impl anathema::core::WidgetFactory for TabFactory {
    fn make(
        &self,
        mut ctx: anathema::core::FactoryContext<'_>,
    ) -> anathema::core::error::Result<Box<dyn anathema::core::AnyWidget>> {
        let widget = Tab {
            style: ctx.style(),
            layout: TextLayout::new(
                Size::ZERO,
                false,
                anathema::widgets::layout::text::Wrap::Normal,
            ),
            text: ctx.text.take(),
        };

        Ok(Box::new(widget))
    }
}

#[derive(Default)]
struct TabRegions {
    map: Vec<(Rect, Arc<String>)>,
}

static REGIONS: Mutex<TabRegions> = Mutex::new(TabRegions { map: Vec::new() });

impl TabRegions {
    fn insert(name: &str, rect: Rect) {
        let g = &mut *REGIONS.lock().unwrap();
        if let Some(pos) = g.map.iter().position(|(_, v)| &**v == name) {
            g.map[pos].0 = rect;
        } else {
            g.map.push((rect, Arc::new(name.to_string())))
        }
    }

    fn get_all() -> Vec<(Rect, Arc<String>)> {
        let g = &*REGIONS.lock().unwrap();
        g.map.clone()
    }

    fn containing_point(pos: Pos2) -> Option<Arc<String>> {
        let g = &*REGIONS.lock().unwrap();
        g.map
            .iter()
            .find_map(|(k, v)| k.contains(pos).then(|| Arc::clone(&v)))
    }

    fn get(rect: Rect) -> Option<Arc<String>> {
        let g = &*REGIONS.lock().unwrap();
        g.map
            .iter()
            .find_map(|(k, v)| (*k == rect).then(|| Arc::clone(&v)))
    }
}

#[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash)]
struct Pos2 {
    x: u16,
    y: u16,
}

const fn pos2(x: u16, y: u16) -> Pos2 {
    Pos2 { x, y }
}

#[derive(Copy, Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash)]
struct Rect {
    min: Pos2,
    max: Pos2,
}

impl Rect {
    const fn from_min_max(min: Pos2, max: Pos2) -> Self {
        Self { min, max }
    }

    const fn contains(&self, pos: Pos2) -> bool {
        self.min.x <= pos.x && pos.x <= self.max.x && self.min.y <= pos.y && pos.y <= self.max.y
    }

    const fn contains_rect(&self, other: Self) -> bool {
        self.contains(other.min) && self.contains(other.max)
    }
}

const TEMPLATE: &str = r##"
if status == "connecting"
    alignment [align: "center"]
        text "Connecting to "
            span [foreground: #6441a5] "Twitch"
            span "."

else if status == "reconnecting"
    alignment [align: "center"]
        text "Reconnecting to "
            span [foreground: #6441a5] "Twitch"
            span "... "
            span "(our user: "
            span [foreground: our_user.color] our_user.name
            span ")"

else if status == "invalid_auth"
    alignment [align: "center"]
        text "Invalid Authentication (check your "
            span [foreground: #f00] "name"
            span " and "
            span [foreground: #f00] "oauth"
            span ")"

else if status == "on_no_channels"
    vstack
        expand
            alignment [align: "center"]
                vstack [height: 3]
                    text "Connected to "
                        span [foreground: #6441a5] "Twitch"
                        span " as "
                        span [foreground: our_user.color] our_user.name
                        span " ("
                        span our_user.user_id
                        span ")"
                    text " "
                    text [text-align: "center"] "Type /join "
                        span [text-align: "center", bold: true, italics: true] "#channel"
                        span [text-align: "center"] " to join a channel"

        hstack [background: #222]
            text input
                span [foreground: #0aa] "█"
            spacer

else
    vstack
        expand
            vstack
                for msg in output
                    hstack
                        text
                            span [foreground: msg.sender.color] msg.sender.name
                            span " "
                            span msg.data
                        spacer


        hstack [background: #000]
            for channel in channels
                hstack
                    tab [foreground: channel.status] channel.name
                    text " "
            spacer

        hstack [background: #222]
            text input
                span [foreground: #0aa] "█"
            spacer
"##;

fn main() -> anyhow::Result<()> {
    simple_env_load::load_env_from([".secrets.env", ".dev.env"]);

    let config = Config::from_env()?;

    anathema::core::Factory::register("tab", TabFactory)?;

    let (req_tx, req_rx) = smol::channel::unbounded();
    let (resp_tx, resp_rx) = smol::channel::unbounded();

    let handle = std::thread::spawn(move || connect(config, req_rx, resp_tx));

    let stop = Arc::new(AtomicBool::new(false));
    let root_view = RootView {
        state: RootState {
            status: StateValue::default(),
            our_user: StateValue::default(),
            input: StateValue::default(),
            channels: List::empty(),
            output: List::empty(),
        },
        tabs: Tabs::default(),
        feed: resp_rx,
        send: req_tx.clone(),
        stop: Arc::clone(&stop),
    };

    let mut templates = anathema::vm::Templates::new(TEMPLATE.to_string(), root_view);
    let templates = templates.compile()?;

    let mut runtime = anathema::runtime::Runtime::new(&templates)?;
    runtime.enable_alt_screen = false;
    runtime.stop = Some(Box::new(move || stop.load(Ordering::SeqCst)));

    runtime.run()?;

    // lets ensure the thread ends, we don't care if we can't send to it
    let _ = req_tx.send_blocking(Request::Disconnect { reconnect: false });

    handle.join().unwrap()
}
