use crate::ping::manager::PingManager;
use crate::plugin::SyncSystems;
use crate::timeline::input::{InputTimelineConfig, LocalTimelineSync};
use bevy_app::{App, Last, Plugin, PostUpdate};
use bevy_ecs::prelude::*;
use bevy_reflect::Reflect;
use bevy_time::{Fixed, Time, Virtual};
use bevy_utils::prelude::DebugName;
use core::time::Duration;
use lightyear_connection::client::{Client, Connected, Disconnected};
use lightyear_connection::host::HostClient;
use lightyear_connection::network_topology::{NetworkTopology, NetworkingMetadata};
use lightyear_core::prelude::LocalTimeline;
use lightyear_core::tick::TickDuration;
use lightyear_core::time::TickInstant;
use lightyear_core::timeline::{LocalTimelineShift, NetworkTimeline};
use tracing::trace;

/// Runtime controller for synchronizing a local clock or cursor to a remote timeline.
///
/// Implementors define their synchronization objective and how a large correction is applied.
/// The common controller sequence is provided by [`TimelineSync::sync`]: it waits for enough
/// latency samples, computes the current error, snaps on initial synchronization, and otherwise
/// slews the controlled clock through [`SyncContext`].
///
/// This trait intentionally does not require [`NetworkTimeline`]. [`LocalTimelineSync`] controls
/// the application-global `LocalTimeline` without owning a second clock, while interpolation owns
/// and directly adjusts its independent presentation timeline.
pub trait TimelineSync {
    /// Settings used by this synchronization policy.
    type Config;

    /// Return the mutable hysteresis state used by the common speed controller.
    fn sync_context(&mut self) -> &mut SyncContext;

    /// Select the common controller settings from this policy's configuration.
    fn sync_config(config: &Self::Config) -> &SyncConfig;

    /// Get the ideal [`TickInstant`] that the controlled clock should follow.
    fn sync_objective<Remote: SyncTargetTimeline>(
        &self,
        other: &Remote,
        config: &Self::Config,
        ping_manager: &PingManager,
        tick_duration: Duration,
    ) -> TickInstant;

    /// Apply a discontinuous correction and return its whole-tick delta.
    ///
    /// `now` is explicit because not every controller owns the clock it disciplines.
    fn resync(&mut self, now: TickInstant, sync_objective: TickInstant) -> i32;

    /// Synchronize the controlled clock to `remote` from its current `now` instant.
    ///
    /// Usually this slightly changes the relative speed. Initial synchronization and errors larger
    /// than [`SyncConfig::max_error_margin`] instead call [`TimelineSync::resync`]. The returned
    /// delta lets the caller propagate a discontinuity to any tick-indexed state it owns.
    fn sync<Remote: SyncTargetTimeline>(
        &mut self,
        now: TickInstant,
        remote: &Remote,
        config: &Self::Config,
        ping_manager: &PingManager,
        tick_duration: Duration,
    ) -> Option<i32>
    where
        Self: Sized,
    {
        let sync_config = Self::sync_config(config);
        if ping_manager.latency_samples_recv() < sync_config.handshake_pings as u32 {
            return None;
        }

        let objective = self.sync_objective(remote, config, ping_manager, tick_duration);
        let error_ticks = (now - objective).to_f32();
        let adjustment = if !self.is_synced() {
            SyncAdjustment::Resync
        } else {
            self.speed_adjustment(config, error_ticks)
        };
        trace!(
            controller = ?DebugName::type_name::<Self>(),
            ?now,
            ?objective,
            ?adjustment,
            ?error_ticks,
            error_margin = sync_config.error_margin,
            max_error_margin = sync_config.max_error_margin,
            "timeline synchronization sample"
        );
        self.set_synced(true);

        match adjustment {
            SyncAdjustment::Resync => Some(self.resync(now, objective)),
            SyncAdjustment::SpeedAdjust(_) | SyncAdjustment::DoNothing => None,
        }
    }

    /// Apply the common speed controller for an error measured in fractional ticks.
    ///
    /// Implementations apply speed changes and recovery toward `1.0`. A
    /// [`SyncAdjustment::Resync`] result is left to the synchronization policy.
    fn speed_adjustment(&mut self, config: &Self::Config, offset: f32) -> SyncAdjustment {
        let adjustment = self
            .sync_context()
            .speed_adjustment(Self::sync_config(config), offset);
        match adjustment {
            SyncAdjustment::SpeedAdjust(ratio) => self.set_relative_speed(ratio),
            SyncAdjustment::DoNothing => {
                // Within acceptable margins, gradually return to normal speed.
                let current = self.relative_speed();
                if (current - 1.0).abs() > 0.001 {
                    self.set_relative_speed(current + (1.0 - current) * 0.1);
                }
            }
            SyncAdjustment::Resync => {}
        }
        adjustment
    }

