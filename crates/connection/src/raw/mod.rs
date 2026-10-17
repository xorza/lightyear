//! A connection whose link is the connection: linking connects, and the [`LocalId`] and
//! [`RemoteId`] come from the link's addresses unless the IO layer sets them.
//!
//! [`LocalId`]: lightyear_core::id::LocalId
//! [`RemoteId`]: lightyear_core::id::RemoteId

pub mod client;
pub mod server;

#[cfg(test)]
mod tests {
    use crate::client::{Connected, Disconnected, DisconnectedReason};
    use bevy_app::App;
    use lightyear_link::crossbeam::{CrossbeamIo, CrossbeamPlugin};
    use lightyear_link::{Link, LinkStart, Linked, UnlinkReason};

    /// A raw client should observe the crossbeam channel disconnect through the
    /// regular Link -> Connection lifecycle and become Disconnected.
    #[test]
    fn peer_disconnect_disconnects_raw_client() {
        use crate::client::Connect;
        use crate::raw::client::{RawClient, RawConnectionPlugin};

        let (client_io, server_io) = CrossbeamIo::new_pair();

        let mut app = App::new();
        app.add_plugins(CrossbeamPlugin);
        app.add_plugins(RawConnectionPlugin);

        let client_entity = app.world_mut().spawn((RawClient, client_io)).id();
        app.world_mut().trigger(Connect {
            entity: client_entity,
        });
        app.update();

        assert!(app.world().get::<Linked>(client_entity).is_some());
        assert!(app.world().get::<Connected>(client_entity).is_some());
        assert!(app.world().get::<Disconnected>(client_entity).is_none());

        drop(server_io);

        app.update();

        assert!(app.world().get::<Linked>(client_entity).is_none());
        assert!(app.world().get::<Connected>(client_entity).is_none());
        assert_eq!(
            app.world()
                .get::<Disconnected>(client_entity)
                .map(|disconnected| &disconnected.reason),
            Some(&DisconnectedReason::LinkFailed(UnlinkReason::PeerStopped))
        );
    }

    /// Pair `CrossbeamPlugin` with server-side `RawConnectionPlugin`, mark the
    /// parent with `RawServer`, spawn the mirror with `LinkOf` + `CrossbeamIo`,
    /// then trigger `LinkStart` (not direct `Linked` insertion). The mirror
    /// should reach `Linked + Connected + ClientOf` so downstream
    /// `On<Insert, (Transport, ClientOf)>` channel observers fire.
    #[test]
    fn server_mirror_via_link_start_reaches_connected() {
        use crate::client::Connected;
        use crate::client_of::ClientOf;
        use crate::raw::server::RawServer;
        use lightyear_link::prelude::server::LinkOf;

        // Keep the client side of the pair alive for the duration of the test —
        // dropping it would Disconnect the server's channel before the assert.
        let (_client_io, server_io) = CrossbeamIo::new_pair();

        let mut app = App::new();
        app.add_plugins(CrossbeamPlugin);
        app.add_plugins(crate::raw::server::RawConnectionPlugin);

        let server_entity = app.world_mut().spawn(RawServer).id();
        let mirror_entity = app
            .world_mut()
            .spawn((
                LinkOf {
                    server: server_entity,
                },
                Link::default(),
                server_io,
            ))
            .id();
        app.world_mut().trigger(LinkStart {
            entity: mirror_entity,
        });

        app.update();

        let world = app.world();
        assert!(world.get::<Linked>(mirror_entity).is_some(), "Linked");
        assert!(world.get::<Connected>(mirror_entity).is_some(), "Connected");
        assert!(world.get::<ClientOf>(mirror_entity).is_some(), "ClientOf");
    }
}
