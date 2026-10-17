use crate::ConnectionSystems;
use crate::client::{Client, Connected, Disconnected};
use crate::host::HostClient;
use crate::server::{Started, Stopped};
use bevy_app::{App, Plugin, PostUpdate};
use bevy_ecs::prelude::*;
use bevy_platform::collections::HashMap;
use lightyear_core::id::PeerId;
use lightyear_link::prelude::{LinkOf, Server};
use smallvec::SmallVec;

/// The ready networking role of this Bevy application.
///
/// Clients are included after they are [`Connected`] and servers after they are [`Started`].
#[derive(Default, Debug, Clone, PartialEq, Eq)]
pub enum NetworkTopology {
    /// No networking topology has been identified yet.
    #[default]
    Undefined,
    /// A connected client and its server link.
    Client(Entity),
    /// A started server.
    Server(Entity),
    /// A connected in-process client and its started server.
    HostClient {
        /// The started server entity.
        server: Entity,
        /// The connected host-client link entity.
        client: Entity,
    },
    /// The ready entities do not form one supported networking topology.
    Invalid(NetworkTopologyError),
}

impl NetworkTopology {
    /// Returns true for a connected client.
    pub fn is_client(&self) -> bool {
        matches!(self, Self::Client(_))
    }

    /// Returns true for a started server, including the server side of a host-client app.
    pub fn is_server(&self) -> bool {
        matches!(self, Self::Server(_) | Self::HostClient { .. })
    }

    /// Returns true for a started server without a connected in-process client.
    pub fn is_headless_server(&self) -> bool {
        matches!(self, Self::Server(_))
    }

    /// Returns true for a ready in-process host-client app.
    pub fn is_host_server(&self) -> bool {
        matches!(self, Self::HostClient { .. })
    }
}

/// Cached metadata describing the networking configuration of this Bevy application.
///
/// [`crate::ConnectionPlugin`] maintains this resource from role and lifecycle components. Users
/// can read [`mode`](Self::mode) and [`peer_map`](Self::peer_map), but do not need to
/// update them themselves.
#[derive(Resource, Debug, Clone)]
pub struct NetworkingMetadata {
    /// The currently identified networking topology.
    pub mode: NetworkTopology,
    /// Mapping from remote peer IDs to their local connection entities.
    pub peer_map: HashMap<PeerId, Entity>,
    // This is mutated without triggering Bevy change detection. Consumers therefore only observe
    // a change after `mode` itself changes.
    dirty: bool,
}

impl Default for NetworkingMetadata {
    fn default() -> Self {
        Self {
            mode: NetworkTopology::Undefined,
            peer_map: HashMap::default(),
            dirty: true,
        }
    }
}

/// Why ready networking entities could not be classified into a supported [`NetworkTopology`].
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum NetworkTopologyError {
    /// More than one client link is connected.
    #[error("multiple client links are connected: {0:?}")]
    MultipleConnectedClients(SmallVec<[Entity; 4]>),
    /// More than one server is started.
    #[error("multiple servers are started: {0:?}")]
    MultipleStartedServers(SmallVec<[Entity; 4]>),
    /// A connected host-client does not have a `Client` marker.
    #[error("connected host-client {client:?} does not have a Client marker")]
    HostClientWithoutClient {
        /// The malformed host-client entity.
        client: Entity,
    },
    /// A connected host-client does not identify its in-process server.
    #[error("connected host-client {client:?} does not have a LinkOf relationship")]
    HostClientMissingLinkOf {
        /// The malformed host-client entity.
        client: Entity,
    },
    /// The server referenced by a connected host-client is not started.
    #[error(
        "connected host-client {client:?} references server {server:?}, but that server is not started"
    )]
    HostClientServerNotStarted {
        /// The connected host-client entity.
        client: Entity,
        /// The referenced server entity.
        server: Entity,
    },
    /// A client and a server are both ready but do not form a host-client pair.
    #[error(
        "connected client {client:?} and started server {server:?} do not form a host-client pair"
    )]
    MixedClientServer {
        /// The connected client entity.
        client: Entity,
        /// The started server entity.
        server: Entity,
    },
}

/// System set that refreshes the cached [`NetworkTopology`].
#[derive(SystemSet, Debug, Hash, PartialEq, Eq, Clone, Copy)]
pub enum NetworkTopologySystems {
    /// Infer the topology after networking lifecycle changes.
    Update,
}

#[derive(Clone, Copy, Debug)]
struct ReadyClient {
    entity: Entity,
    is_host: bool,
    server: Option<Entity>,
}

type TopologyComponents = (
    Client,
    Server,
    HostClient,
    LinkOf,
    Connected,
    Disconnected,
    Started,
    Stopped,
);

pub(crate) struct NetworkTopologyPlugin;

