//! Fragment validation, reassembly, and decompression for channel receives.

use bevy_platform::collections::HashMap;

use crate::channel::receive::ChannelReceiveError;

type Result<T> = core::result::Result<T, ChannelReceiveError>;
#[cfg(feature = "compression_lz4")]
use crate::packet::compression::CompressionAlgorithm;
use crate::packet::compression::{CompressionConfig, decompress_payload};
use crate::packet::error::PacketError;
use crate::packet::message::{FragmentCompression, FragmentData, MessageId};
use bytes::Bytes;
use core::time::Duration;
use lightyear_core::tick::Tick;
use tracing::trace;

/// `FragmentReceiver` is used to reconstruct fragmented messages
///
/// Each sender derives its fragment size from its own link, so the receiver takes the size of a
/// message's fragments from those fragments instead of from its own link.
#[derive(Debug)]
pub struct FragmentReceiver {
    fragment_messages: HashMap<MessageId, FragmentConstructor>,
}

impl FragmentReceiver {
    pub fn new() -> Self {
        Self {
            fragment_messages: HashMap::default(),
        }
    }

    /// Discard all messages for which the latest fragment was received before the cleanup time
    /// (i.e. we probably lost some fragments and we will never complete the message)
    ///
    /// If we don't keep track of the last received time, we will never clean up the messages.
    pub fn cleanup(&mut self, cleanup_time: Duration) {
        self.fragment_messages.retain(|_, c| {
            c.last_received
                .map(|t| t > cleanup_time)
                .unwrap_or_else(|| true)
        })
    }

    /// Receive a fragment of a FragmentData message.
    ///
    /// When we complete the final message by aggregating all fragments, we will return the
    /// `remote_sent_tick` associated with the first fragment received.
    pub fn receive_fragment(
        &mut self,
        fragment: FragmentData,
        remote_sent_tick: Tick,
        current_time: Option<Duration>,
        compression: CompressionConfig,
    ) -> Result<Option<(Tick, Bytes)>> {
        let num_fragments = fragment.num_fragments.0;
        let fragment_index = fragment.fragment_id.0;
        if num_fragments == 0 {
            return Err(ChannelReceiveError::InvalidFragmentCount { num_fragments });
        }
        if fragment_index >= num_fragments {
            return Err(ChannelReceiveError::InvalidFragmentIndex {
                fragment_index,
                num_fragments,
            });
        }
        let num_fragments = usize::try_from(num_fragments)
            .map_err(|_| ChannelReceiveError::FragmentedMessageSizeOverflow)?;
        let fragment_index = usize::try_from(fragment_index)
            .map_err(|_| ChannelReceiveError::FragmentedMessageSizeOverflow)?;

        let message_id = fragment.message_id;
        let fragment_compression = fragment.compression;
        let fragment_message = self
            .fragment_messages
            .entry(message_id)
            .or_insert_with(|| FragmentConstructor::new(remote_sent_tick, num_fragments));
        if fragment_message.num_fragments != num_fragments {
            return Err(ChannelReceiveError::FragmentCountMismatch {
                expected: fragment_message.num_fragments,
                actual: num_fragments,
            });
        }
        if let Some(fragment_compression) = fragment_compression {
            fragment_message.set_compression(fragment_compression)?;
        } else if fragment_index == 0 {
            return Err(ChannelReceiveError::MissingFragmentCompression);
        }

        if let Some(payload) =
            fragment_message.receive_fragment(fragment_index, fragment.bytes, current_time)?
        {
            let fragment_compression = fragment_message
                .compression
                .ok_or(ChannelReceiveError::MissingFragmentCompression)?;
            self.fragment_messages.remove(&message_id);
            let (tick, payload) = payload;
            return decompress_fragment_payload(fragment_compression, payload, compression)
                .map(|payload| Some((tick, payload)));
        }

        Ok(None)
    }
}

#[derive(Debug, Clone)]
/// Data structure to reconstruct a single fragmented message from individual fragments
pub struct FragmentConstructor {
    num_fragments: usize,
    num_received_fragments: usize,
    received: Vec<bool>,
    layout: FragmentLayout,

    tick: Tick,
    compression: Option<FragmentCompression>,
    last_received: Option<Duration>,
}

/// Where each fragment of a message goes in the reassembled message.
#[derive(Debug, Clone)]
enum FragmentLayout {
    /// No non-final fragment was received, so the sender's fragment size is not known yet. The
    /// final fragment waits here when it arrives first, because its offset depends on that size.
    Unknown {
        final_fragment: Option<Bytes>,
    },
    Known(ReassemblyBuffer),
}

