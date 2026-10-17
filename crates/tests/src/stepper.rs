use crate::protocol::ProtocolPlugin;
use bevy::MinimalPlugins;
use bevy::app::PluginsState;
use bevy::input::InputPlugin;
use bevy::log::LogPlugin;
use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use core::net::{Ipv4Addr, SocketAddr};
use core::time::Duration;
use lightyear::prelude::{client::*, server::*, *};
#[cfg(feature = "test_utils")]
use lightyear_core::test::TestHelper;
use lightyear_raw_connection::client::RawClient;
use lightyear_raw_connection::server::RawServer;
use lightyear_replication::receive::ReplicationReceiver;

pub const TICK_DURATION: Duration = Duration::from_millis(10);

/// Adds the per-peer components needed by the shared test protocol to links created by any IO.
fn configure_server_link(
    trigger: On<Add, LinkOf>,
    clients: Query<(), With<Client>>,
    mut commands: Commands,
) {
    // A host client also has `LinkOf`, but it is a local Client relationship rather than a remote
    // server peer. Host-specific systems configure it separately.
    if clients.contains(trigger.entity) {
        return;
    }
    commands.entity(trigger.entity).insert((
        PingManager::new(PingConfig {
            ping_interval: Duration::default(),
        }),
        ReplicationSender,
        ReplicationReceiver,
        #[cfg(feature = "test_utils")]
        TestHelper::default(),
    ));
}
/// Returns whether the application-global local timeline synchronization is ready.
pub fn input_timeline_is_synced(world: &World) -> bool {
    world
        .get_resource::<LocalTimelineSync>()
        .is_some_and(LocalTimelineSync::is_synced)
}

/// Returns whether the application-global presentation timeline is ready.
pub fn interpolation_timeline_is_synced(world: &World) -> bool {
    world
        .get_resource::<InterpolationTimeline>()
        .is_some_and(InterpolationTimeline::is_synced)
}

/// Stepper with:
/// - n client in one 'client' App
/// - 1 server in another App, with n ClientOf connected to each client
///
/// Clients and server link over in-process Crossbeam channels with raw connections. We create
/// separate apps to make client/server update ordering explicit.
pub struct ClientServerStepper {
    pub client_apps: Vec<App>,
    pub server_app: App,
    pub client_entities: Vec<Entity>,
    pub server_entity: Entity,
    pub client_of_entities: Vec<Entity>,
    pub host_client_entity: Option<Entity>,
    pub frame_duration: Duration,
    pub tick_duration: Duration,
    pub current_time: bevy::platform::time::Instant,
    server_started: bool,
}

/// Connection layer used by a stepper client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientType {
    Host,
    Raw,
}

/// Configuration for ClientServerStepper
pub struct StepperConfig {
    pub frame_duration: Duration,
    pub tick_duration: Duration,
    pub clients: Vec<ClientType>,
    pub init: bool,
    pub server_registry: Option<MetricsRegistry>,
    pub client_registry: Option<MetricsRegistry>,
}

impl StepperConfig {
    /// Basic scenario: one remote client.
    pub fn single() -> Self {
        Self::with_clients(vec![ClientType::Raw])
    }

    /// Host server scenario: one host client and one remote client.
    pub fn host_server() -> Self {
        Self::with_clients(vec![ClientType::Host, ClientType::Raw])
    }

    /// `n` remote clients.
    pub fn with_raw_clients(n: usize) -> Self {
        Self::with_clients(vec![ClientType::Raw; n])
    }

    pub fn with_clients(clients: Vec<ClientType>) -> Self {
        Self {
            frame_duration: TICK_DURATION,
            tick_duration: TICK_DURATION,
            clients,
            init: true,
            server_registry: None,
            client_registry: None,
        }
    }
}

