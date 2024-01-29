use std::{
    collections::HashMap,
    io,
    mem::MaybeUninit,
    net::{IpAddr, SocketAddr},
    str::FromStr,
    time::Duration,
};

use clap::Parser;
use config::Args;
use drill_proto::{AuthMode, Event};
use futures_util::SinkExt;
use httparse::Status;
use libc::{c_int, sighandler_t, signal, SIGINT, SIGTERM};
use state::State;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    runtime,
    sync::mpsc::{unbounded_channel, UnboundedReceiver},
    time::sleep,
};
use tokio_websockets::Message as WebsocketMessage;

use crate::{auth::AuthState, event::StreamEvent, util::random_token};

mod auth;
mod config;
mod event;
mod state;
mod util;

const MISSING_HOST_HEADER: &[u8] = b"HTTP/1.1 400\r\nContent-Length: 19\r\n\r\nMissing Host header";
const FAILED_TO_PARSE: &[u8] = b"HTTP/1.1 400\r\nContent-Length: 23\r\n\r\nFailed to parse request";
const HOST_NOT_UTF8: &[u8] =
    b"HTTP/1.1 400\r\nContent-Length: 27\r\n\r\nHost header is invalid UTF8";
const HOST_NO_SUBDOMAIN: &[u8] =
    b"HTTP/1.1 400\r\nContent-Length: 28\r\n\r\nHost header has no subdomain";
const NOT_FOUND: &[u8] = b"HTTP/1.1 404\r\nContent-Length: 0\r\n\r\n";
const SERVICE_ALIVE: &[u8] =
    b"HTTP/1.1 200\r\nContent-Length: 19\r\n\r\nDrill service alive";

