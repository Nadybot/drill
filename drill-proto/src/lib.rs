use std::slice::SliceIndex;

/// The version of the protocol implemented by the crate.
/// The first two bytes of the `Hello` [`Event`] must match this integer.
const IMPLEMENTED_PROTO_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum AuthMode {
    StaticToken = 1,
    Anonymous = 2,
    AoTell = 3,
}

impl TryFrom<&u8> for AuthMode {
    type Error = ParseEventError;

    fn try_from(value: &u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::StaticToken),
            2 => Ok(Self::Anonymous),
            3 => Ok(Self::AoTell),
            _ => Err(Self::Error::InvalidAuthMode),
        }
    }
}

const HELLO: u8 = 1;
const AO_AUTH: u8 = 2;
const TOKEN_IN_AO_TELL: u8 = 3;
const PRESENT_TOKEN: u8 = 4;
const LETS_GO: u8 = 5;
const AUTH_FAILED: u8 = 6;
const OUT_OF_CAPACITY: u8 = 7;
const DISALLOWED_PACKET: u8 = 8;
const DATA: u8 = 9;
const CLOSED: u8 = 10;

/// A packet exchanged between client and server on the control websocket.
#[derive(Debug)]
pub enum Event<'a> {
    /// Hello packet is sent by server when a client connects.
    /// The protocol version will always be present in the first two bytes
    /// of this packet, no matter the version.
    Hello {
        proto_version: u16,
        auth_mode: AuthMode,
        description: &'a str,
    },
    /// Packets for authentication flow with [`AuthMode::AoTell`].
    AoAuth {
        character_name: &'a str,
    },
    TokenInAoTell {
        sender: &'a str,
    },
    /// Client has a token (either static or through [`AuthMode::AoTell`]) and
    /// wants to authenticate.
    PresentToken {
        token: &'a str,
        desired_subdomain: &'a str,
    },
    /// Server has accepted client's token and created a subdomain. The public
    /// URL may not match the desired subdomain.
    LetsGo {
        public_url: &'a str,
    },
    /// Authentication has failed, the token is not valid.
    AuthFailed,
    /// Authentication was successful, but the server is out of capacity (e.g.
    /// client limit reached).
    OutOfCapacity,
    /// Disallowed packet sent (e.g. for wrong auth method).
    DisallowedPacket,
    /// A packet with raw TCP data.
    Data {
        id: &'a str,
        data: &'a [u8],
    },
    /// TCP connection has been terminated.
    Closed {
        id: &'a str,
    },
}

impl<'a> Event<'a> {
    #[must_use] pub fn hello(auth_mode: AuthMode, description: &'a str) -> Self {
        Self::Hello {
            proto_version: IMPLEMENTED_PROTO_VERSION,
            auth_mode,
            description,
        }
    }

    #[must_use] pub fn ao_auth(character_name: &'a str) -> Self {
        Self::AoAuth { character_name }
    }

    #[must_use] pub fn token_in_ao_tell(sender: &'a str) -> Self {
        Self::TokenInAoTell { sender }
    }

    #[must_use] pub fn present_token(token: &'a str, desired_subdomain: &'a str) -> Self {
        Self::PresentToken {
            token,
            desired_subdomain,
        }
    }

    #[must_use] pub fn lets_go(public_url: &'a str) -> Self {
        Self::LetsGo { public_url }
    }

    #[must_use] pub fn auth_failed() -> Self {
        Self::AuthFailed
    }

    #[must_use] pub fn out_of_capacity() -> Self {
        Self::OutOfCapacity
    }

    #[must_use] pub fn disallowed_packet() -> Self {
        Self::DisallowedPacket
    }

    #[must_use] pub fn data(id: &'a str, data: &'a [u8]) -> Self {
        Self::Data { id, data }
    }

    #[must_use] pub fn closed(id: &'a str) -> Self {
        Self::Closed { id }
    }
}

#[derive(Debug)]
pub enum ParseEventError {
    Eof,
    InvalidUtf8,
    UnknownType,
    InvalidAuthMode,
    ProtocolVersionMismatch,
}

fn try_parse_str<I: SliceIndex<[u8], Output = [u8]>>(
    input: &[u8],
    idx: I,
) -> Result<&str, ParseEventError> {
    std::str::from_utf8(input.get(idx).ok_or(ParseEventError::Eof)?)
        .map_err(|_| ParseEventError::InvalidUtf8)
}

