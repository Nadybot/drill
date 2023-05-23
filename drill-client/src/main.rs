use std::{collections::HashMap, iter::repeat, net::SocketAddr, str::FromStr};

use clap::Parser;
use drill_proto::{AuthMode, Event};
use futures_util::SinkExt;
use http::{
    uri::{PathAndQuery, Scheme},
    Uri,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        tcp::{OwnedReadHalf, OwnedWriteHalf},
        TcpStream,
    },
    runtime,
    sync::mpsc::{unbounded_channel, UnboundedSender},
};
use tokio_websockets::Message;

mod args;

const GATEWAY_ERROR: &[u8] = b"HTTP/1.1 502\r\nContent-Length: 0\r\n\r\n";

#[derive(Debug)]
enum Error {
    Websocket(tokio_websockets::Error),
    UnexpectedClose,
    Proto(drill_proto::ParseEventError),
    ExpectedHello,
}

impl From<tokio_websockets::Error> for Error {
    fn from(value: tokio_websockets::Error) -> Self {
        Self::Websocket(value)
    }
}

impl From<drill_proto::ParseEventError> for Error {
    fn from(value: drill_proto::ParseEventError) -> Self {
        Self::Proto(value)
    }
}

async fn run(mut args: args::Args) -> Result<(), Error> {
    log::info!("Connecting to {}", args.drill);

    if args.drill.scheme().is_none() {
        let mut parts = args.drill.into_parts();
        parts.scheme = Some(Scheme::from_str("wss").unwrap());
        parts.path_and_query = Some(PathAndQuery::from_static("/"));
        args.drill = Uri::from_parts(parts).unwrap();
    }

    let mut ws = tokio_websockets::ClientBuilder::from_uri(args.drill)
        .fail_fast_on_invalid_utf8(false)
        .connect()
        .await?;

    // Receive a Hello message
    let msg = ws.next().await.ok_or(Error::UnexpectedClose)??;
    let hello = Event::deserialize(&msg.as_data())?;

    let token = match hello {
        Event::Hello {
            auth_mode,
            description,
            ..
        } => {
            log::info!("Server uses {auth_mode:?} authentication");
            log::info!("Server description: {description}");

            match auth_mode {
                AuthMode::AoTell => {
                    log::error!("AoTell authentication is unsupported by this client");
                    return Ok(());
                }
                AuthMode::Anonymous => args.token.unwrap_or_else(|| repeat("A").take(36).collect()),
                AuthMode::StaticToken => {
                    if let Some(token) = args.token {
                        token
                    } else {
                        log::error!("Server auth mode requires setting a token via --token");
                        return Ok(());
                    }
                }
            }
        }
        _ => return Err(Error::ExpectedHello),
    };

    ws.send(Message::binary(
        Event::present_token(&token, &args.subdomain).serialize(),
    ))
    .await?;

    let tcp_addr = SocketAddr::from(([127, 0, 0, 1], args.port));

    let (msg_tx, mut msg_rx) = unbounded_channel();
    let mut connections: HashMap<String, OwnedWriteHalf> = HashMap::new();

    loop {
        tokio::select! {
            Some(Ok(msg)) = ws.next() => {
                let event = if msg.is_binary() || msg.is_text() {
                    Event::deserialize(msg.as_data())?
                } else {
                    continue;
                };

                match event {
                    Event::LetsGo { public_url } => {
                        log::info!("Now live at {public_url}!");
                    }
                    Event::AuthFailed => {
                        log::error!("Authentication failed");
                        break;
                    }
                    Event::OutOfCapacity => {
                        log::error!("Server is out of capacity. Try again later!");
                        break;
                    }
                    Event::Data { id, data } => {
                        if let Some(conn) = connections.get_mut(id) {
                            if conn.write_all(data).await.is_err() {
                                connections.remove(id);
                                ws.send(Message::binary(Event::closed(id).serialize())).await?;
                            };
                        } else {
                            if let Ok(mut stream) = TcpStream::connect(tcp_addr).await {
                                if stream.write_all(data).await.is_err() {
                                    ws.send(Message::binary(Event::closed(id).serialize())).await?;
                                } else {
                                    let (read, write) = stream.into_split();
                                    connections.insert(id.to_string(), write);
                                    tokio::spawn(proxy_read(read, id.to_string(), msg_tx.clone()));
                                }
                            } else {
                                ws.send(Message::binary(Event::data(id, GATEWAY_ERROR).serialize())).await?;
                                ws.send(Message::binary(Event::closed(id).serialize())).await?;
                            };
                        }
                    }
                    Event::Closed { id } => {
                        if let Some(mut conn) = connections.remove(id) {
                            let _ = conn.shutdown().await;
                        }
                    }
                    _ => {
                        log::error!("Protocol error");
                        break;
                    }
                }
            },
            Some(msg) = msg_rx.recv() => {
                ws.send(msg).await?;
                continue;
            }
            else => break,
        };
    }

    Ok(())
}

async fn proxy_read(mut read: OwnedReadHalf, id: String, msg_tx: UnboundedSender<Message>) {
    let mut buffer = [0; 4096];

    loop {
        let evt = match read.read(&mut buffer).await {
            Ok(n) if n == 0 => Event::closed(&id),
            Ok(n) => Event::data(&id, &buffer[..n]),
            Err(_) => Event::closed(&id),
        };

        let should_stop = matches!(evt, Event::Closed { .. });

        if msg_tx.send(Message::binary(evt.serialize())).is_err() {
            break;
        };

        if should_stop {
            break;
        }
    }
}

fn main() {
    let args = args::Args::parse();

    env_logger::builder().filter_level(args.log_level).init();

    if let Err(e) = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(args))
    {
        log::error!("Fatal error: {e:?}");
    }
}
