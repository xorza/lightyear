//! The bench process's metrics recorder, which reads back what Lightyear emits.

use std::collections::HashMap;
use std::sync::LazyLock;

use metrics::Key;
use metrics_util::debugging::{DebugValue, DebuggingRecorder, Snapshotter};
use metrics_util::{CompositeKey, MetricKind};

/// The `metrics` crate holds one recorder per process, so every bench shares this one.
static SNAPSHOTTER: LazyLock<Snapshotter> = LazyLock::new(|| {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    recorder
        .install()
        .expect("no other metrics recorder is installed in a bench process");
    snapshotter
});

/// The metrics recorded since the previous [`MetricsSnapshot::take`].
pub struct MetricsSnapshot(HashMap<CompositeKey, DebugValue>);

impl MetricsSnapshot {
    /// Takes every value recorded since the last call and resets the recorder, so the next
    /// snapshot holds only what comes after this one.
    pub fn take() -> Self {
        Self(
            SNAPSHOTTER
                .snapshot()
                .into_vec()
                .into_iter()
                .map(|(key, _, _, value)| (key, value))
                .collect(),
        )
    }

    pub fn gauge(&self, name: &'static str, labels: &[(&'static str, &'static str)]) -> f64 {
        match self.0.get(&CompositeKey::new(
            MetricKind::Gauge,
            Key::from_parts(name, labels),
        )) {
            Some(DebugValue::Gauge(value)) => value.into_inner(),
            _ => 0.0,
        }
    }

    pub fn counter(&self, name: &'static str) -> f64 {
        match self.0.get(&CompositeKey::new(
            MetricKind::Counter,
            Key::from_name(name),
        )) {
            Some(DebugValue::Counter(value)) => *value as f64,
            _ => 0.0,
        }
    }
}