    fn is_synced(&self) -> bool;

    /// Set whether the timeline has completed its initial synchronization.
    fn set_synced(&mut self, synced: bool);

    /// Returns the speed of your timeline relative to your system clock as an `f32`.
    /// A value of `1.0` means the timeline is running at normal speed.
    /// A value of `0.5` means the timeline is running at half speed,
    fn relative_speed(&self) -> f32;

    fn set_relative_speed(&mut self, ratio: f32);

    /// Reset the controller to its initial state (used when a client reconnects).
    fn reset(&mut self);
}

pub trait SyncTargetTimeline: NetworkTimeline + Default {
    fn current_estimate(&self) -> TickInstant;

    /// Returns true if the SyncTimelines are allowed to use this timeline as a sync target this frame
    fn received_packet(&self) -> bool;
}

/// Immutable settings shared by local-clock and interpolation synchronization controllers.
///
/// The controllers keep their mutable hysteresis in [`SyncContext`]. Local simulation uses these
/// settings to pace the application-global `LocalTimeline`; interpolation uses them to pace its
/// independent presentation timeline.
#[derive(Clone, Copy, Debug, Reflect)]
pub struct SyncConfig {
    /// How much multiple of jitter do we apply as margin when computing the time
    /// a packet will get received by the server
    /// (worst case will be RTT / 2 + jitter * multiple_margin + jitter_margin)
    /// % of packets that will be received within k * jitter
    /// 1: 65%, 2: 95%, 3: 99.7%
    pub jitter_multiple: u8,
    /// Fixed safety margin added on top of the jitter-derived one, expressed as a
    /// fractional number of ticks.
    ///
    /// The default is `1.0`, which guarantees the client timeline lands at least
    /// one full tick ahead of the server timeline under zero-RTT/zero-jitter
    /// conditions (e.g. loopback). Any less than `1.0` risks the server
    /// simulating tick `T` before the client's input for `T` has arrived.
    pub jitter_margin: f32,
    /// Number of pings to exchange with the server before finalizing the handshake
    pub handshake_pings: u8,
    /// Error margin for upstream throttle (in multiple of ticks)
    pub error_margin: f32,
    /// If the error is too large, snap the controlled timeline to its objective.
    pub max_error_margin: f32,
    /// How many consecutive errors are allowed before we start adjusting the speed
    pub consecutive_errors_threshold: u8,
    // TODO: instead of constant speedup_factor, the speedup should be linear w.r.t the offset
    /// By how much should we speed up the simulation to make ticks stay in sync with server?
    pub speedup_factor: f32,
}

impl SyncConfig {
    /// Total jitter-based safety margin as a `Duration`, combining the measured
    /// jitter scaled by [`Self::jitter_multiple`] with the fixed fractional-tick
    /// margin in [`Self::jitter_margin`].
    pub fn jitter_margin(&self, jitter: Duration, tick_duration: Duration) -> Duration {
        jitter * self.jitter_multiple as u32 + tick_duration.mul_f32(self.jitter_margin)
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        SyncConfig {
            jitter_multiple: 4,
            jitter_margin: 1.0,
            handshake_pings: 3,
            error_margin: 1.0,
            max_error_margin: 10.0,
            consecutive_errors_threshold: 3,
            speedup_factor: 1.05,
        }
    }
}

/// Mutable hysteresis shared by timeline synchronization controllers.
///
/// This type is public because synchronization policies live in separate Lightyear crates. Its
/// fields and adjustment algorithm remain private; applications normally interact with a
/// [`TimelineSync`] implementation instead.
#[derive(Debug, Reflect)]
pub struct SyncContext {
    /// How many consecutive errors have we seen that are in the same direction
    consecutive_errors: u8,
    /// Sign of the previous error
    previous_error_sign: bool,
}

impl Default for SyncContext {
    fn default() -> Self {
        Self {
            consecutive_errors: 0,
            previous_error_sign: true,
        }
    }
}

#[derive(Debug)]
pub enum SyncAdjustment {
    Resync,
    SpeedAdjust(f32),
    DoNothing,
}

