use crate::protocol::StringMessage;
use crate::stepper::*;
use aeronet_io::connection::{LocalAddr, PeerAddr};
use lightyear_connection::prelude::client::*;
use lightyear_connection::server::{Started, Stop, Stopped};
use lightyear_core::prelude::*;
use lightyear_interpolation::prelude::*;
use lightyear_link::crossbeam::CrossbeamIo;
use lightyear_link::prelude::*;
use lightyear_messages::prelude::*;
use lightyear_replication::prelude::*;
use lightyear_sync::prelude::client::*;
use lightyear_sync::prelude::*;
use lightyear_transport::prelude::*;
use test_log::test;

/// Check that the client/server setup is correct:
/// - the various components we expect are present
#[test]
fn test_setup_client_server() {
    let mut stepper = ClientServerStepper::from_config(StepperConfig::single());

    // Check that the various components we expect are present
    assert!(stepper.client(0).contains::<PingManager>());
    assert!(
        stepper
            .client_app()
            .world()
            .contains_resource::<LocalTimelineSync>()
    );
    assert!(!stepper.client(0).contains::<LocalTimelineSync>());
    assert!(stepper.client(0).contains::<RemoteTimeline>());
    assert!(
        stepper
            .client_app()
            .world()
            .contains_resource::<InterpolationTimeline>()
    );
    assert!(!stepper.client(0).contains::<InterpolationTimeline>());
    assert!(stepper.client(0).contains::<Transport>());
    assert!(stepper.client(0).contains::<MessageManager>());
    assert!(stepper.client(0).contains::<MessageSender<StringMessage>>());
    // Message receivers are created lazily when the first payload arrives.
    assert!(
        !stepper
            .client(0)
            .contains::<MessageReceiver<StringMessage>>()
    );
    assert!(stepper.client(0).contains::<ReplicationSender>());
    assert!(stepper.client(0).contains::<CrossbeamIo>());
    assert!(stepper.client(0).contains::<Connected>());
    assert!(stepper.client(0).contains::<LocalAddr>());
    assert!(stepper.client(0).contains::<PeerAddr>());
    assert!(stepper.client(0).contains::<LocalId>());
    assert!(stepper.client(0).contains::<RemoteId>());

    assert!(stepper.server().contains::<Started>());

    assert!(stepper.client_of(0).contains::<Transport>());
    assert!(stepper.client_of(0).contains::<MessageManager>());
    assert!(
        stepper
            .client_of(0)
            .contains::<MessageSender<StringMessage>>()
    );
    assert!(
        !stepper
            .client_of(0)
            .contains::<MessageReceiver<StringMessage>>()
    );
    assert!(stepper.client_of(0).contains::<CrossbeamIo>());
    assert!(stepper.client_of(0).contains::<Connected>());
    assert!(stepper.client_of(0).contains::<PeerAddr>());
    assert!(stepper.client_of(0).contains::<LocalId>());
    assert!(stepper.client_of(0).contains::<RemoteId>());
}

#[test]
fn test_stop_server_unlinks_transport() {
    let mut stepper = ClientServerStepper::from_config(StepperConfig::single());
    let server = stepper.server_entity;

    stepper
        .server_app
        .world_mut()
        .trigger(Stop { entity: server });
    stepper.frame_step_server_first(10);

    let server = stepper.server();
    assert!(server.contains::<Stopped>());
    assert!(server.contains::<Unlinked>());
    assert!(!server.contains::<Linked>());
}

#[test]
fn test_sender_metadata() {
    let stepper = ClientServerStepper::from_config(StepperConfig::single());
    let client = stepper.client(0).id();
    let client_of = stepper.client_of(0).id();

    assert_eq!(
        stepper
            .client_of(0)
            .get::<MessageManager>()
            .unwrap()
            .entity_mapper
            .get_local(client)
            .expect("client is not present in entity map"),
        client_of
    );
    // NOTE: the (client_of -> client) connection pair lives only in the local
    // `MessageManager` map (populated from `SenderMetadata`); replicon's map
    // only tracks replicated entities, so this lookup intentionally does not
    // go through `ServerEntityMap`.
    assert_eq!(
        stepper
            .client(0)
            .get::<MessageManager>()
            .unwrap()
            .entity_mapper
            .get_local(client_of)
            .expect("client_of is not present in entity map"),
        client
    );
}
