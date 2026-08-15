use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    protocol::Message,
    zsh::{self, ZshProtocolError},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireProtocol {
    JsonNdjson,
    ZshTab,
}

#[derive(Debug, Error)]
pub enum WireError {
    #[error("empty wire frame")]
    Empty,
    #[error("unrecognized wire protocol")]
    UnknownProtocol,
    #[error("invalid JSON protocol frame: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid Zsh protocol frame: {0}")]
    Zsh(#[from] ZshProtocolError),
}

pub fn decode_incoming(line: &str) -> Result<(WireProtocol, Message), WireError> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        return Err(WireError::Empty);
    }
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed)
            .map(|message| (WireProtocol::JsonNdjson, message))
            .map_err(Into::into);
    }
    if trimmed.starts_with("ZSH\t") {
        return zsh::decode_request(trimmed)
            .map(Message::from)
            .map(|message| (WireProtocol::ZshTab, message))
            .map_err(Into::into);
    }
    Err(WireError::UnknownProtocol)
}

pub fn encode_outgoing(protocol: WireProtocol, message: &Message) -> Result<String, WireError> {
    match protocol {
        WireProtocol::JsonNdjson => serde_json::to_string(message).map_err(Into::into),
        WireProtocol::ZshTab => zsh::encode_message(message).map_err(Into::into),
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::{Request, RequestBody};

    use super::*;

    #[test]
    fn detects_json_and_zsh() {
        let json =
            serde_json::to_string(&Message::from(Request::new(None, RequestBody::Ping))).unwrap();
        assert_eq!(decode_incoming(&json).unwrap().0, WireProtocol::JsonNdjson);
        assert_eq!(
            decode_incoming("ZSH\tPING\t\t").unwrap().0,
            WireProtocol::ZshTab
        );
    }
}
