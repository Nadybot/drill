#![feature(if_let_guard)]

use std::{
    collections::HashMap,
    env, io,
    mem::MaybeUninit,
    net::SocketAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use futures_util::SinkExt;
use httparse::Status;
use nadylib::{
    account::{AccountManager, AccountManagerHttpClient},
    models::{Channel, Message},
    packets::{ClientLookupPacket, LoginSelectPacket, MsgPrivatePacket},
    AOSocket, ReceivedPacket, SocketConfig,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    runtime,
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    time::sleep,
};
use tokio_websockets::Message as WebsocketMessage;
use uuid::Uuid;

const DEFAULT_PORT: u16 = 7777;
const WEBSOCKET_SUBDOMAIN: &str = "localhost";

const MISSING_HOST_HEADER: &[u8] = b"HTTP/1.1 400\r\nContent-Length: 19\r\n\r\nMissing Host header";
const FAILED_TO_PARSE: &[u8] = b"HTTP/1.1 400\r\nContent-Length: 23\r\n\r\nFailed to parse request";
const HOST_NOT_UTF8: &[u8] =
    b"HTTP/1.1 400\r\nContent-Length: 27\r\n\r\nHost header is invalid UTF8";
const HOST_NO_SUBDOMAIN: &[u8] =
    b"HTTP/1.1 400\r\nContent-Length: 28\r\n\r\nHost header has no subdomain";
const NOT_FOUND: &[u8] = b"HTTP/1.1 404\r\n\r\n";

#[derive(Debug)]
enum Event<'a> {
    ReserveSubdomain { subdomain: &'a str },
    PromptToken { sender: &'a str },
    ReplyToken { token: &'a str },
    LetsGo { public_url: &'a str },
    AuthFailed,
    Data { id: &'a str, data: &'a [u8] },
    Closed { id: &'a str },
}

enum StreamEvent {
    New {
        id: String,
        sender: UnboundedSender<StreamEvent>,
    },
    Data {
        id: String,
        data: Vec<u8>,
    },
    Close {
        id: String,
    },
}

enum ParseEventError {
    Eof,
    InvalidUtf8,
    OnlyOutgoing,
    UnknownType,
}

impl<'a> Event<'a> {
    fn deserialize(input: &'a [u8]) -> Result<Self, ParseEventError> {
        let event_type = input.first().ok_or(ParseEventError::Eof)?;

        match event_type {
            1 => {
                let subdomain = std::str::from_utf8(input.get(1..).ok_or(ParseEventError::Eof)?)
                    .map_err(|_| ParseEventError::InvalidUtf8)?;

                Ok(Self::ReserveSubdomain { subdomain })
            }
            3 => {
                let token = std::str::from_utf8(input.get(1..).ok_or(ParseEventError::Eof)?)
                    .map_err(|_| ParseEventError::InvalidUtf8)?;

                Ok(Self::ReplyToken { token })
            }
            6 => {
                let id = std::str::from_utf8(input.get(1..37).ok_or(ParseEventError::Eof)?)
                    .map_err(|_| ParseEventError::InvalidUtf8)?;
                let data = input.get(37..).ok_or(ParseEventError::Eof)?;

                Ok(Self::Data { id, data })
            }
            7 => {
                let id = std::str::from_utf8(input.get(1..37).ok_or(ParseEventError::Eof)?)
                    .map_err(|_| ParseEventError::InvalidUtf8)?;

                Ok(Self::Closed { id })
            }
            2 | 4 | 5 => Err(ParseEventError::OnlyOutgoing),
            _ => Err(ParseEventError::UnknownType),
        }
    }

    fn serialize(&self) -> Vec<u8> {
        match self {
            Self::PromptToken { sender } => {
                let mut serialized = Vec::with_capacity(1 + sender.len());
                serialized.push(2);
                serialized.extend_from_slice(sender.as_bytes());

                serialized
            }
            Self::LetsGo { public_url } => {
                let mut serialized = Vec::with_capacity(1 + public_url.len());
                serialized.push(4);
                serialized.extend_from_slice(public_url.as_bytes());

                serialized
            }
            Self::AuthFailed => {
                vec![5]
            }
            Self::Data { id, data } => {
                let mut serialized = Vec::with_capacity(37 + data.len());
                serialized.push(6);
                serialized.extend_from_slice(id.as_bytes());
                serialized.extend_from_slice(data);

                serialized
            }
            Self::Closed { id } => {
                let mut serialized = Vec::with_capacity(37);
                serialized.push(7);
                serialized.extend_from_slice(id.as_bytes());

                serialized
            }
            _ => unreachable!("not an outgoing event"),
        }
    }
}

#[derive(PartialEq)]
enum ClientState {
    WaitingForReservation,
    WaitingForTokenReply {
        expected_token: String,
        subdomain: String,
    },
    Proxying,
}

#[derive(Clone)]
struct State {
    send_tokens: UnboundedSender<(String, String)>,
    event_senders: Arc<RwLock<HashMap<String, UnboundedSender<StreamEvent>>>>,
    domain: String,
    character: String,
}

impl State {
    fn send_token_tell(&self, character: String, token: String) {
        let _ = self.send_tokens.send((character, token));
    }

    fn get_event_sender(&self, subdomain: &str) -> Option<UnboundedSender<StreamEvent>> {
        self.event_senders
            .read()
            .expect("RwLock poisoned")
            .get(subdomain)
            .cloned()
    }

    fn register_event_listener(&self, subdomain: String, sender: UnboundedSender<StreamEvent>) {
        self.event_senders
            .write()
            .expect("RwLock poisoned")
            .insert(subdomain, sender);
    }

    fn unregister_event_listener(&self, subdomain: &str) {
        self.event_senders
            .write()
            .expect("RwLock poisoned")
            .remove(subdomain);
    }
}

fn random_token() -> String {
    Uuid::new_v4().to_string()
}

async fn handle_stream(state: State, mut stream: TcpStream, addr: SocketAddr) -> io::Result<()> {
    log::info!("New client connection from {addr}");

    // The first thing sent by a client MUST be a HTTP request - either to the
    // websocket server at the "go" subdomain or to some other subdomain.
    // We will peek the data to see what the Host header is set to.

    // More than 4KB of headers would be very weird.
    let mut bytes = vec![0; 4096];

    let host = loop {
        let n = stream.peek(&mut bytes).await?;

        if n == 0 {
            return Ok(());
        }

        let mut headers: [MaybeUninit<httparse::Header<'_>>; 100] =
            unsafe { MaybeUninit::uninit().assume_init() };
        let mut req = httparse::Request::new(&mut []);

        match req.parse_with_uninit_headers(&bytes, &mut headers) {
            Ok(Status::Complete(_)) => {
                let host = req
                    .headers
                    .iter()
                    .find(|header| header.name.eq_ignore_ascii_case("host"));

                if let Some(host) = host {
                    if let Ok(host_str) = std::str::from_utf8(host.value) {
                        break host_str;
                    }

                    log::error!("Client sent request with invalid UTF8 in host header");
                    stream.write_all(HOST_NOT_UTF8).await?;
                    return Ok(());
                }

                log::error!("Client sent request without host header");
                stream.write_all(MISSING_HOST_HEADER).await?;
                return Ok(());
            }
            Ok(Status::Partial) => {}
            Err(e) => {
                log::error!("Failed to parse client request: {e}");
                stream.write_all(FAILED_TO_PARSE).await?;
                return Ok(());
            }
        }
    };

    let Some((subdomain, _)) = host.split_once('.') else {
        stream.write_all(HOST_NO_SUBDOMAIN).await?;
        return Ok(());
    };

    log::info!("{subdomain}");

    if subdomain == WEBSOCKET_SUBDOMAIN {
        if let Err(e) = handle_client(state, stream, addr).await {
            log::error!("Error in websocket connection: {e}");
        };
    } else {
        let Some(sender) = state.get_event_sender(subdomain) else {
            stream.write_all(NOT_FOUND).await?;
            return Ok(());
        };

        let connection_id = random_token();
        let (tx, mut rx) = unbounded_channel();

        if sender
            .send(StreamEvent::New {
                id: connection_id.clone(),
                sender: tx,
            })
            .is_err()
        {
            return Ok(());
        };

        let mut buffer = [0; 4096];

        loop {
            tokio::select! {
                res = stream.read(&mut buffer) => {
                    if let Ok(n) = res {
                        if sender
                            .send(StreamEvent::Data {
                                id: connection_id.clone(),
                                data: buffer[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    } else {
                        let _ = sender.send(StreamEvent::Close { id: connection_id });
                        break;
                    }
                },
                Some(evt) = rx.recv() => {
                    match evt {
                        StreamEvent::Data { data, .. } => {
                            if stream.write_all(&data).await.is_err() {
                                let _ = sender.send(StreamEvent::Close { id: connection_id });
                                break;
                            }
                        },
                        StreamEvent::Close { .. } => {
                            break;
                        },
                        StreamEvent::New { .. } => unreachable!(),
                    }
                },
                else => break,
            }
        }
    }

    Ok(())
}

async fn handle_client(
    state: State,
    stream: TcpStream,
    addr: SocketAddr,
) -> Result<(), tokio_websockets::Error> {
    let mut ws = tokio_websockets::ServerBuilder::new()
        .fail_fast_on_invalid_utf8(false)
        .accept(stream)
        .await?;

    log::info!("Websocket connection with client from {addr} established");

    let mut client_state = ClientState::WaitingForReservation;
    let mut event_receiver: MaybeUninit<UnboundedReceiver<StreamEvent>> = MaybeUninit::uninit();
    let mut event_senders = HashMap::new();
    let mut actual_subdomain = String::new();

    let mut i_am_waiting_for_pong = false;

    loop {
        let msg = tokio::select! {
            res = ws.next() => {
                match res {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => {
                        log::error!("Websocket stream error: {e}");
                        break;
                    },
                    None => {
                        break;
                    }
                }
            },
            res = unsafe { event_receiver.assume_init_mut() }.recv(), if client_state == ClientState::Proxying => {
                match res {
                    Some(evt) => {
                        match evt {
                            StreamEvent::New { id, sender } => {
                                event_senders.insert(id, sender);
                            },
                            StreamEvent::Close { id } => {
                                event_senders.remove(&id);
                                if ws.send(WebsocketMessage::binary(Event::Closed { id: &id }.serialize())).await.is_err() {
                                    break;
                                };
                            },
                            StreamEvent::Data { id, data } => {
                                if ws.send(WebsocketMessage::binary(Event::Data { id: &id, data: &data }.serialize())).await.is_err() {
                                    break;
                                };
                            }
                        }

                        continue;
                    },
                    None => break,
                }
            }
            _ = sleep(Duration::from_secs(60)) => {
                if i_am_waiting_for_pong {
                    log::warn!("Did not receive a pong within 60 seconds, disconnecting client");
                    break;
                }

                if ws.send(WebsocketMessage::ping("")).await.is_err() {
                    break;
                };
                i_am_waiting_for_pong = true;

                continue;
            },
        };

        if msg.is_pong() {
            i_am_waiting_for_pong = false;
        }

        if msg.is_binary() || msg.is_text() {
            let payload = msg.into_data();

            if let Ok(evt) = Event::deserialize(&payload) {
                match evt {
                    Event::ReserveSubdomain { subdomain }
                        if client_state == ClientState::WaitingForReservation =>
                    {
                        log::info!("{addr} wants to reserve {subdomain}");

                        // Make botname -> Botname
                        let mut bot_name = subdomain.to_string();
                        if let Some(c) = bot_name.get_mut(0..1) {
                            c.make_ascii_uppercase();
                        }

                        let token = random_token();

                        client_state = ClientState::WaitingForTokenReply { expected_token: token.clone(), subdomain: subdomain.to_ascii_lowercase() };
                        state.send_token_tell(bot_name, token);

                        ws.send(WebsocketMessage::binary(Event::PromptToken { sender: &state.character }.serialize())).await?;
                    }
                    Event::ReplyToken { token }
                        if let ClientState::WaitingForTokenReply { expected_token, subdomain } = &client_state => {
                            if token == expected_token {
                                log::info!("{addr} has authenticated for {subdomain}");

                                actual_subdomain = subdomain.to_string();
                                let public_url = format!("https://{subdomain}.{}", state.domain);

                                let (event_tx, event_rx) = unbounded_channel();
                                state.register_event_listener(subdomain.to_string(), event_tx);
                                event_receiver.write(event_rx);

                                client_state = ClientState::Proxying;

                                if ws.send(WebsocketMessage::binary(Event::LetsGo { public_url: &public_url }.serialize())).await.is_err() {
                                    break;
                                };
                            } else {
                                log::info!("{addr} has failed authentication for {subdomain}");

                                ws.send(WebsocketMessage::binary(Event::AuthFailed.serialize())).await?;
                            }
                        }
                    Event::Data { id, data } if client_state == ClientState::Proxying => {
                        if let Some(sender) = event_senders.get(id) {
                            let _ = sender.send(StreamEvent::Data { id: id.to_string(), data: data.to_vec() });
                        }
                    }
                    Event::Closed { id } if client_state == ClientState::Proxying => {
                        if let Some(sender) = event_senders.remove(id) {
                            let _ = sender.send(StreamEvent::Close { id: id.to_string() });
                        }
                    }
                    _ => {
                        log::error!("Received disallowed event at client auth stage");
                        break;
                    }
                }
            } else {
                log::error!("Failed to deserialize event from websocket");
            }
        }
    }

    if !actual_subdomain.is_empty() {
        state.unregister_event_listener(&actual_subdomain);
    }

    for (id, sender) in event_senders {
        let _ = sender.send(StreamEvent::Close { id });
    }

    Ok(())
}

async fn ao_bot_inner(
    mut sock: AOSocket,
    username: String,
    password: String,
    character: String,
    token_rx: &mut UnboundedReceiver<(String, String)>,
) -> nadylib::Result<()> {
    let mut pending_tells = HashMap::new();

    loop {
        let packet = tokio::select! {
            Ok(packet) = sock.read_packet() => packet,
            Some((character_name, token)) = token_rx.recv() => {
                pending_tells.insert(character_name.clone(), token);

                let pack = ClientLookupPacket {
                    character_name,
                };
                sock.send(pack).await?;

                continue;
            }
            else => break,
        };

        match packet {
            ReceivedPacket::LoginSeed(s) => {
                sock.login(&username, &password, &s.login_seed).await?;
            }
            ReceivedPacket::LoginCharlist(c) => {
                let character = c.characters.iter().find(|i| i.name == character).unwrap();
                let pack = LoginSelectPacket {
                    character_id: character.id,
                };
                sock.send(pack).await?;
            }
            ReceivedPacket::LoginOk => log::info!("AO bot logged in successfully"),
            ReceivedPacket::LoginError(e) => {
                log::error!("AO bot failed to log in due to {}", e.message);

                if e.message.contains("Account system denies login") {
                    if let Ok(unfreeze_result) =
                        AccountManager::from_client(AccountManagerHttpClient::new(true))
                            .username(username.clone())
                            .password(password.clone())
                            .reactivate()
                            .await
                    {
                        if unfreeze_result.should_continue() {
                            log::info!("Unfroze account, waiting 5 seconds before reconnecting");
                            sleep(Duration::from_secs(5)).await;
                            sock.reconnect().await?;
                            continue;
                        }
                    }
                }

                break;
            }
            ReceivedPacket::ClientLookup(c) => {
                if let Some(token) = pending_tells.remove(&c.character_name) {
                    if c.exists {
                        let pack = MsgPrivatePacket {
                            message: Message {
                                sender: None,
                                channel: Channel::Tell(c.character_id),
                                text: format!("!drill {token}"),
                                send_tag: String::from("\u{0}"),
                            },
                        };

                        sock.send(pack).await?;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

async fn ao_bot(
    username: String,
    password: String,
    character: String,
    mut token_rx: UnboundedReceiver<(String, String)>,
) {
    loop {
        let Ok(sock) = AOSocket::connect("chat.d1.funcom.com:7105", &SocketConfig::default()).await else {
            log::warn!("Failed to connect to AO chat server, retrying in 10s");
            sleep(Duration::from_secs(10)).await;
            continue;
        };

        if let Err(e) = ao_bot_inner(
            sock,
            username.clone(),
            password.clone(),
            character.clone(),
            &mut token_rx,
        )
        .await
        {
            log::error!("AO bot errored: {e:?}, reconnecting in 10s");
            sleep(Duration::from_secs(10)).await;
        } else {
            break;
        }
    }
}

async fn run() -> io::Result<()> {
    let username = env::var("USERNAME").expect("USERNAME is not set");
    let password = env::var("PASSWORD").expect("PASSWORD is not set");
    let character = env::var("CHARACTER").expect("CHARACTER is not set");

    let (token_tx, token_rx) = unbounded_channel();

    tokio::spawn(ao_bot(username, password, character.clone(), token_rx));

    let domain = env::var("DOMAIN").unwrap_or_else(|_| String::from("nadybotters.org"));

    let state = State {
        send_tokens: token_tx,
        event_senders: Arc::new(RwLock::new(HashMap::new())),
        domain,
        character,
    };

    let port = env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    let server_addr = SocketAddr::from(([0, 0, 0, 0], port));

    let tcp_server = TcpListener::bind(server_addr).await?;

    log::info!("Listening on {server_addr}");

    loop {
        let (stream, addr) = tcp_server.accept().await?;

        tokio::spawn(handle_stream(state.clone(), stream, addr));
    }
}

fn main() {
    env_logger::init();

    if let Err(e) = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run())
    {
        log::error!("Fatal I/O error: {e}");
    }
}
