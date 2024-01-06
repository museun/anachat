use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    task::Poll,
};

use smol::{
    channel::{Receiver, Sender},
    future::FutureExt,
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
};
use twitch_message::{encode::Encode, messages::MessageKind};

pub enum Request {
    JoinChannel { channel: String },
    PartChannel { channel: String },
    SendMesage { channel: String, data: String },
    Disconnect { reconnect: bool },
}

#[derive(Debug)]
pub enum Response {
    Connecting,
    Connected { user: User },
    JoinChannel { channel: String },
    PartChannel { channel: String },
    Message { message: Message },
    Disconnected,
    AuthenticationFailed,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub sender: User,
    pub channel: String,
    pub data: String,
}

#[derive(Clone, Debug)]
pub struct User {
    pub color: twitch_message::Color,
    pub user_id: String,
    pub name: String,
}

pub fn connect(
    config: Config,
    req: Receiver<Request>,
    resp: Sender<Response>,
) -> anyhow::Result<()> {
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

pub struct Config {
    pub name: String,
    pub oauth: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
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

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
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
