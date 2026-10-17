//! Defines bevy resources needed for Prediction

use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;

use crate::correction::CorrectionPolicy;
use crate::rollback::RollbackState;
use core::ops::{Deref, DerefMut};
use lightyear_core::prelude::Tick;
use lightyear_sync::prelude::InputTimelineConfig;
use parking_lot::RwLock;

#[derive(Debug, Clone, Copy, Default, Reflect)]
pub enum RollbackMode {
    /// We always rollback on newly received confirmed state, without comparing it with the
    /// predicted history.
    ///
    /// It can be useful to always do rollbacks to test that your game can handle the CPU demand of doing the
    /// frequent rollbacks. This also unlocks perf optimizations such as not storing a PredictionHistory.
    Always,
    #[default]
    /// We check if we should rollback by comparing with a previous value.
    Check,
    /// We don't rollback or do any checks.
    Disabled,
}

#[derive(Debug, Clone, Copy, Reflect)]
/// The RollbackPolicy defines how we check and trigger rollbacks.
pub struct RollbackPolicy {
    /// How a confirmed state that differs from the prediction triggers a rollback.
    pub state: RollbackMode,
    /// Upper bound on the number of ticks we can roll back.
    ///
    /// A non-zero [`InputTimelineConfig::maximum_predicted_ticks`] can lower the effective bound.
    /// Rollback requests beyond the effective bound are ignored.
    pub max_rollback_ticks: u16,
}

impl Default for RollbackPolicy {
    fn default() -> Self {
        Self {
            state: RollbackMode::Check,
            max_rollback_ticks: 20,
        }
    }
}

impl RollbackPolicy {
    /// Maximum rollback depth after applying the input timing configuration's prediction limit.
    ///
    /// A non-zero `maximum_predicted_ticks` bounds how far ahead the local simulation can be, so
    /// rollback state older than that cannot be needed. Zero denotes lockstep and does not cap
    /// explicit or forced state rollbacks.
    pub fn effective_max_rollback_ticks(&self, input_config: &InputTimelineConfig) -> u16 {
        let maximum_predicted_ticks = input_config.maximum_predicted_ticks();
        if maximum_predicted_ticks == 0 {
            self.max_rollback_ticks
        } else {
            self.max_rollback_ticks.min(maximum_predicted_ticks)
        }
    }
}

/// Application-global state that enables prediction and rollback.
///
/// [`PredictionPlugin`](crate::prelude::PredictionPlugin) installs the prediction systems, but
/// those systems only run when this resource is present and the cached network topology is a
/// client. Insert one manager into the application when it should
/// create predicted entities, record prediction history, and perform one global
/// rollback/reconciliation pipeline.
#[derive(Resource, Debug, Reflect)]
pub struct PredictionManager {
    /// Configuration for how rollbacks are triggered
    pub rollback_policy: RollbackPolicy,
    /// Configuration for smoothing the rollback error over time
    pub correction_policy: CorrectionPolicy,
    #[doc(hidden)]
    pub local_despawn: Vec<(Tick, Entity)>,
    #[doc(hidden)]
    pub local_skip_despawn: Vec<(Tick, Entity)>,
    #[doc(hidden)]
    #[reflect(ignore)]
    pub rollback: RwLock<RollbackState>,
}

/// Stores metadata related to state-based prediction.
#[derive(Resource, Clone, Copy, Debug, Default, Reflect)]
pub struct StateRollbackMetadata {
    /// Latest completed server mutate tick consumed by the rollback check.
    ///
    /// Replicon advances `ServerMutateTicks` during receive; this advances only after
    /// `check_rollback` handles that frontier. We retain it separately to:
    /// - avoid rescanning entities while the completed frontier is unchanged;
    /// - provide a safe watermark for pruning diff history.
    ///
    /// This tick is always in the past or present relative to the local simulation tick when it is
    /// stored. It can lag behind Replicon's globally latest completed checkpoint when that
    /// checkpoint is still in the local simulation's future, or when an unresolved predicted diff
    /// makes a newer checkpoint unsafe to process.
    last_processed_confirmed_tick: Option<Tick>,

    /// Set to true if we received any replication message this frame.
    /// Used to trigger `RollbackMode::Always`.
    pub(crate) received_messages_this_frame: bool,

    /// Tick at which an external caller has requested a one-shot rollback.
    ///
    /// Consumed by `check_rollback` regardless of the `rollback_policy.state`
    /// setting — this is an explicit request, not a mismatch-triggered one.
    /// Set via [`StateRollbackMetadata::request_forced_rollback`]. Cleared
    /// when consumed.
    pub(crate) forced_rollback_tick: Option<Tick>,
}

impl StateRollbackMetadata {
    /// Mark the current frame as having received replicated state.
    #[cfg(feature = "test_utils")]
    #[doc(hidden)]
    pub fn mark_received_messages_this_frame(&mut self) {
        self.received_messages_this_frame = true;
    }

    /// Request a one-shot rollback from `tick`, regardless of the
    /// `rollback_policy.state` mode.
    ///
    /// Intended for scenarios where an external system (e.g. a snapshot an app
    /// applies to a late-joining client) has deposited confirmed state at a specific tick and
    /// needs the simulation to re-run from there. Subsequent calls within the
    /// same frame take the earliest tick.
    pub fn request_forced_rollback(&mut self, tick: Tick) {
        match self.forced_rollback_tick {
            None => self.forced_rollback_tick = Some(tick),
            Some(existing) if tick < existing => self.forced_rollback_tick = Some(tick),
            _ => {}
        }
    }