/// The message bytes, with the fragment size of the first non-final fragment received. The sender
/// splits a message into fragments of one size, so every other non-final fragment must have it too.
#[derive(Debug, Clone)]
struct ReassemblyBuffer {
    fragment_size: usize,
    bytes: Vec<u8>,
}

impl FragmentConstructor {
    pub fn new(tick: Tick, num_fragments: usize) -> Self {
        Self {
            num_fragments,
            num_received_fragments: 0,
            received: vec![false; num_fragments],
            layout: FragmentLayout::Unknown {
                final_fragment: None,
            },
            tick,
            compression: None,
            last_received: None,
        }
    }

    pub fn set_compression(&mut self, compression: FragmentCompression) -> Result<()> {
        if let Some(expected) = self.compression {
            if expected != compression {
                return Err(ChannelReceiveError::FragmentCompressionMismatch {
                    expected: expected.as_str(),
                    actual: compression.as_str(),
                });
            }
        } else {
            self.compression = Some(compression);
        }
        Ok(())
    }

    pub fn receive_fragment(
        &mut self,
        fragment_index: usize,
        bytes: Bytes,
        received_time: Option<Duration>,
    ) -> Result<Option<(Tick, Bytes)>> {
        self.last_received = received_time;

        let is_last_fragment = fragment_index == self.num_fragments - 1;
        if !is_last_fragment {
            self.learn_fragment_size(bytes.len())?;
        } else if let FragmentLayout::Known(buffer) = &self.layout
            && bytes.len() > buffer.fragment_size
        {
            return Err(ChannelReceiveError::InvalidFinalFragmentSize {
                actual: bytes.len(),
                max: buffer.fragment_size,
            });
        }

        if self.received[fragment_index] {
            return Ok(None);
        }
        self.received[fragment_index] = true;
        self.num_received_fragments += 1;

        match &mut self.layout {
            FragmentLayout::Known(buffer) => buffer.write(fragment_index, is_last_fragment, &bytes),
            // The only fragment is also the whole message.
            FragmentLayout::Unknown { .. } if self.num_fragments == 1 => {
                return Ok(Some((self.tick, bytes)));
            }
            FragmentLayout::Unknown { final_fragment } => *final_fragment = Some(bytes),
        }

        if self.num_received_fragments == self.num_fragments {
            trace!("Received all fragments!");
            let FragmentLayout::Known(buffer) = &mut self.layout else {
                unreachable!("a message of several fragments has a non-final fragment");
            };
            let payload = core::mem::take(&mut buffer.bytes);
            return Ok(Some((self.tick, payload.into())));
        }

        Ok(None)
    }

    /// Checks a non-final fragment's size against the message's fragment size, or makes it the
    /// message's fragment size if it is the first non-final fragment.
    fn learn_fragment_size(&mut self, fragment_size: usize) -> Result<()> {
        let final_fragment = match &mut self.layout {
            FragmentLayout::Known(buffer) => {
                return if fragment_size == buffer.fragment_size {
                    Ok(())
                } else {
                    Err(ChannelReceiveError::InvalidNonFinalFragmentSize {
                        actual: fragment_size,
                        expected: buffer.fragment_size,
                    })
                };
            }
            FragmentLayout::Unknown { final_fragment } => final_fragment,
        };
        if fragment_size == 0 {
            return Err(ChannelReceiveError::EmptyNonFinalFragment);
        }
        if let Some(final_fragment) = final_fragment
            && final_fragment.len() > fragment_size
        {
            return Err(ChannelReceiveError::InvalidFinalFragmentSize {
                actual: final_fragment.len(),
                max: fragment_size,
            });
        }
        let max_message_size = self
            .num_fragments
            .checked_mul(fragment_size)
            .ok_or(ChannelReceiveError::FragmentedMessageSizeOverflow)?;

        let mut buffer = ReassemblyBuffer {
            fragment_size,
            bytes: vec![0; max_message_size],
        };
        if let Some(final_fragment) = final_fragment.take() {
            buffer.write(self.num_fragments - 1, true, &final_fragment);
        }
        self.layout = FragmentLayout::Known(buffer);
        Ok(())
    }
}

