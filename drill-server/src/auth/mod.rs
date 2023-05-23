use std::collections::HashSet;

use drill_proto::AuthMode;

use crate::{config::SubdomainStrategy, util::random_subdomain};

#[cfg(feature = "ao")]
pub mod ao;
#[cfg(feature = "dynamic")]
pub mod dynamic;

#[derive(PartialEq)]
pub enum AuthState {
    AwaitingAoAuth,
    AwaitingToken,
    Proxying,
}

impl From<AuthMode> for AuthState {
    fn from(value: AuthMode) -> Self {
        match value {
            AuthMode::Anonymous | AuthMode::StaticToken => Self::AwaitingToken,
            AuthMode::AoTell => Self::AwaitingAoAuth,
        }
    }
}

pub enum AuthProvider {
    #[cfg(feature = "ao")]
    Ao(ao::AoAuthProvider),
    Anonymous,
    Private(HashSet<String>),
    #[cfg(feature = "dynamic")]
    Dynamic(dynamic::DynamicAuthProvider),
}

impl AuthProvider {
    #[cfg(feature = "ao")]
    pub fn expect(&self, character: String, token: String) {
        match self {
            Self::Ao(ao) => ao.expect(character, token),
            _ => {}
        }
    }

    pub async fn verify(
        &self,
        token: &str,
        desired_subdomain: &str,
        strategy: SubdomainStrategy,
    ) -> Option<String> {
        match self {
            #[cfg(feature = "ao")]
            Self::Ao(ao) => ao.verify(token, desired_subdomain, strategy),
            Self::Anonymous => match strategy {
                SubdomainStrategy::ClientChoice => Some(desired_subdomain.to_string()),
                _ => Some(random_subdomain()),
            },
            Self::Private(set) => {
                if set.contains(token) {
                    match strategy {
                        SubdomainStrategy::ClientChoice => Some(desired_subdomain.to_string()),
                        _ => Some(random_subdomain()),
                    }
                } else {
                    None
                }
            }
            #[cfg(feature = "ao")]
            Self::Dynamic(dynamic) => dynamic.verify(token, desired_subdomain).await,
        }
    }
}