async fn handle_stream(state: State, mut stream: TcpStream, mut ip: IpAddr) -> io::Result<()> {
    // The first thing sent by a client MUST be a HTTP request - either to the
    // websocket server at the "go" subdomain or to some other subdomain.
    // We will peek the data to see what the Host header is set to.

    let mut bytes = vec![0; state.config.buffer_size];
    let path: Option<&str>;

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
                path = req.path;
                let host = req
                    .headers
                    .iter()
                    .find(|header| header.name.eq_ignore_ascii_case("host"));

                let forwarded_for = req
                    .headers
                    .iter()
                    .find(|header| header.name.eq_ignore_ascii_case("x-forwarded-for"));

                if let Some(forwarded_for) = forwarded_for {
                    if let Some(forwarded_ip) =
                        std::str::from_utf8(forwarded_for.value).ok().and_then(|f| {
                            IpAddr::from_str(f.split(',').next().unwrap_or_default()).ok()
                        })
                    {
                        ip = forwarded_ip;
                    };
                }

                log::debug!("New client connection from {ip}");

                if let Some(host) = host {
                    if let Ok(host_str) = std::str::from_utf8(host.value) {
                        break host_str;
                    }

                    log::debug!("Client sent request with invalid UTF8 in host header");
                    stream.write_all(HOST_NOT_UTF8).await?;
                    return Ok(());
                }

                log::debug!("Client sent request without host header");
                stream.write_all(MISSING_HOST_HEADER).await?;
                return Ok(());
            }
            Ok(Status::Partial) => {}
            Err(e) => {
                log::debug!("Failed to parse client request: {e}");
                stream.write_all(FAILED_TO_PARSE).await?;
                return Ok(());
            }
        }
    };

    // Clients may connect either to a subdomain (i.e. require to be tunneled) or to
    // the websocket server.
    if host == state.config.websocket_host {
        if path == Some("/livez") {
            stream.write_all(SERVICE_ALIVE).await?;
            return Ok(());
        }
        if let Err(e) = handle_client(state, stream, ip).await {
            log::debug!("Error in websocket connection: {e}");
        };
    } else {
        let Some((subdomain, _)) = host.split_once('.') else {
            stream.write_all(HOST_NO_SUBDOMAIN).await?;
            return Ok(());
        };

        let Some(sender) = state.get_event_sender(subdomain) else {
            stream.write_all(NOT_FOUND).await?;
            return Ok(());
        };

        let connection_id = random_token();
        let (tx, mut rx) = unbounded_channel();

        if sender
            .send(StreamEvent::new(connection_id.clone(), tx))
            .is_err()
        {
            return Ok(());
        };

        loop {
            tokio::select! {
                res = stream.read(&mut bytes) => {
                    if let Ok(n) = res {
                        if n == 0 {
                            break;
                        }

                        if sender
                            .send(StreamEvent::data(connection_id.clone(), bytes[..n].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    } else {
                        break;
                    }
                },
                Some(evt) = rx.recv() => {
                    if let StreamEvent::Data { data, ..} = evt {
                        if stream.write_all(&data).await.is_err() {
                            break;
                        }
                    } else {
                        break
                    }
                },
                else => break,
            }
        }

        let _ = sender.send(StreamEvent::close(connection_id));
    }

    Ok(())
}

async fn handle_client(
    state: State,
    stream: TcpStream,
    ip: IpAddr,
) -> Result<(), tokio_websockets::Error> {
    let mut ws = tokio_websockets::ServerBuilder::new()
        .fail_fast_on_invalid_utf8(false)
        .accept(stream)
        .await?;

    log::info!("Websocket connection with client from {ip} established");

    let auth_mode: AuthMode = state.config.auth.into();
    let mut auth_state = AuthState::from(auth_mode);
    let mut stream_receiver: MaybeUninit<UnboundedReceiver<StreamEvent>> = MaybeUninit::uninit();
    let mut stream_senders = HashMap::new();
    let mut actual_subdomain = String::new();

    let mut i_am_waiting_for_pong = false;

    // Send the initial HELLO packet
    ws.send(WebsocketMessage::binary(
        Event::hello(auth_mode, &state.config.description).serialize(),
    ))
    .await?;

    loop {
        let sleep = sleep(Duration::from_secs(15));
        tokio::pin!(sleep);

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
            res = unsafe { stream_receiver.assume_init_mut() }.recv(), if auth_state == AuthState::Proxying => {
                match res {
                    Some(evt) => {
                        match evt {
                            StreamEvent::New { id, sender } => {
                                stream_senders.insert(id, sender);
                            },
                            StreamEvent::Close { id } => {
                                stream_senders.remove(&id);
                                if ws.send(WebsocketMessage::binary(Event::closed(&id).serialize())).await.is_err() {
                                    break;
                                };
                            },
                            StreamEvent::Data { id, data } => {
                                if ws.send(WebsocketMessage::binary(Event::data(&id, &data).serialize())).await.is_err() {
                                    break;
                                };
                            }
                        }

                        continue;
                    },
                    None => break,
                }
            }
            _ = &mut sleep => {
                if i_am_waiting_for_pong {
                    log::warn!("Did not receive a pong within 60 seconds, disconnecting client at {ip}");
                    break;
                }

                log::trace!("Sending ping to websocket");

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
                    #[cfg(feature = "ao")]
                    Event::AoAuth { character_name } if auth_state == AuthState::AwaitingAoAuth => {
                        log::debug!("{character_name} wants to authenticate with AO");

                        // Make botname -> Botname
                        let mut bot_name = character_name.to_string();
                        if let Some(c) = bot_name.get_mut(0..1) {
                            c.make_ascii_uppercase();
                        }

                        let token = random_token();

                        auth_state = AuthState::AwaitingToken;
                        state.auth_provider.expect(bot_name, token);

                        ws.send(WebsocketMessage::binary(
                            Event::token_in_ao_tell(state.config.ao_character.as_ref().unwrap())
                                .serialize(),
                        ))
                        .await?;
                    }
                    Event::PresentToken {
                        token,
                        desired_subdomain,
                    } if AuthState::AwaitingToken == auth_state => {
                        if let Some(subdomain) = state
                            .auth_provider
                            .verify(token, desired_subdomain, state.config.subdomain_strategy)
                            .await
                        {
                            log::info!("{ip} has authenticated, wanted {desired_subdomain}, got {subdomain}");

                            actual_subdomain = subdomain;

                            let public_url =
                                format!("https://{actual_subdomain}.{}", state.config.domain);

                            let (event_tx, event_rx) = unbounded_channel();

                            if !state.register_event_listener(actual_subdomain.clone(), event_tx) {
                                log::warn!("Rejecting {ip} due to capacity limit reached or subdomain in use");
                                let _ = ws
                                    .send(WebsocketMessage::binary(
                                        Event::out_of_capacity().serialize(),
                                    ))
                                    .await;
                                break;
                            };

                            stream_receiver.write(event_rx);

                            auth_state = AuthState::Proxying;

                            if ws
                                .send(WebsocketMessage::binary(
                                    Event::lets_go(&public_url).serialize(),
                                ))
                                .await
                                .is_err()
                            {
                                break;
                            };
                        } else {
                            log::debug!("{ip} has failed authentication");

                            ws.send(WebsocketMessage::binary(Event::auth_failed().serialize()))
                                .await?;
                            break;
                        }
                    }
                    Event::Data { id, data } if auth_state == AuthState::Proxying => {
                        if let Some(sender) = stream_senders.get(id) {
                            let _ = sender.send(StreamEvent::data(id.to_string(), data.to_vec()));
                        }
                    }
                    Event::Closed { id } if auth_state == AuthState::Proxying => {
                        if let Some(sender) = stream_senders.remove(id) {
                            let _ = sender.send(StreamEvent::close(id.to_string()));
                        }
                    }
                    _ => {
                        log::debug!("Received disallowed event from client at current auth stage");
                        let _ = ws
                            .send(WebsocketMessage::binary(
                                Event::disallowed_packet().serialize(),
                            ))
                            .await;
                        break;
                    }
                }
            } else {
                log::debug!("Failed to deserialize event from websocket");
                let _ = ws
                    .send(WebsocketMessage::binary(
                        Event::disallowed_packet().serialize(),
                    ))
                    .await;
                break;
            }
        }
    }

    log::info!("Cleaning up client {ip}");

    if !actual_subdomain.is_empty() {
        state.unregister_event_listener(&actual_subdomain);
    }

    for (id, sender) in stream_senders {
        let _ = sender.send(StreamEvent::close(id));
    }

    Ok(())
}

async fn run(args: Args) -> io::Result<()> {
    let state = State::new(args);

    let server_addr = SocketAddr::from(([0, 0, 0, 0], state.config.port));

    let tcp_server = TcpListener::bind(server_addr).await?;

    log::info!("Listening on {server_addr}");

    loop {
        let (stream, addr) = tcp_server.accept().await?;
        let ip = addr.ip();

        tokio::spawn(handle_stream(state.clone(), stream, ip));
    }
}

pub extern "C" fn handler(_: c_int) {
    std::process::exit(0);
}

unsafe fn set_os_handlers() {
    signal(SIGINT, handler as extern "C" fn(_) as sighandler_t);
    signal(SIGTERM, handler as extern "C" fn(_) as sighandler_t);
}

fn main() {
    let args = Args::parse();

    env_logger::builder().filter_level(args.log_level).init();

    unsafe { set_os_handlers() };

    if let Err(e) = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(args))
    {
        log::error!("Fatal I/O error: {e}");
    }
}