impl ClientServerStepper {
    pub fn from_config(config: StepperConfig) -> Self {
        let mut stepper = Self::new_server(
            config.tick_duration,
            config.frame_duration,
            config.server_registry.clone(),
        );
        for client_type in config.clients {
            stepper.new_client(client_type, config.client_registry.clone());
        }
        if config.init {
            stepper.init();
        }
        stepper
    }
}

impl ClientServerStepper {
    pub fn new_server(
        tick_duration: Duration,
        frame_duration: Duration,
        metrics_registry: Option<MetricsRegistry>,
    ) -> Self {
        let mut server_app = App::new();
        // NOTE: we add LogPlugin so that tracing works
        server_app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            StatesPlugin,
            InputPlugin,
            LogPlugin::default(),
            MetricsPlugin::new(metrics_registry),
        ));
        server_app.add_plugins((server::ServerPlugins { tick_duration }, RoomPlugin));
        server_app.add_observer(configure_server_link);
        // ProtocolPlugin needs to be added AFTER InputPlugin
        server_app.add_plugins(ProtocolPlugin);
        let server_entity = server_app.world_mut().spawn(RawServer).id();

        Self {
            client_apps: vec![],
            server_app,
            client_entities: vec![],
            server_entity,
            host_client_entity: None,
            client_of_entities: vec![],
            frame_duration,
            tick_duration,
            current_time: bevy::platform::time::Instant::now(),
            server_started: false,
        }
    }

    pub fn new_client(
        &mut self,
        client_type: ClientType,
        metrics_registry: Option<MetricsRegistry>,
    ) -> usize {
        let mut client_app = App::new();
        client_app.add_plugins((
            MinimalPlugins,
            TransformPlugin,
            StatesPlugin,
            InputPlugin,
            LogPlugin::default(),
            MetricsPlugin::new(metrics_registry),
        ));

        client_app.add_plugins(client::ClientPlugins {
            tick_duration: self.tick_duration,
        });
        // ProtocolPlugin needs to be added AFTER ClientPlugins, InputPlugin, because we need the PredictionRegistry to exist
        client_app.add_plugins(ProtocolPlugin);
        client_app.finish();
        client_app.cleanup();
        let client_id = self.client_entities.len();

        if client_type == ClientType::Host {
            // for host client we don't need auth
            // the server app will contain both client and server plugins
            self.server_app.add_plugins(client::ClientPlugins {
                tick_duration: self.tick_duration,
            });
            self.host_client_entity = Some(
                self.server_app
                    .world_mut()
                    .spawn((
                        // Client + LinkOf = HostServer
                        Client,
                        LinkOf {
                            server: self.server_entity,
                        },
                        // TODO: maybe don't add Link either?
                        Link::default(),
                        Linked,
                    ))
                    .id(),
            );
            return 0;
        }
        client_app.insert_resource(PredictionManager::default());
        let mut client = client_app.world_mut().spawn((
            Client,
            // Send pings every frame, so that the Acks are sent every frame
            PingManager::new(PingConfig {
                ping_interval: Duration::default(),
            }),
            ReplicationSender,
            ReplicationReceiver,
            #[cfg(feature = "test_utils")]
            TestHelper::default(),
        ));
        let (crossbeam_client, crossbeam_server) = lightyear_crossbeam::CrossbeamIo::new_pair();
        client.insert((crossbeam_client, RawClient));
        self.client_of_entities.push(
            self.server_app
                .world_mut()
                .spawn((
                    LinkOf {
                        server: self.server_entity,
                    },
                    Link::default(),
                    PeerAddr(SocketAddr::new(
                        core::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
                        client_id as u16,
                    )),
                    crossbeam_server,
                ))
                .id(),
        );
        self.client_entities.push(client.id());
        self.client_apps.push(client_app);
        client_id
    }

    /// Disconnect the last client
    pub fn disconnect_client(&mut self) {
        let client_entity = self.client_entities.pop().unwrap();
        let server_entity = self.client_of_entities.pop().unwrap();
        let mut client_app = self.client_apps.pop().unwrap();

        client_app.world_mut().trigger(Disconnect {
            entity: client_entity,
        });
        // on the server normally we should wait for the client to send a Disconnect message, but if we despawn the client entity
        // the crossbeam io gets severed
        self.server_app
            .world_mut()
            .entity_mut(server_entity)
            .insert(Disconnected {
                reason: DisconnectedReason::Unknown,
            });
        client_app.world_mut().flush();
        self.server_app.world_mut().flush();
        client_app.world_mut().despawn(client_entity);
        self.server_app.world_mut().despawn(server_entity);
        self.frame_step(1);
    }

    pub fn default_no_init() -> Self {
        Self::new_server(TICK_DURATION, TICK_DURATION, None)
    }

    pub fn client_app(&mut self) -> &mut App {
        assert_eq!(self.client_apps.len(), 1);
        &mut self.client_apps[0]
    }

    pub fn client_tick(&self, id: usize) -> Tick {
        self.client_apps[id]
            .world()
            .resource::<LocalTimeline>()
            .tick()
    }
    pub fn server_tick(&self) -> Tick {
        self.server_app.world().resource::<LocalTimeline>().tick()
    }

    pub fn host_client(&self) -> EntityRef<'_> {
        self.server_app
            .world()
            .entity(self.host_client_entity.unwrap())
    }

    pub fn host_client_mut(&mut self) -> EntityWorldMut<'_> {
        self.server_app
            .world_mut()
            .entity_mut(self.host_client_entity.unwrap())
    }

    pub fn client(&self, id: usize) -> EntityRef<'_> {
        self.client_apps[id]
            .world()
            .entity(self.client_entities[id])
    }

    pub fn client_mut(&mut self, id: usize) -> EntityWorldMut<'_> {
        self.client_apps[id]
            .world_mut()
            .entity_mut(self.client_entities[id])
    }

    pub fn server(&self) -> EntityRef<'_> {
        self.server_app.world().entity(self.server_entity)
    }

    pub fn server_mut(&mut self) -> EntityWorldMut<'_> {
        self.server_app.world_mut().entity_mut(self.server_entity)
    }

    pub fn client_of(&self, id: usize) -> EntityRef<'_> {
        self.server_app.world().entity(self.client_of_entities[id])
    }

    pub fn client_of_mut(&mut self, id: usize) -> EntityWorldMut<'_> {
        self.server_app
            .world_mut()
            .entity_mut(self.client_of_entities[id])
    }

    pub fn init(&mut self) {
        self.start_server();

        let now = self.current_time;
        for i in 0..self.client_entities.len() {
            self.client_apps[i]
                .world_mut()
                .get_resource_mut::<Time<Real>>()
                .unwrap()
                .update_with_instant(now);
            self.client_apps[i].world_mut().trigger(Connect {
                entity: self.client_entities[i],
            });
            // Crossbeam has no listening IO to accept the client, so the server side of the
            // link is linked here, once the client connects.
            let mut client_of = self
                .server_app
                .world_mut()
                .entity_mut(self.client_of_entities[i]);
            if !client_of.contains::<Linked>() {
                client_of.insert(Linked);
            }
        }
        if let Some(host) = self.host_client_entity {
            self.server_app
                .world_mut()
                .trigger(Connect { entity: host });
        }

        self.wait_for_connection();
        self.wait_for_sync();
    }

    /// Finalizes and starts the server once.
    pub fn start_server(&mut self) {
        if self.server_started {
            return;
        }

        if matches!(
            self.server_app.plugins_state(),
            PluginsState::Ready | PluginsState::Adding
        ) {
            self.server_app.finish();
            self.server_app.cleanup();
        }

        // Initialize Real time (needed only for the first TimeSystem run)
        let now = bevy::platform::time::Instant::now();
        self.current_time = now;
        self.server_app
            .world_mut()
            .get_resource_mut::<Time<Real>>()
            .unwrap()
            .update_with_instant(now);
        self.server_app.world_mut().trigger(Start {
            entity: self.server_entity,
        });
        // Crossbeam has no listening IO to link the server, and `RawServer` starts once linked.
        self.server_app
            .world_mut()
            .entity_mut(self.server_entity)
            .insert(Linked);
        // For HostServer, the server needs to be started before the client,
        // so make sure it is started
        self.server_app.world_mut().flush();
        self.server_started = true;
    }

    /// Frame step until all clients are connected
    pub fn wait_for_connection(&mut self) {
        for _ in 0..50 {
            if (0..self.client_entities.len())
                .all(|client_id| self.client(client_id).contains::<Connected>())
            {
                info!("Clients are all connected");
                break;
            }
            self.tick_step(1);
        }
    }

    // Advance the world until the client is synced
    pub fn wait_for_sync(&mut self) {
        for _ in 0..50 {
            if (0..self.client_entities.len()).all(|client_id| {
                input_timeline_is_synced(self.client_apps[client_id].world())
                    && interpolation_timeline_is_synced(self.client_apps[client_id].world())
            }) {
                info!("Clients are all synced");
                break;
            }
            self.tick_step(1);
        }
    }

    pub fn advance_time(&mut self, duration: Duration) {
        self.current_time += duration;
        self.client_apps.iter_mut().for_each(|client_app| {
            client_app.insert_resource(TimeUpdateStrategy::ManualInstant(self.current_time));
        });
        self.server_app
            .insert_resource(TimeUpdateStrategy::ManualInstant(self.current_time));
        #[cfg(feature = "test_utils")]
        mock_instant::global::MockClock::advance(duration);
        #[cfg(not(feature = "test_utils"))]
        std::thread::sleep(duration);
    }

    pub fn flush(&mut self) {
        self.client_apps.iter_mut().for_each(|client_app| {
            client_app.world_mut().flush();
        });
        self.server_app.world_mut().flush();
    }

    /// Advance the world by one frame duration
    pub fn frame_step(&mut self, n: usize) {
        for _ in 0..n {
            self.advance_time(self.frame_duration);
            // we want to log the next frame's tick before the frame starts
            let client_tick = if self.client_entities.is_empty() {
                None
            } else {
                Some(self.client_tick(0) + 1)
            };
            let server_tick = self.server_tick() + 1;
            info!(?client_tick, ?server_tick, "Frame step");
            self.client_apps
                .iter_mut()
                .enumerate()
                .for_each(|(i, client_app)| {
                    error_span!("client", ?i).in_scope(|| client_app.update());
                });
            error_span!("server").in_scope(|| self.server_app.update());
        }
    }

    /// Advance the world by one frame duration
    pub fn frame_step_server_first(&mut self, n: usize) {
        for _ in 0..n {
            self.advance_time(self.frame_duration);
            // we want to log the next frame's tick before the frame starts
            let client_tick = if self.client_entities.is_empty() {
                None
            } else {
                Some(self.client_tick(0) + 1)
            };
            let server_tick = self.server_tick() + 1;
            info!(?client_tick, ?server_tick, "Frame step");
            error_span!("server").in_scope(|| self.server_app.update());
            self.client_apps
                .iter_mut()
                .enumerate()
                .for_each(|(i, client_app)| {
                    error_span!("client", ?i).in_scope(|| client_app.update());
                });
        }
    }

    pub fn tick_step(&mut self, n: usize) {
        for _ in 0..n {
            self.advance_time(self.tick_duration);
            // we want to log the next frame's tick before the frame starts
            let client_tick = if self.client_entities.is_empty() {
                None
            } else {
                Some(self.client_tick(0) + 1)
            };
            let server_tick = self.server_tick() + 1;
            info!(?client_tick, ?server_tick, "Tick step");
            self.client_apps
                .iter_mut()
                .enumerate()
                .for_each(|(i, client_app)| {
                    error_span!("client", ?i).in_scope(|| client_app.update());
                });
            error_span!("server").in_scope(|| self.server_app.update());
        }
    }
}
