use std::{collections::HashMap, time::Duration};

use dashmap::DashMap;
use nadylib::{
    account::{AccountManager, AccountManagerHttpClient},
    models::{Channel, Message},
    packets::{ClientLookupPacket, LoginSelectPacket, MsgPrivatePacket},
    AOSocket, ReceivedPacket, SocketConfig,
};
use tokio::{
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
    time::sleep,
};

use crate::{
    config::{AoCredentials, SubdomainStrategy},
    util::random_subdomain,
};

const RETRY_INTERVAL: Duration = Duration::from_secs(10);

pub struct TokenTell {
    character: String,
    token: String,
}

async fn ao_bot_inner(
    mut sock: AOSocket,
    credentials: AoCredentials,
    token_rx: &mut UnboundedReceiver<TokenTell>,
) -> nadylib::Result<()> {
    let mut pending_tells = HashMap::new();

    loop {
        let packet = tokio::select! {
            Ok(packet) = sock.read_packet() => packet,
            Some(token_tell) = token_rx.recv() => {
                pending_tells.insert(token_tell.character.clone(), token_tell.token);

                let pack = ClientLookupPacket {
                    character_name:  token_tell.character,
                };
                sock.send(pack).await?;

                continue;
            }
            else => break,
        };

        match packet {
            ReceivedPacket::LoginSeed(s) => {
                sock.login(&credentials.username, &credentials.password, &s.login_seed)
                    .await?;
            }
            ReceivedPacket::LoginCharlist(c) => {
                let Some(character) = c
                    .characters
                    .iter()
                    .find(|i| i.name == credentials.character) else {
                        log::error!("Character {} is not on account {}", credentials.character, credentials.username);
                        break;
                    };

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
                            .username(credentials.username.clone())
                            .password(credentials.password.clone())
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

                        log::info!(
                            "Client lookup complete, sending in-game tell to {}",
                            c.character_name
                        );

                        sock.send(pack).await?;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

pub async fn ao_bot(credentials: AoCredentials, mut token_rx: UnboundedReceiver<TokenTell>) {
    loop {
        let Ok(sock) = AOSocket::connect("chat.d1.funcom.com:7105", &SocketConfig::default()).await else {
            log::warn!("Failed to connect to AO chat server, retrying");
            sleep(RETRY_INTERVAL).await;
            continue;
        };

        if let Err(e) = ao_bot_inner(sock, credentials.clone(), &mut token_rx).await {
            log::error!("AO bot errored: {e:?}, reconnecting");
            sleep(RETRY_INTERVAL).await;
        } else {
            break;
        }
    }
}

#[derive(Debug)]
pub struct AoAuthProvider {
    // Mapping of token to character name
    valid_tokens: DashMap<String, String>,
    tell_sender: UnboundedSender<TokenTell>,
}

impl AoAuthProvider {
    pub fn new(tell_sender: UnboundedSender<TokenTell>) -> Self {
        Self {
            valid_tokens: DashMap::new(),
            tell_sender,
        }
    }

    /// TODO: Clean these up after a bit or when client disconnects...
    pub fn expect(&self, character: String, token: String) {
        self.valid_tokens.insert(token.clone(), character.clone());
        let _ = self.tell_sender.send(TokenTell { character, token });
    }

    pub fn verify(
        &self,
        token: &str,
        desired_subdomain: &str,
        strategy: SubdomainStrategy,
    ) -> Option<String> {
        if let Some((_, character_name)) = self.valid_tokens.remove(token) {
            match strategy {
                SubdomainStrategy::AoCharacter => Some(character_name.to_lowercase()),
                SubdomainStrategy::ClientChoice => Some(desired_subdomain.to_string()),
                SubdomainStrategy::Random => Some(random_subdomain()),
            }
        } else {
            None
        }
    }
}
