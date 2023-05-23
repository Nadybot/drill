use std::{collections::HashSet, process::exit, sync::Arc};

use dashmap::DashMap;
#[cfg(feature = "ao")]
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::mpsc::UnboundedSender;

#[cfg(feature = "dynamic")]
use crate::auth::dynamic::DynamicAuthProvider;
#[cfg(feature = "ao")]
use crate::{
    auth::ao::{ao_bot, AoAuthProvider},
    config::AoCredentials,
};
use crate::{
    auth::AuthProvider,
    config::{Args, AuthBackend},
    StreamEvent,
};

#[derive(Clone)]
pub struct State {
    pub auth_provider: Arc<AuthProvider>,
    event_senders: Arc<DashMap<String, UnboundedSender<StreamEvent>>>,
    pub config: Arc<Args>,
}

impl State {
    pub fn new(config: Args) -> Self {
        let auth_provider = match config.auth {
            #[cfg(feature = "ao")]
            AuthBackend::Ao => {
                let Ok(credentials) = AoCredentials::try_from(&config) else {
                    log::error!("AO credentials incomplete.");
                    exit(1);
                };

                let (tx, rx) = unbounded_channel();
                tokio::spawn(ao_bot(credentials, rx));

                AuthProvider::Ao(AoAuthProvider::new(tx))
            }
            AuthBackend::Anonymous => AuthProvider::Anonymous,
            AuthBackend::Private => {
                let Some(token_file_path) = &config.token_file else {
                    log::error!("Private auth backend requires token file to be specified");
                    exit(1);
                };

                let Ok(tokens) = std::fs::read_to_string(token_file_path) else {
                    log::error!("Failed to open token file");
                    exit(1);
                };

                let tokens: HashSet<_> = tokens.split_whitespace().map(std::string::ToString::to_string).collect();

                AuthProvider::Private(tokens)
            }
            #[cfg(feature = "dynamic")]
            AuthBackend::Dynamic => {
                let Some(auth_backend) = config.auth_backend.clone() else {
                    log::error!("Dynamic auth backend requires auth backend URL to be specified");
                    exit(1);
                };

                AuthProvider::Dynamic(DynamicAuthProvider::new(auth_backend))
            }
        };

        Self {
            auth_provider: Arc::new(auth_provider),
            event_senders: Arc::new(DashMap::new()),
            config: Arc::new(config),
        }
    }

    pub fn get_event_sender(&self, subdomain: &str) -> Option<UnboundedSender<StreamEvent>> {
        self.event_senders.get(subdomain).as_deref().cloned()
    }

    pub fn register_event_listener(
        &self,
        subdomain: String,
        sender: UnboundedSender<StreamEvent>,
    ) -> bool {
        if self.event_senders.len() >= self.config.max_clients
            || self.event_senders.contains_key(&subdomain)
        {
            false
        } else {
            self.event_senders.insert(subdomain, sender);
            true
        }
    }

    pub fn unregister_event_listener(&self, subdomain: &str) {
        self.event_senders.remove(subdomain);
    }
}