impl ReassemblyBuffer {
    /// `fragment` must hold `fragment_size` bytes, or at most that many if it is the final one.
    fn write(&mut self, fragment_index: usize, is_last_fragment: bool, fragment: &[u8]) {
        // Cannot overflow: the buffer holds `num_fragments * fragment_size` bytes until the final
        // fragment shortens it, and every non-final fragment lies before the final one.
        let start = fragment_index * self.fragment_size;
        let end = start + fragment.len();
        if is_last_fragment {
            self.bytes.truncate(end);
        }
        self.bytes[start..end].copy_from_slice(fragment);
    }
}

fn decompress_fragment_payload(
    compression: FragmentCompression,
    payload: Bytes,
    config: CompressionConfig,
) -> Result<Bytes> {
    match compression {
        FragmentCompression::None => Ok(payload),
        FragmentCompression::Lz4 => {
            let config = config_for_fragment_compression(compression, config)?;
            decompress_payload(payload.as_ref(), config)
                .map(Bytes::from)
                .map_err(map_fragment_decompression_error)
        }
    }
}

fn config_for_fragment_compression(
    compression: FragmentCompression,
    config: CompressionConfig,
) -> Result<CompressionConfig> {
    match compression {
        FragmentCompression::None => Ok(config),
        FragmentCompression::Lz4 => {
            #[cfg(feature = "compression_lz4")]
            {
                if config.algorithm == Some(CompressionAlgorithm::Lz4) {
                    Ok(config)
                } else {
                    Err(ChannelReceiveError::UnsupportedFragmentCompression {
                        compression: compression.as_str(),
                    })
                }
            }
            #[cfg(not(feature = "compression_lz4"))]
            {
                Err(ChannelReceiveError::UnsupportedFragmentCompression {
                    compression: compression.as_str(),
                })
            }
        }
    }
}

fn map_fragment_decompression_error(error: PacketError) -> ChannelReceiveError {
    match error {
        PacketError::UnsupportedCompression => {
            ChannelReceiveError::UnsupportedFragmentCompression {
                compression: FragmentCompression::Lz4.as_str(),
            }
        }
        PacketError::DecompressionFailed => ChannelReceiveError::FragmentDecompressionFailed,
        PacketError::DecompressedPayloadTooLarge { actual, limit } => {
            ChannelReceiveError::FragmentDecompressedPayloadTooLarge { actual, limit }
        }
        _ => ChannelReceiveError::FragmentDecompressionFailed,
    }
}

#[cfg(test)]
mod tests {
    use crate::channel::fragments::FragmentSender;
    #[cfg(feature = "compression_lz4")]
    use crate::packet::compression::CompressionScratch;
    use crate::packet::message::FragmentIndex;
    use crate::packet::packet::FRAGMENT_SIZE;

    use super::*;

    #[test]
    fn test_receiver() -> Result<()> {
        let mut receiver = FragmentReceiver::new();
        let num_bytes = (FRAGMENT_SIZE as f32 * 1.5) as usize;
        let message_bytes = Bytes::from(vec![1u8; num_bytes]);
        let fragments = FragmentSender::new().build_fragments(MessageId(0), message_bytes.clone());

        assert_eq!(
            receiver.receive_fragment(
                fragments[1].clone(),
                Tick(1),
                None,
                CompressionConfig::DISABLED
            )?,
            None
        );
        assert_eq!(
            receiver.receive_fragment(
                fragments[0].clone(),
                Tick(0),
                None,
                CompressionConfig::DISABLED
            )?,
            Some((Tick(1), message_bytes.clone()))
        );
        Ok(())
    }

    /// The receiver is never told the sender's fragment size, which here is smaller than, equal to
    /// and larger than the default. Every message is `2 * fragment_size + 26` bytes, so it splits
    /// into fragments of `fragment_size`, `fragment_size` and 26 bytes, received in every order.
    #[test]
    fn reassembles_fragments_of_any_sender_fragment_size_in_any_order() -> Result<()> {
        const ORDERS: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for fragment_size in [37, FRAGMENT_SIZE, 2 * FRAGMENT_SIZE] {
            let mut sender = FragmentSender::new();
            sender.set_fragment_size(fragment_size);
            // Every byte differs from its neighbours, so a fragment written at a wrong offset
            // changes the message.
            let message_bytes: Bytes = (0..2 * fragment_size + 26)
                .map(|i| (i % 251) as u8)
                .collect::<Vec<u8>>()
                .into();
            let fragments = sender.build_fragments(MessageId(4), message_bytes.clone());
            assert_eq!(
                fragments
                    .iter()
                    .map(|fragment| fragment.bytes.len())
                    .collect::<Vec<_>>(),
                [fragment_size, fragment_size, 26]
            );

            for order in ORDERS {
                let mut receiver = FragmentReceiver::new();
                let mut result = None;
                for index in order {
                    assert_eq!(result, None, "size {fragment_size}, order {order:?}");
                    result = receiver.receive_fragment(
                        fragments[index].clone(),
                        Tick(index as u32),
                        None,
                        CompressionConfig::DISABLED,
                    )?;
                }
                assert_eq!(
                    result,
                    Some((Tick(order[0] as u32), message_bytes.clone())),
                    "size {fragment_size}, order {order:?}"
                );
            }
        }
        Ok(())
    }