impl Plugin for NetworkTopologyPlugin {
    fn build(&self, app: &mut App) {
        // NetworkingMetadata starts dirty so that entities spawned before this plugin are
        // classified on the first update too.
        app.init_resource::<NetworkingMetadata>();

        app.add_observer(mark_dirty_on_insert);
        app.add_observer(mark_dirty_on_remove);
        app.add_observer(mark_dirty_on_discard);

        // Reconcile lifecycle changes once per frame.
        app.configure_sets(
            PostUpdate,
            NetworkTopologySystems::Update.before(ConnectionSystems::Send),
        );
        app.add_systems(
            PostUpdate,
            refresh_network_topology
                .in_set(NetworkTopologySystems::Update)
                .run_if(network_topology_is_dirty),
        );
    }
}

fn mark_dirty_on_insert(
    _trigger: On<Insert, TopologyComponents>,
    mut metadata: ResMut<NetworkingMetadata>,
) {
    metadata.bypass_change_detection().dirty = true;
}

fn mark_dirty_on_remove(
    _trigger: On<Remove, TopologyComponents>,
    mut metadata: ResMut<NetworkingMetadata>,
) {
    metadata.bypass_change_detection().dirty = true;
}

fn mark_dirty_on_discard(
    _trigger: On<Discard, TopologyComponents>,
    mut metadata: ResMut<NetworkingMetadata>,
) {
    metadata.bypass_change_detection().dirty = true;
}

fn network_topology_is_dirty(metadata: Res<NetworkingMetadata>) -> bool {
    metadata.dirty
}

fn refresh_network_topology(
    mut metadata: ResMut<NetworkingMetadata>,
    ready_clients: Query<
        (Entity, Has<HostClient>, Option<&LinkOf>),
        (With<Client>, With<Connected>),
    >,
    ready_servers: Query<Entity, (With<Server>, With<Started>)>,
    malformed_hosts: Query<Entity, (With<HostClient>, With<Connected>, Without<Client>)>,
) {
    let malformed_host = malformed_hosts
        .iter()
        .min_by_key(|entity| entity.index_u32());
    let next = if let Some(client) = malformed_host {
        NetworkTopology::Invalid(NetworkTopologyError::HostClientWithoutClient { client })
    } else {
        let client = unique_ready_client(ready_clients.iter().map(|(entity, is_host, link_of)| {
            ReadyClient {
                entity,
                is_host,
                server: link_of.map(|link| link.server),
            }
        }));
        match client {
            Err(error) => NetworkTopology::Invalid(error),
            Ok(client) => match unique_ready_server(ready_servers.iter()) {
                Err(error) => NetworkTopology::Invalid(error),
                Ok(server) => infer_standard_topology(client, server),
            },
        }
    };

    let mode_changed = metadata.mode != next;
    metadata.bypass_change_detection().dirty = false;
    if mode_changed {
        if let NetworkTopology::Invalid(error) = &next {
            tracing::error!(%error, "invalid Lightyear networking topology");
        }
        metadata.mode = next;
    }
}

fn unique_ready_client(
    mut clients: impl Iterator<Item = ReadyClient>,
) -> Result<Option<ReadyClient>, NetworkTopologyError> {
    let Some(first) = clients.next() else {
        return Ok(None);
    };
    let Some(second) = clients.next() else {
        return Ok(Some(first));
    };

    let mut entities: SmallVec<[Entity; 4]> = SmallVec::from_slice(&[first.entity, second.entity]);
    entities.extend(clients.map(|client| client.entity));
    entities.sort_unstable_by_key(|entity| entity.index_u32());
    Err(NetworkTopologyError::MultipleConnectedClients(entities))
}

fn unique_ready_server(
    mut servers: impl Iterator<Item = Entity>,
) -> Result<Option<Entity>, NetworkTopologyError> {
    let Some(first) = servers.next() else {
        return Ok(None);
    };
    let Some(second) = servers.next() else {
        return Ok(Some(first));
    };

    let mut entities: SmallVec<[Entity; 4]> = SmallVec::from_slice(&[first, second]);
    entities.extend(servers);
    entities.sort_unstable_by_key(|entity| entity.index_u32());
    Err(NetworkTopologyError::MultipleStartedServers(entities))
}

