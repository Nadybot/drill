use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use drill_proto::AuthMode;
use log::LevelFilter;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Log level.
    #[arg(long, default_value_t = LevelFilter::Info)]
    pub log_level: LevelFilter,

    /// Maximum parallel connected clients.
    #[arg(long, default_value_t = usize::MAX)]
    pub max_clients: usize,

    /// Username of the AO account to send tells from.
    #[cfg(feature = "ao")]
    #[arg(long)]
    pub ao_username: Option<String>,

    /// Password of the AO account to send tells from.
    #[cfg(feature = "ao")]
    #[arg(long)]
    pub ao_password: Option<String>,

    /// Character name on the AO account to send tells from.
    #[cfg(feature = "ao")]
    #[arg(long)]
    pub ao_character: Option<String>,

    /// Authentication backend.
    #[arg(long)]
    pub auth: AuthBackend,

    /// Strategy to choose subdomains for clients.
    #[arg(long)]
    pub subdomain_strategy: SubdomainStrategy,

    /// For "private" authentication backend, path to file with
    /// whitespace-seperated tokens.
    #[arg(long)]
    pub token_file: Option<PathBuf>,

    /// For "dynamic" authentication backend, URL of authentication API.
    #[cfg(feature = "dynamic")]
    #[arg(long)]
    pub auth_backend: Option<hyper::Uri>,

    /// Hostname for the websocket server, e.g. "connect.mydomain.com".
    #[arg(long)]
    pub websocket_host: String,

    /// Domain that subdomains can be handed out for, e.g. "mydomain.com".
    #[arg(long)]
    pub domain: String,

    /// Description of this instance, clients can display this to users.
    #[arg(long)]
    pub description: String,

    /// Port to run on.
    #[arg(long, short)]
    pub port: u16,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SubdomainStrategy {
    #[cfg(feature = "ao")]
    AoCharacter,
    ClientChoice,
    Random,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum AuthBackend {
    #[cfg(feature = "ao")]
    Ao,
    Anonymous,
    Private,
    #[cfg(feature = "dynamic")]
    Dynamic,
}

impl From<AuthBackend> for AuthMode {
    fn from(val: AuthBackend) -> Self {
        match val {
            #[cfg(feature = "ao")]
            AuthBackend::Ao => AuthMode::AoTell,
            AuthBackend::Anonymous => AuthMode::Anonymous,
            AuthBackend::Private => AuthMode::StaticToken,
            #[cfg(feature = "dynamic")]
            AuthBackend::Dynamic => AuthMode::StaticToken,
        }
    }
}

#[cfg(feature = "ao")]
#[derive(Debug, Clone)]
pub struct AoCredentials {
    pub username: String,
    pub password: String,
    pub character: String,
}

#[cfg(feature = "ao")]
pub struct MissingCredentialsError;

#[cfg(feature = "ao")]
impl TryFrom<&Args> for AoCredentials {
    type Error = MissingCredentialsError;

    fn try_from(value: &Args) -> Result<Self, Self::Error> {
        if let (Some(username), Some(password), Some(character)) = (
            value.ao_username.clone(),
            value.ao_password.clone(),
            value.ao_character.clone(),
        ) {
            Ok(Self {
                username,
                password,
                character,
            })
        } else {
            Err(MissingCredentialsError)
        }
    }
}
