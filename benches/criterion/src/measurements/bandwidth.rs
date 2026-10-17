use crate::measurements::recorder::MetricsSnapshot;
use criterion::Throughput;
use criterion::measurement::{Measurement, ValueFormatter};
use lightyear_replication::channels::{RepliconMutationsChannel, RepliconUpdatesChannel};

pub struct Bandwidth;

pub enum BandwidthChannel {
    Total,
    Replication,
}

impl Bandwidth {
    pub fn value(
        metrics: &MetricsSnapshot,
        send: bool,
        recv: bool,
        channel: BandwidthChannel,
    ) -> f64 {
        let (send_name, recv_name, channels): (_, _, &[&'static str]) = match channel {
            BandwidthChannel::Total => ("transport/send_bytes", "transport/recv_bytes", &[]),
            BandwidthChannel::Replication => (
                "channel/send_bytes",
                "channel/recv_bytes",
                &[
                    core::any::type_name::<RepliconUpdatesChannel>(),
                    core::any::type_name::<RepliconMutationsChannel>(),
                ],
            ),
        };
        let bytes = |name| {
            if channels.is_empty() {
                metrics.gauge(name, &[])
            } else {
                channels
                    .iter()
                    .map(|channel| metrics.gauge(name, &[("channel", channel)]))
                    .sum()
            }
        };
        let mut total = 0.0;
        if send {
            total += bytes(send_name);
        }
        if recv {
            total += bytes(recv_name);
        }
        total
    }
}

impl Measurement for Bandwidth {
    type Intermediate = f64;
    type Value = f64;

    fn start(&self) -> Self::Intermediate {
        0.0
    }
    fn end(&self, i: Self::Intermediate) -> Self::Value {
        unimplemented!("unimplement because we use iter_custom")
    }
    fn add(&self, v1: &Self::Value, v2: &Self::Value) -> Self::Value {
        *v1 + *v2
    }
    fn zero(&self) -> Self::Value {
        0.0
    }
    fn to_f64(&self, val: &Self::Value) -> f64 {
        *val
    }
    fn formatter(&self) -> &dyn ValueFormatter {
        &BandwidthFormatter
    }
}

struct BandwidthFormatter;
impl ValueFormatter for BandwidthFormatter {
    fn format_value(&self, value: f64) -> String {
        // The value will be in nanoseconds so we have to convert to half-seconds.
        format!("{:.2} KB", value / 1024.0)
    }

    fn format_throughput(&self, throughput: &Throughput, value: f64) -> String {
        match *throughput {
            Throughput::Bytes(bytes) => format!("{:.2} KB/s", bytes as f64 / (value * 1024.0)),
            Throughput::BytesDecimal(bytes) => {
                format!("{:.2} KB/s", bytes as f64 / (value * 1024.0))
            }
            Throughput::Elements(elems) => format!("{:.2} elem/s", elems as f64 / (value * 1024.0)),
            _ => {
                unimplemented!()
            }
        }
    }

    fn scale_values(&self, ns: f64, values: &mut [f64]) -> &'static str {
        for val in values {
            *val /= 1024.0;
        }
        "KB"
    }

    fn scale_throughputs(
        &self,
        _typical: f64,
        throughput: &Throughput,
        values: &mut [f64],
    ) -> &'static str {
        match *throughput {
            Throughput::Bytes(bytes) => {
                for val in values {
                    *val = (bytes as f64) / (*val * 1024.0)
                }

                "KB/s"
            }
            Throughput::BytesDecimal(bytes) => {
                // Convert nanoseconds/iteration to bytes/half-second.
                for val in values {
                    *val = (bytes as f64) / (*val * 1024.0)
                }
                "KB/s"
            }
            Throughput::Elements(elems) => {
                for val in values {
                    *val = (elems as f64) / (*val * 1024.0)
                }
                "elem/s"
            }
            _ => {
                unimplemented!()
            }
        }
    }

    fn scale_for_machines(&self, values: &mut [f64]) -> &'static str {
        "B"
    }
}
