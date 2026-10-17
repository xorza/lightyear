use bevy_app::{PluginGroup, PluginGroupBuilder};

use crate::shared::SharedPlugins;
use core::time::Duration;
#[cfg(feature = "replication")]
use lightyear_replication::LightyearRepliconClientBackend;

/// A plugin group containing all the client plugins.
///
/// The order in which the plugins are added matters!
/// You need to add:
/// - first add the `ClientPlugins`
/// - then build your protocol (usually in a `ProtocolPlugin`)
/// - then spawn your `Client` entity
pub struct ClientPlugins {
    /// The tick interval for the client. This is used to determine how often the client should tick.
    /// The default value is 1/60 seconds.
    pub tick_duration: Duration,
}

impl Default for ClientPlugins {
    fn default() -> Self {
        Self {
            tick_duration: Duration::from_secs_f32(1.0 / 60.0),
        }
    }
}

impl PluginGroup for ClientPlugins {
    fn build(self) -> PluginGroupBuilder {
        let builder = PluginGroupBuilder::start::<Self>();

        let builder = builder.add(lightyear_sync::client::ClientPlugin);

        let builder = builder.add(SharedPlugins {
            tick_duration: self.tick_duration,
        });

        #[cfg(feature = "replication")]
        let builder = builder.add(LightyearRepliconClientBackend);

        #[cfg(feature = "prediction")]
        let builder = builder.add(lightyear_prediction::plugin::PredictionPlugin);

        builder.add(lightyear_connection::raw::client::RawConnectionPlugin)
    }
}