impl SyncContext {
    fn speed_adjustment(&mut self, config: &SyncConfig, offset: f32) -> SyncAdjustment {
        let current_error_sign = offset.is_sign_positive();
        let previous_error_sign = self.previous_error_sign;
        self.previous_error_sign = current_error_sign;
        if offset.abs() > config.max_error_margin {
            self.consecutive_errors = 0;
            SyncAdjustment::Resync
        } else if offset.abs() > config.error_margin {
            self.consecutive_errors = self.consecutive_errors.saturating_add(1);
            // skip if we haven't seen enough consecutive errors in the same direction
            if (current_error_sign ^ previous_error_sign)
                || self.consecutive_errors < config.consecutive_errors_threshold
            {
                self.previous_error_sign = current_error_sign;
                return SyncAdjustment::DoNothing;
            }
            let base_factor = config.speedup_factor - 1.0;
            let error_ratio = (offset.abs() / config.max_error_margin).clamp(0.0, 1.0);

            // Apply progressively stronger adjustment as error increases
            let adjustment = 1.0 + (base_factor * error_ratio * 2.0);

            // Slow down if we are ahead
            let ratio = if offset > 0.0 {
                1.0 / adjustment
            } else {
                adjustment
            };
            SyncAdjustment::SpeedAdjust(ratio)
        } else {
            self.consecutive_errors = 0;
            SyncAdjustment::DoNothing
        }
    }
}

/// Synchronizes the application-global `LocalTimeline` without maintaining a shadow clock.
///
/// `Remote` remains generic so the controller can be tested with deterministic remote estimates.
pub struct LocalTimelineSyncPlugin<Remote> {
    marker: core::marker::PhantomData<Remote>,
}

impl<Remote> Default for LocalTimelineSyncPlugin<Remote> {
    fn default() -> Self {
        Self {
            marker: core::marker::PhantomData,
        }
    }
}

impl<Remote: SyncTargetTimeline> LocalTimelineSyncPlugin<Remote> {
    /// Apply the synchronization controller's pacing factor to Bevy virtual time.
    ///
    /// `LocalTimeline` remains the authoritative tick clock; the synchronization resource only
    /// supplies the relative speed used to make future fixed updates converge on the target.
    fn apply_relative_speed(
        sync: &LocalTimelineSync,
        local_timeline: &LocalTimeline,
        virtual_time: &mut Time<Virtual>,
    ) {
        trace!(
            target: "lightyear_debug::sync",
            kind = "relative_speed",
            schedule = "Last",
            sample_point = "Last",
            timeline = "LocalTimeline",
            tick = local_timeline.tick().0,
            relative_speed = sync.relative_speed(),
            "local timeline synchronization speed applied to Virtual time"
        );
        // TODO: compose this with a relative speed set by the application.
        virtual_time.set_relative_speed(sync.relative_speed());
    }

    /// Reset synchronization controller state when a client session connects.
    fn handle_connect(
        trigger: On<Add, Connected>,
        clients: Query<(), With<Client>>,
        mut sync: ResMut<LocalTimelineSync>,
    ) {
        if clients.get(trigger.entity).is_ok() {
            sync.reset();
        }
    }

    /// Mark an in-process host client synchronized immediately.
    ///
    /// A host client shares the server's world clock and therefore does not need latency samples
    /// or a remote-clock correction before local input systems can run.
    fn handle_host_client(
        trigger: On<Add, HostClient>,
        clients: Query<(), With<Client>>,
        mut sync: ResMut<LocalTimelineSync>,
    ) {
        if clients.get(trigger.entity).is_ok() {
            sync.set_synced(true);
        }
    }

    /// Clear client readiness when its session disconnects.
    fn handle_disconnect(
        trigger: On<Add, Disconnected>,
        clients: Query<(), With<Client>>,
        mut sync: ResMut<LocalTimelineSync>,
    ) {
        if clients.get(trigger.entity).is_ok() {
            sync.set_synced(false);
        }
    }

    /// Apply local-clock pacing while a client or host client is synchronized, else normal speed.
    fn update_virtual_time(
        metadata: Res<NetworkingMetadata>,
        local_timeline: Res<LocalTimeline>,
        sync: Res<LocalTimelineSync>,
        mut virtual_time: ResMut<Time<Virtual>>,
    ) {
        if sync.is_synced()
            && matches!(
                metadata.mode,
                NetworkTopology::Client(_) | NetworkTopology::HostClient { .. }
            )
        {
            Self::apply_relative_speed(&sync, &local_timeline, &mut virtual_time);
        } else {
            virtual_time.set_relative_speed(1.0);
        }
    }