fn infer_standard_topology(client: Option<ReadyClient>, server: Option<Entity>) -> NetworkTopology {
    match (client.as_ref(), server) {
        (None, None) => NetworkTopology::Undefined,
        (None, Some(server)) => NetworkTopology::Server(server),
        (Some(client), None) if client.is_host => match client.server {
            Some(server) => {
                NetworkTopology::Invalid(NetworkTopologyError::HostClientServerNotStarted {
                    client: client.entity,
                    server,
                })
            }
            None => NetworkTopology::Invalid(NetworkTopologyError::HostClientMissingLinkOf {
                client: client.entity,
            }),
        },
        (Some(client), None) => NetworkTopology::Client(client.entity),
        (Some(client), Some(server)) if client.is_host => match client.server {
            Some(linked_server) if linked_server == server => NetworkTopology::HostClient {
                server,
                client: client.entity,
            },
            Some(linked_server) => {
                NetworkTopology::Invalid(NetworkTopologyError::HostClientServerNotStarted {
                    client: client.entity,
                    server: linked_server,
                })
            }
            None => NetworkTopology::Invalid(NetworkTopologyError::HostClientMissingLinkOf {
                client: client.entity,
            }),
        },
        (Some(client), Some(server)) => {
            NetworkTopology::Invalid(NetworkTopologyError::MixedClientServer {
                client: client.entity,
                server,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::DisconnectedReason;
    use bevy_ecs::change_detection::DetectChanges;
    use lightyear_core::id::{PeerId, RemoteId};

    fn test_app() -> App {
        let mut app = App::new();
        app.add_plugins(crate::ConnectionPlugin);
        app.update();
        app
    }

    fn connect_client(app: &mut App, peer: u64) -> Entity {
        app.world_mut()
            .spawn((Client, RemoteId(PeerId::Local(peer)), Connected))
            .id()
    }

    fn start_server(app: &mut App) -> Entity {
        app.world_mut().spawn((Server::default(), Started)).id()
    }

    fn mode(app: &App) -> &NetworkTopology {
        &app.world().resource::<NetworkingMetadata>().mode
    }

    #[test]
    fn only_ready_client_and_server_entities_are_cached() {
        let mut app = test_app();
        let client = app.world_mut().spawn(Client).id();
        app.update();
        assert_eq!(mode(&app), &NetworkTopology::Undefined);

        app.world_mut()
            .entity_mut(client)
            .insert((RemoteId(PeerId::Local(1)), Connected));
        app.update();
        assert_eq!(mode(&app), &NetworkTopology::Client(client));

        app.world_mut().entity_mut(client).insert(Disconnected {
            reason: DisconnectedReason::UserRequested(Some("test".into())),
        });
        app.update();
        assert_eq!(mode(&app), &NetworkTopology::Undefined);

        let server = app.world_mut().spawn(Server::default()).id();
        app.update();
        assert_eq!(mode(&app), &NetworkTopology::Undefined);

        app.world_mut().entity_mut(server).insert(Started);
        app.update();
        assert_eq!(mode(&app), &NetworkTopology::Server(server));

        app.world_mut().entity_mut(server).insert(Stopped);
        app.update();
        assert_eq!(mode(&app), &NetworkTopology::Undefined);
    }

    #[test]
    fn host_client_requires_a_connected_client_and_its_started_server() {
        let mut app = test_app();
        let server = app.world_mut().spawn(Server::default()).id();
        let client = app
            .world_mut()
            .spawn((
                Client,
                RemoteId(PeerId::Local(0)),
                Connected,
                LinkOf { server },
                HostClient { buffer: Vec::new() },
            ))
            .id();

        app.update();
        assert_eq!(
            mode(&app),
            &NetworkTopology::Invalid(NetworkTopologyError::HostClientServerNotStarted {
                client,
                server
            })
        );

        app.world_mut().entity_mut(server).insert(Started);
        app.update();
        assert_eq!(mode(&app), &NetworkTopology::HostClient { server, client });
    }

    #[test]
    fn unsupported_ready_role_combinations_are_invalid() {
        let mut app = test_app();
        let first = connect_client(&mut app, 1);
        let second = connect_client(&mut app, 2);
        app.update();
        assert_eq!(
            mode(&app),
            &NetworkTopology::Invalid(NetworkTopologyError::MultipleConnectedClients(
                SmallVec::from_slice(&[first, second])
            ))
        );

        app.world_mut().despawn(second);
        let server = start_server(&mut app);
        app.update();
        assert_eq!(
            mode(&app),
            &NetworkTopology::Invalid(NetworkTopologyError::MixedClientServer {
                client: first,
                server,
            })
        );

        let other_server = start_server(&mut app);
        app.world_mut().despawn(first);
        app.update();
        assert_eq!(
            mode(&app),
            &NetworkTopology::Invalid(NetworkTopologyError::MultipleStartedServers(
                SmallVec::from_slice(&[server, other_server])
            ))
        );
    }

    #[test]
    fn connected_host_without_link_relationship_is_invalid() {
        let mut app = test_app();
        let client = app
            .world_mut()
            .spawn((
                Client,
                RemoteId(PeerId::Local(0)),
                Connected,
                HostClient { buffer: Vec::new() },
            ))
            .id();

        app.update();
        assert_eq!(
            mode(&app),
            &NetworkTopology::Invalid(NetworkTopologyError::HostClientMissingLinkOf { client })
        );
    }

    #[test]
    fn unchanged_invalidation_does_not_mark_networking_metadata_changed() {
        let mut app = test_app();
        let last_changed = app
            .world()
            .resource_ref::<NetworkingMetadata>()
            .last_changed();

        // This invalidates the cache, but a disconnected Client does not change the mode.
        app.world_mut().spawn(Client);
        app.update();

        assert_eq!(
            app.world()
                .resource_ref::<NetworkingMetadata>()
                .last_changed(),
            last_changed
        );
    }
}
