#![allow(unused_must_use)]
/*! # Lightyear IO

Low-level IO primitives for the lightyear networking library.
This crate provides abstractions for sending and receiving raw bytes over the network.
*/

#[cfg(test)]
#[cfg(feature = "test_utils")]
mod client_server;
pub mod protocol;
pub mod stepper;

#[cfg(test)]
#[cfg(feature = "test_utils")]
mod timeline;

#[cfg(test)]
mod host_server;