    /// Synchronize the local simulation instant against one conventional remote clock estimate.
    ///
    /// This preserves the previous `InputTimeline` behavior: wait for latency samples, compute an
    /// objective that includes network margin and subtracts input delay, slew for small errors,
    /// and emit a whole-tick [`LocalTimelineShift`] for large errors.
    fn sync_client_server(
        source: Entity,
        local_now: TickInstant,
        sync: &mut LocalTimelineSync,
        config: &InputTimelineConfig,
        remote: &Remote,
        ping_manager: &PingManager,
        tick_duration: &TickDuration,
        commands: &mut Commands,
    ) {
        if !remote.received_packet() {
            return;
        }
        let remote_estimate = remote.current_estimate();
        let before_speed = sync.relative_speed();
        if let Some(tick_delta) =
            sync.sync(local_now, remote, config, ping_manager, tick_duration.0)
        {
            trace!(
                target: "lightyear_debug::sync",
                kind = "sync_adjustment",
                schedule = "PostUpdate",
                sample_point = "PostUpdate",
                ?source,
                timeline = "LocalTimeline",
                remote_timeline = ?DebugName::type_name::<Remote>(),
                local_tick = local_now.tick().0,
                remote_tick = remote.tick().0,
                remote_estimate = ?remote_estimate,
                tick_delta,
                relative_speed = sync.relative_speed(),
                rtt_ms = ping_manager.rtt().as_secs_f64() * 1000.0,
                jitter_ms = ping_manager.jitter().as_secs_f64() * 1000.0,
                "local timeline sync emitted LocalTimelineShift"
            );
            commands.trigger(LocalTimelineShift { delta: tick_delta });
        } else {
            trace!(
                target: "lightyear_debug::sync",
                kind = "sync_sample",
                schedule = "PostUpdate",
                sample_point = "PostUpdate",
                ?source,
                timeline = "LocalTimeline",
                remote_timeline = ?DebugName::type_name::<Remote>(),
                local_tick = local_now.tick().0,
                remote_tick = remote.tick().0,
                remote_estimate = ?remote_estimate,
                relative_speed_before = before_speed,
                relative_speed = sync.relative_speed(),
                rtt_ms = ping_manager.rtt().as_secs_f64() * 1000.0,
                jitter_ms = ping_manager.jitter().as_secs_f64() * 1000.0,
                "local timeline sync sampled"
            );
        }
    }

    /// Select the topology's clock source and update [`LocalTimelineSync`].
    ///
    /// A client follows its sole authoritative server; a host client shares the server's clock
    /// and is synchronized at once.
    fn sync_timelines(
        tick_duration: Res<TickDuration>,
        fixed_time: Res<Time<Fixed>>,
        metadata: Res<NetworkingMetadata>,
        config: Res<InputTimelineConfig>,
        local_timeline: Res<LocalTimeline>,
        mut sync: ResMut<LocalTimelineSync>,
        remotes: Query<
            (&Remote, &PingManager),
            (With<Client>, With<Connected>, Without<HostClient>),
        >,
        mut commands: Commands,
    ) {
        let mut is_synced = sync.is_synced();
        let local_now = local_timeline.instant(&fixed_time);

        if metadata.is_changed() {
            sync.reset();
            sync.set_synced(false);
            is_synced = false;
        }

        match &metadata.mode {
            NetworkTopology::Client(client) => {
                let Ok((remote, ping_manager)) = remotes.get(*client) else {
                    return;
                };
                Self::sync_client_server(
                    *client,
                    local_now,
                    &mut sync,
                    &config,
                    remote,
                    ping_manager,
                    &tick_duration,
                    &mut commands,
                );
            }
            NetworkTopology::HostClient { .. } if !is_synced => {
                sync.set_synced(true);
            }
            _ => {}
        }
    }

    /// Apply a whole-tick synchronization correction to the authoritative [`LocalTimeline`].
    ///
    /// Input, prediction, and prespawn observers receive the same event and shift their indexed
    /// histories consistently with the world clock.
    fn handle_local_timeline_shift(
        trigger: On<LocalTimelineShift>,
        mut local_timeline: ResMut<LocalTimeline>,
    ) {
        local_timeline.apply_delta(trigger.delta);
        trace!(
            target: "lightyear_debug::sync",
            kind = "local_timeline_shift_apply",
            schedule = "PostUpdate",
            sample_point = "PostUpdate",
            timeline = "LocalTimeline",
            tick_delta = trigger.delta,
            local_tick = local_timeline.tick().0,
            "applied LocalTimelineShift to LocalTimeline"
        );
    }
}

impl<Remote: SyncTargetTimeline> Plugin for LocalTimelineSyncPlugin<Remote> {
    fn build(&self, app: &mut App) {
        app.init_resource::<LocalTimelineSync>();
        app.init_resource::<InputTimelineConfig>();

        app.add_observer(Self::handle_connect);
        app.add_observer(Self::handle_host_client);
        app.add_observer(Self::handle_disconnect);
        app.add_observer(Self::handle_local_timeline_shift);
        app.add_systems(PostUpdate, Self::sync_timelines.in_set(SyncSystems::Sync));
        app.add_systems(Last, Self::update_virtual_time);
    }
}