impl<'a> Event<'a> {
    /// Parse an [`Event`] from a slice of bytes.
    ///
    /// # Errors
    ///
    /// Returns a [`ParseEventError`] if parsing an event failed.
    pub fn deserialize(input: &'a [u8]) -> Result<Self, ParseEventError> {
        let event_type = input.first().ok_or(ParseEventError::Eof)?;

        match *event_type {
            HELLO => {
                let proto_version = u16::from_be_bytes(unsafe {
                    input
                        .get(1..3)
                        .ok_or(ParseEventError::Eof)?
                        .try_into()
                        .unwrap_unchecked()
                });

                if proto_version != IMPLEMENTED_PROTO_VERSION {
                    return Err(ParseEventError::ProtocolVersionMismatch);
                }

                let auth_mode = AuthMode::try_from(input.get(3).ok_or(ParseEventError::Eof)?)?;
                let description = try_parse_str(input, 4..)?;

                Ok(Self::Hello {
                    proto_version,
                    auth_mode,
                    description,
                })
            }
            AO_AUTH => {
                let character_name = try_parse_str(input, 1..)?;

                Ok(Self::AoAuth { character_name })
            }
            TOKEN_IN_AO_TELL => {
                let sender = try_parse_str(input, 1..)?;

                Ok(Self::TokenInAoTell { sender })
            }
            PRESENT_TOKEN => {
                let token = try_parse_str(input, 1..37)?;
                let desired_subdomain = try_parse_str(input, 37..)?;

                Ok(Self::PresentToken {
                    token,
                    desired_subdomain,
                })
            }
            LETS_GO => {
                let public_url = try_parse_str(input, 1..)?;

                Ok(Self::LetsGo { public_url })
            }
            AUTH_FAILED => Ok(Self::AuthFailed),
            OUT_OF_CAPACITY => Ok(Self::OutOfCapacity),
            DISALLOWED_PACKET => Ok(Self::DisallowedPacket),
            DATA => {
                let id = try_parse_str(input, 1..37)?;
                let data = input.get(37..).ok_or(ParseEventError::Eof)?;

                Ok(Self::Data { id, data })
            }
            CLOSED => {
                let id = try_parse_str(input, 1..37)?;

                Ok(Self::Closed { id })
            }
            _ => Err(ParseEventError::UnknownType),
        }
    }

    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        match self {
            Self::Hello {
                proto_version,
                auth_mode,
                description,
            } => {
                let mut serialized = Vec::with_capacity(4 + description.len());
                serialized.push(HELLO);
                serialized.extend_from_slice(&proto_version.to_be_bytes());
                serialized.push(*auth_mode as u8);
                serialized.extend_from_slice(description.as_bytes());

                serialized
            }
            Self::AoAuth { character_name } => {
                let mut serialized = Vec::with_capacity(1 + character_name.len());
                serialized.push(AO_AUTH);
                serialized.extend_from_slice(character_name.as_bytes());

                serialized
            }
            Self::TokenInAoTell { sender } => {
                let mut serialized = Vec::with_capacity(1 + sender.len());
                serialized.push(TOKEN_IN_AO_TELL);
                serialized.extend_from_slice(sender.as_bytes());

                serialized
            }
            Self::PresentToken {
                token,
                desired_subdomain,
            } => {
                let mut serialized = Vec::with_capacity(37 + desired_subdomain.len());
                serialized.push(PRESENT_TOKEN);
                serialized.extend_from_slice(token.as_bytes());
                serialized.extend_from_slice(desired_subdomain.as_bytes());

                serialized
            }
            Self::LetsGo { public_url } => {
                let mut serialized = Vec::with_capacity(1 + public_url.len());
                serialized.push(LETS_GO);
                serialized.extend_from_slice(public_url.as_bytes());

                serialized
            }
            Self::AuthFailed => {
                vec![AUTH_FAILED]
            }
            Self::OutOfCapacity => {
                vec![OUT_OF_CAPACITY]
            }
            Self::DisallowedPacket => {
                vec![DISALLOWED_PACKET]
            }
            Self::Data { id, data } => {
                let mut serialized = Vec::with_capacity(37 + data.len());
                serialized.push(DATA);
                serialized.extend_from_slice(id.as_bytes());
                serialized.extend_from_slice(data);

                serialized
            }
            Self::Closed { id } => {
                let mut serialized = Vec::with_capacity(37);
                serialized.push(CLOSED);
                serialized.extend_from_slice(id.as_bytes());

                serialized
            }
        }
    }
}