    /// Tick at which a one-shot rollback has been requested but not yet
    /// consumed by `check_rollback`. While this is `Some`, prediction
    /// history buffers on the entities targeted by the rollback must not
    /// be mutated (e.g. checksum systems that use destructive reads must
    /// skip), otherwise `prepare_rollback` won't find the restore value.
    pub fn forced_rollback_tick(&self) -> Option<Tick> {
        self.forced_rollback_tick
    }

    /// Reset all connection-scoped rollback metadata.
    pub(crate) fn reset_connection_state(&mut self) {
        *self = Self::default();
    }

    /// Reset the per-frame state tracking.
    pub(crate) fn reset_frame_state(&mut self) {
        self.received_messages_this_frame = false;
    }

    /// Returns the latest completed server mutate tick consumed by rollback checking.
    ///
    /// During Replicon's receive systems this still reflects the previous rollback-check frontier;
    /// it is advanced only after `check_rollback` handles a completed checkpoint that is at or
    /// before the local simulation tick.
    /// It is used to avoid rescanning an already-consumed completed tick. This is a processing
    /// watermark, not the target of a rollback currently in progress; that target is stored in
    /// [`PredictionManager`] and returned by [`PredictionManager::get_rollback_start_tick`].
    pub fn last_processed_confirmed_tick(&self) -> Option<Tick> {
        self.last_processed_confirmed_tick
    }

    /// Record that rollback checking consumed this completed server mutate tick.
    ///
    /// Call this only after `check_rollback` consumes the checkpoint: after the full state scan in
    /// `RollbackMode::Check`, or after scheduling its unconditional rollback in
    /// `RollbackMode::Always`. Do not advance it directly from `ServerMutateTicks`, while the
    /// completed tick is still in the client's future, or while a diff at or before this checkpoint
    /// is unresolved.
    pub fn set_last_processed_confirmed_tick(&mut self, tick: Tick) {
        self.last_processed_confirmed_tick = Some(tick);
    }

    /// Check if the completed mutate tick has advanced since we last processed it.
    ///
    /// If this returns false, `check_rollback` can skip the full state scan because the current
    /// completed checkpoint was already handled on an earlier frame.
    pub fn has_confirmed_tick_advanced(&self, current_tick: Tick) -> bool {
        match self.last_processed_confirmed_tick {
            None => true, // First time, always process
            Some(last) => current_tick > last,
        }
    }
}

impl Default for PredictionManager {
    fn default() -> Self {
        Self {
            rollback_policy: RollbackPolicy::default(),
            correction_policy: CorrectionPolicy::default(),
            local_skip_despawn: Vec::default(),
            local_despawn: Vec::default(),
            rollback: RwLock::new(RollbackState::Default),
        }
    }
}

// SAFETY: We never use UnsafeCell to mutate the predicted_entity_map, so it's safe to send and sync
unsafe impl Send for PredictionManager {}
unsafe impl Sync for PredictionManager {}

impl PredictionManager {
    /// Returns true if we are currently in a rollback state
    pub fn is_rollback(&self) -> bool {
        match *self.rollback.read().deref() {
            RollbackState::RollbackStart { .. } => true,
            RollbackState::Default => false,
        }
    }

    /// Get the current rollback tick
    pub fn get_rollback_start_tick(&self) -> Option<Tick> {
        match *self.rollback.read().deref() {
            RollbackState::RollbackStart(start_tick) => Some(start_tick),
            RollbackState::Default => None,
        }
    }

    /// Set the rollback state back to non-rollback
    pub fn set_non_rollback(&self) {
        *self.rollback.write().deref_mut() = RollbackState::Default;
    }

    /// Set the rollback state to `ShouldRollback` with the given tick.
    pub fn set_rollback_tick(&self, tick: Tick) {
        *self.rollback.write().deref_mut() = RollbackState::RollbackStart(tick)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightyear_sync::timeline::input::InputDelayConfig;

    #[test]
    fn effective_max_rollback_ticks_respects_prediction_limit() {
        let mut policy = RollbackPolicy::default();
        assert_eq!(policy.max_rollback_ticks, 20);

        let balanced =
            InputTimelineConfig::default().with_input_delay(InputDelayConfig::balanced());
        assert_eq!(policy.effective_max_rollback_ticks(&balanced), 7);

        policy.max_rollback_ticks = 5;
        assert_eq!(policy.effective_max_rollback_ticks(&balanced), 5);

        let lockstep =
            InputTimelineConfig::default().with_input_delay(InputDelayConfig::no_prediction());
        assert_eq!(policy.effective_max_rollback_ticks(&lockstep), 5);
    }

    #[test]
    fn confirmed_tick_advancement_uses_last_processed_confirmed_tick() {
        let mut metadata = StateRollbackMetadata::default();
        assert!(metadata.has_confirmed_tick_advanced(Tick(10)));

        metadata.set_last_processed_confirmed_tick(Tick(10));
        assert!(!metadata.has_confirmed_tick_advanced(Tick(10)));
        assert!(!metadata.has_confirmed_tick_advanced(Tick(9)));
        assert!(metadata.has_confirmed_tick_advanced(Tick(11)));
    }

    #[test]
    fn server_mutate_last_tick_can_be_newer_than_latest_complete_tick() {
        use bevy_replicon::client::server_mutate_ticks::ServerMutateTicks;
        use bevy_replicon::prelude::RepliconTick;

        let complete_tick = RepliconTick::new(9);
        let incomplete_tick = RepliconTick::new(10);

        let mut server_mutate_ticks = ServerMutateTicks::default();
        assert!(server_mutate_ticks.confirm(complete_tick, 1));
        assert!(!server_mutate_ticks.confirm(incomplete_tick, 2));

        assert_eq!(server_mutate_ticks.last_tick(), incomplete_tick);
        assert!(server_mutate_ticks.contains(complete_tick));
        assert!(!server_mutate_ticks.contains(incomplete_tick));
    }
}
