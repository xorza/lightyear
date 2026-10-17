use crate::protocol::CompFull;
use crate::stepper::*;
use bevy::ecs::hierarchy::ChildOf;
use bevy_replicon::shared::server_entity_map::ServerEntityMap;
use lightyear_connection::network_target::NetworkTarget;
use lightyear_replication::prelude::{PredictionTarget, Replicate};
use test_log::test;
use tracing::info;

/// https://github.com/cBournhonesque/lightyear/issues/627
/// A predicted parent and its child keep their hierarchy on the client: the child's `ChildOf`
/// names the client's entity for the server's parent.
#[test]
fn test_spawn_predicted_with_hierarchy() {
    let mut stepper = ClientServerStepper::from_config(StepperConfig::single());

    let server_child = stepper.server_app.world_mut().spawn_empty().id();
    let server_parent = stepper
        .server_app
        .world_mut()
        .spawn((
            Replicate::to_clients(NetworkTarget::All),
            PredictionTarget::to_clients(NetworkTarget::All),
        ))
        .add_child(server_child)
        .id();
    stepper.frame_step(2);

    // check that the parent and child are spawned on the client
    let predicted_child = stepper.client_apps[0]
        .world()
        .resource::<ServerEntityMap>()
        .to_client()
        .get(&server_child)
        .copied()
        .expect("child entity was not replicated to client");
    let predicted_parent = stepper.client_apps[0]
        .world()
        .resource::<ServerEntityMap>()
        .to_client()
        .get(&server_parent)
        .copied()
        .expect("parent entity was not replicated to client");
    info!("parent: {predicted_parent:?}, child: {predicted_child:?}");

    // check that the parent-child hierarchy is maintained
    assert_eq!(
        stepper
            .client_app()
            .world()
            .get::<ChildOf>(predicted_child)
            .expect("predicted child entity doesn't have a parent")
            .parent(),
        predicted_parent
    );
}

/// https://github.com/cBournhonesque/lightyear/issues/1692
/// A Full-mode component the server inserts MID-GAME on an already-predicted
/// entity — one the client never predicts itself (think a server-authoritative
/// `Dead` marker) — must still reach the Predicted entity, even when the
/// client's prediction never mispredicts.
///
/// Without the fix, the confirmed insert is parked in `ConfirmedHistory<C>`:
/// the completed-checkpoint scan returns early without a retained predicted
/// sample, and `prepare_rollback` skips the component entirely because the
/// entity has no `PredictionHistory<C>` — so the component only ever arrives
/// as a side effect of a rollback caused by something else.
#[test]
fn test_late_server_insert_of_unpredicted_component_reaches_predicted() {
    let mut stepper = ClientServerStepper::from_config(StepperConfig::single());

    // Spawn a predicted entity WITHOUT CompFull.
    let server_entity = stepper
        .server_app
        .world_mut()
        .spawn((
            Replicate::to_clients(NetworkTarget::All),
            PredictionTarget::to_clients(NetworkTarget::All),
        ))
        .id();
    stepper.frame_step(2);

    let predicted = stepper.client_apps[0]
        .world()
        .resource::<ServerEntityMap>()
        .to_client()
        .get(&server_entity)
        .copied()
        .expect("entity was not replicated to client");
    assert!(
        stepper
            .client_app()
            .world()
            .get::<CompFull>(predicted)
            .is_none()
    );

    // Settle well past the spawn so this is a true mid-game insert, not part
    // of the initial sync.
    stepper.frame_step(10);

    // The server inserts the component mid-game. The client has no system
    // predicting CompFull and nothing else mispredicts, so no rollback fires
    // for any other reason.
    stepper
        .server_app
        .world_mut()
        .entity_mut(server_entity)
        .insert(CompFull(42.0));

    // Replication + the completed-mutate-tick scan get time to deliver.
    stepper.frame_step(20);

    assert_eq!(
        stepper.client_app().world().get::<CompFull>(predicted),
        Some(&CompFull(42.0)),
        "server-inserted Full-mode component never reached the Predicted entity (#1692)"
    );
}
