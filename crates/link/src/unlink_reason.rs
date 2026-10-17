use bevy_reflect::Reflect;
use core::fmt;
use serde::{Deserialize, Serialize};

use crate::app_code::AppCode;

/// Why a [`Link`](crate::Link) is unlinked: the request an [`Unlink`](crate::Unlink) makes, and
/// the account [`Unlinked`](crate::Unlinked) keeps from the frame the link unlinks to the frame it
/// links again. An IO layer tells how its connection ended in the cases its protocol can tell
/// apart; one that cannot tell a case reports the nearest.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Reflect)]
pub enum UnlinkReason {
    /// The link has not linked yet.
    #[default]
    Initial,
    /// This end closed the link with its code.
    Closed(AppCode),
    /// The link's server stopped, which ends every link it holds.
    ServerStopped,
    /// The peer sent nothing for the idle timeout.
    TimedOut,
    /// The peer's endpoint holds no connection of this one: it started again since.
    Reset,
    /// The server refused the connection before its handshake: it has not started, it ended, or
    /// the address holds as many connections or handshakes as its limit lets.
    Refused,
    /// The server presented a key the client did not pin.
    WrongServerKey,
    /// The peer's endpoint dropped, as its app exited.
    PeerStopped,
    /// The peer ended the link with no reason given.
    Ended,
    /// The connection cannot carry the link's minimum MTU.
    Unfit,
    /// The peer's app closed the link with its code.
    ClosedByPeer(AppCode),
    /// The peer's app closed the link before the peer confirmed the handshake, whose packets carry
    /// no app code (RFC 9000 §10.2.3).
    ClosedInHandshake,
    /// The transport protocol failed with this error code, the peer's or this end's.
    Transport { code: u64 },
    /// The peer speaks no protocol version this end does.
    VersionMismatch,
    /// The endpoint could not open the connection.
    NotOpened,
    /// The endpoint's socket failed: no connection carries anything more.
    SocketFailed,
}

impl fmt::Display for UnlinkReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initial => f.write_str("not linked yet"),
            Self::Closed(code) => write!(f, "closed with code {}", code.get()),
            Self::ServerStopped => f.write_str("the server stopped"),
            Self::TimedOut => f.write_str("the peer timed out"),
            Self::Reset => f.write_str("the peer reset the connection"),
            Self::Refused => f.write_str("the server refused the connection"),
            Self::WrongServerKey => f.write_str("the server presented a key that was not pinned"),
            Self::PeerStopped => f.write_str("the peer's endpoint stopped"),
            Self::Ended => f.write_str("the peer ended the link"),
            Self::Unfit => f.write_str("the connection cannot carry the link's minimum MTU"),
            Self::ClosedByPeer(code) => write!(f, "the peer closed with code {}", code.get()),
            Self::ClosedInHandshake => f.write_str("the peer closed during the handshake"),
            Self::Transport { code } => write!(f, "transport error {code:#x}"),
            Self::VersionMismatch => f.write_str("the peer speaks no shared protocol version"),
            Self::NotOpened => f.write_str("the connection could not open"),
            Self::SocketFailed => f.write_str("the socket failed"),
        }
    }
}