    /// Feeds hand-made fragments of a message with `num_fragments` fragments, given as
    /// `(fragment index, byte count)`, and returns the result of the last one.
    fn receive_fragments(
        num_fragments: u64,
        fragments: &[(u64, usize)],
    ) -> Result<Option<(Tick, Bytes)>> {
        let mut receiver = FragmentReceiver::new();
        let mut result = Ok(None);
        for &(index, len) in fragments {
            assert!(matches!(result, Ok(None)), "{fragments:?}: {result:?}");
            let fragment = FragmentData {
                message_id: MessageId(9),
                fragment_id: FragmentIndex(index),
                num_fragments: FragmentIndex(num_fragments),
                compression: (index == 0).then_some(FragmentCompression::None),
                bytes: Bytes::from(vec![index as u8; len]),
            };
            result =
                receiver.receive_fragment(fragment, Tick(0), None, CompressionConfig::DISABLED);
        }
        result
    }

    #[test]
    fn fragment_sizes_must_match_the_first_non_final_fragment() {
        assert!(matches!(
            receive_fragments(3, &[(0, 37), (1, 36)]),
            Err(ChannelReceiveError::InvalidNonFinalFragmentSize {
                actual: 36,
                expected: 37
            })
        ));
        assert!(matches!(
            receive_fragments(3, &[(1, 37), (0, 38)]),
            Err(ChannelReceiveError::InvalidNonFinalFragmentSize {
                actual: 38,
                expected: 37
            })
        ));
        assert!(matches!(
            receive_fragments(3, &[(0, 37), (2, 38)]),
            Err(ChannelReceiveError::InvalidFinalFragmentSize {
                actual: 38,
                max: 37
            })
        ));
        // The final fragment arrives before the size is known, so the first non-final fragment
        // rejects it.
        assert!(matches!(
            receive_fragments(3, &[(2, 38), (0, 37)]),
            Err(ChannelReceiveError::InvalidFinalFragmentSize {
                actual: 38,
                max: 37
            })
        ));
        assert!(matches!(
            receive_fragments(3, &[(0, 0)]),
            Err(ChannelReceiveError::EmptyNonFinalFragment)
        ));

        // A final fragment equal to the fragment size, and a message of one fragment with no
        // non-final fragment to set the size: 37 + 37 + 37 = 111 bytes, and 5 bytes.
        let complete = |result: Result<Option<(Tick, Bytes)>>| result.unwrap().unwrap().1.len();
        assert_eq!(
            complete(receive_fragments(3, &[(2, 37), (1, 37), (0, 37)])),
            111
        );
        assert_eq!(complete(receive_fragments(1, &[(0, 5)])), 5);
    }

    #[cfg(feature = "compression_lz4")]
    #[test]
    fn compressed_fragments_are_decompressed_after_reassembly() -> Result<()> {
        let compression = CompressionConfig {
            min_payload_size: 0,
            max_decompressed_payload_size: FRAGMENT_SIZE * 4,
            ..CompressionConfig::LZ4
        };
        let mut receiver = FragmentReceiver::new();
        let message_bytes = Bytes::from(vec![7u8; FRAGMENT_SIZE * 3]);
        let fragments = FragmentSender::new().build_fragments_for_message(
            MessageId(1),
            message_bytes.clone(),
            compression,
            &mut CompressionScratch::default(),
        );

        assert!(fragments.len() < 3);
        assert_eq!(fragments[0].compression, Some(FragmentCompression::Lz4));
        assert!(
            fragments
                .iter()
                .skip(1)
                .all(|fragment| fragment.compression.is_none())
        );

        let mut result = None;
        for (index, fragment) in fragments.into_iter().enumerate() {
            result = receiver.receive_fragment(fragment, Tick(index as u32), None, compression)?;
        }

        assert_eq!(result, Some((Tick(0), message_bytes)));
        Ok(())
    }
}
