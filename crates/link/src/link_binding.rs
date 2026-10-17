use bevy_ecs::component::Component;

/// A value both ends of a link hold and no third party can learn, which an app signs to bind its
/// own handshake to the link: the channel binding of a secure IO layer's session (RFC 9266's
/// `tls-exporter` for QUIC), or one value an in-process link gives both its ends. The IO layer
/// inserts it on a link's entity as the link links.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkBinding(pub [u8; 32]);
