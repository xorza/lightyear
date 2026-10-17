//! Plugin groups that install Lightyear's layer crates in the order they need.
//!
//! [`client::ClientPlugins`] and [`server::ServerPlugins`] are the entry points. Every type comes
//! from its layer crate, as `lightyear_link::Link` or `lightyear_replication::send::Replicate`;
//! this crate re-exports none of them.
//!
//! ### Feature Flags
#![doc = document_features::document_features!()]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "client")]
pub mod client;

#[cfg(feature = "server")]
pub mod server;

mod shared;

#[cfg(feature = "replication")]
mod protocol;
