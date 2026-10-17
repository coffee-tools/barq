//! Decode the BOLT4 failure message Core Lightning hands back in the
//! `raw_message` field of a failed `waitsendpay`.
//!
//! Forwarding errors with the `UPDATE` flag carry the failing node's
//! latest `channel_update`. The rapid gossip sync snapshots barq routes
//! on lag behind the network (the server batches updates and LDK backdates
//! them by a week), so that update is the fastest way to learn the policy
//! (fees, CLTV delta, HTLC limits, disabled flag) the node really
//! enforces. This module extracts it and says what the failure means for
//! the scorer; the plugin applies it to the graph and hands it to
//! `addgossip`.
use anyhow::Result;

use lampo_common::ldk::ln::msgs::{ChannelUpdate, UnsignedChannelUpdate};
use lampo_common::ldk::util::ser::Readable;

/// BOLT4 failure code flags.
pub const BADONION: u16 = 0x8000;
pub const PERM: u16 = 0x4000;
pub const NODE: u16 = 0x2000;
pub const UPDATE: u16 = 0x1000;

/// The failure codes that carry a `channel_update`.
pub const TEMPORARY_CHANNEL_FAILURE: u16 = UPDATE | 7;
pub const AMOUNT_BELOW_MINIMUM: u16 = UPDATE | 11;
pub const FEE_INSUFFICIENT: u16 = UPDATE | 12;
pub const INCORRECT_CLTV_EXPIRY: u16 = UPDATE | 13;
pub const EXPIRY_TOO_SOON: u16 = UPDATE | 14;
pub const CHANNEL_DISABLED: u16 = UPDATE | 20;

/// The failure codes after which the channel is of no use.
pub const PERMANENT_CHANNEL_FAILURE: u16 = PERM | 8;
pub const REQUIRED_CHANNEL_FEATURE_MISSING: u16 = PERM | 9;
pub const UNKNOWN_NEXT_PEER: u16 = PERM | 10;

/// The recipient does not know the payment hash, or the amount or CLTV is
/// wrong for it.
pub const INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS: u16 = PERM | 15;

/// BOLT1 message type of `channel_update`.
const WIRE_CHANNEL_UPDATE: u16 = 258;

/// `channel_flags` bit marking the direction as disabled.
const FLAG_DISABLED: u8 = 1 << 1;

/// A `channel_update` carried by a failure message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedUpdate {
    /// The parsed message, signature included.
    pub update: ChannelUpdate,
    /// The BOLT7 message with its type prefix, as `addgossip` expects it.
    pub raw: Vec<u8>,
}

impl CarriedUpdate {
    pub fn contents(&self) -> &UnsignedChannelUpdate {
        &self.update.contents
    }

    /// Whether the update disables the channel in the failing direction.
    pub fn disables_channel(&self) -> bool {
        self.update.contents.flags & FLAG_DISABLED != 0
    }
}

/// A decoded failure message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionFailure {
    pub failcode: u16,
    /// Present for `UPDATE` failures that embed a non-empty update.
    pub channel_update: Option<CarriedUpdate>,
}

/// Decode a failure message: the 2-byte failure code followed by the
/// failure specific fields.
///
/// Only the `channel_update` is extracted; the other fields (the HTLC
/// amount or CLTV the node complains about) are skipped. A failure that
/// announces an update but carries garbage is an error: the failure code
/// is still known from Core Lightning's `failcode` field.
pub fn parse_failure_message(raw: &[u8]) -> Result<OnionFailure> {
    if raw.len() < 2 {
        anyhow::bail!("Failure message too short: {} bytes", raw.len());
    }
    let failcode = u16::from_be_bytes([raw[0], raw[1]]);
    let body = &raw[2..];
    // Bytes between the failure code and the length-prefixed update.
    let skip = match failcode {
        TEMPORARY_CHANNEL_FAILURE | EXPIRY_TOO_SOON => 0,
        AMOUNT_BELOW_MINIMUM | FEE_INSUFFICIENT => 8,
        INCORRECT_CLTV_EXPIRY => 4,
        CHANNEL_DISABLED => 2,
        _ => {
            return Ok(OnionFailure {
                failcode,
                channel_update: None,
            })
        }
    };
    let field = body
        .get(skip..)
        .ok_or_else(|| anyhow::anyhow!("Failure {failcode:#06x} is truncated"))?;
    Ok(OnionFailure {
        failcode,
        channel_update: channel_update_field(field)?,
    })
}

/// The block height the recipient reported in an
/// `incorrect_or_unknown_payment_details` failure (`u64 htlc_msat`,
/// `u32 height`). A height above the one the payment used means our chain
/// view is behind, not that the invoice is unknown: the payment is worth
/// retrying with CLTVs computed from that height.
pub fn reported_height(raw: &[u8]) -> Option<u32> {
    if raw.len() < 2 + 8 + 4
        || u16::from_be_bytes([raw[0], raw[1]]) != INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS
    {
        return None;
    }
    Some(u32::from_be_bytes(raw[10..14].try_into().ok()?))
}

/// Parse `u16 len | len bytes of channel_update`.
///
/// The BOLTs were unclear for a long time on whether the type prefix is
/// part of the embedded message, so both forms are still seen on the
/// network and both are accepted here, like LDK and Core Lightning do.
fn channel_update_field(bytes: &[u8]) -> Result<Option<CarriedUpdate>> {
    if bytes.len() < 2 {
        anyhow::bail!("Missing channel_update length");
    }
    let len = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let update = bytes.get(2..2 + len).ok_or_else(|| {
        anyhow::anyhow!(
            "channel_update truncated: {len} bytes announced, {} present",
            bytes.len() - 2
        )
    })?;
    if update.is_empty() {
        return Ok(None);
    }
    let has_type = update.len() > 2 && update[..2] == WIRE_CHANNEL_UPDATE.to_be_bytes();
    let candidates: &[&[u8]] = if has_type {
        // A prefix-less update whose signature happens to start with the
        // type bytes still parses on the second try.
        &[&update[2..], update]
    } else {
        &[update]
    };
    let mut last_err = None;
    for candidate in candidates {
        let mut reader = *candidate;
        match ChannelUpdate::read(&mut reader) {
            Ok(parsed) => {
                let mut raw = WIRE_CHANNEL_UPDATE.to_be_bytes().to_vec();
                raw.extend_from_slice(candidate);
                return Ok(Some(CarriedUpdate {
                    update: parsed,
                    raw,
                }));
            }
            Err(err) => last_err = Some(err),
        }
    }
    anyhow::bail!("Malformed channel_update: {:?}", last_err.unwrap())
}

/// What a failure says about the channel it was reported on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The channel could not carry the amount right now. This is the
    /// signal the probabilistic scorer learns from.
    Liquidity,
    /// The node rejected the HTLC because our view of its policy (fees,
    /// CLTV delta, HTLC limits, enabled flag) is stale. The update carried
    /// by the failure is the fix; the liquidity estimate is unaffected.
    Policy,
    /// The channel cannot be used at all: the next peer is unknown, the
    /// channel failed permanently or requires features we lack.
    ChannelUnusable,
    /// The node itself (or the onion we built) is the problem; nothing to
    /// learn about the channel.
    Node,
    /// The recipient rejected the payment, or the code says nothing about
    /// the route.
    Other,
    /// Core Lightning could not tell us the failure code: blame the
    /// channel as if it lacked liquidity.
    Unknown,
}

impl FailureKind {
    /// Whether the scorer should lower its liquidity estimate for the
    /// failing channel.
    pub fn penalise_liquidity(self) -> bool {
        matches!(
            self,
            FailureKind::Liquidity | FailureKind::ChannelUnusable | FailureKind::Unknown
        )
    }
}

/// Classify a failure code. `update` is the `channel_update` the failure
/// carried, if any: a `temporary_channel_failure` whose update disables
/// the channel is a policy change, not a liquidity signal.
pub fn classify_failure(failcode: Option<u16>, update: Option<&CarriedUpdate>) -> FailureKind {
    let Some(code) = failcode else {
        return FailureKind::Unknown;
    };
    if code & (BADONION | NODE) != 0 {
        return FailureKind::Node;
    }
    if code & UPDATE != 0 {
        let disabled = update.is_some_and(CarriedUpdate::disables_channel);
        return if code == TEMPORARY_CHANNEL_FAILURE && !disabled {
            FailureKind::Liquidity
        } else {
            FailureKind::Policy
        };
    }
    match code {
        PERMANENT_CHANNEL_FAILURE | REQUIRED_CHANNEL_FEATURE_MISSING | UNKNOWN_NEXT_PEER => {
            FailureKind::ChannelUnusable
        }
        _ => FailureKind::Other,
    }
}

#[cfg(test)]
mod height_tests {
    use super::*;

    #[test]
    fn height_from_unknown_payment_details() {
        let mut raw = INCORRECT_OR_UNKNOWN_PAYMENT_DETAILS.to_be_bytes().to_vec();
        raw.extend(42_000u64.to_be_bytes());
        raw.extend(812u32.to_be_bytes());
        assert_eq!(reported_height(&raw), Some(812));
        // Older nodes send the code alone.
        assert_eq!(reported_height(&raw[..2]), None);
        let mut other = FEE_INSUFFICIENT.to_be_bytes().to_vec();
        other.extend(&raw[2..]);
        assert_eq!(reported_height(&other), None);
    }
}

#[cfg(test)]
mod tests {
    use lampo_common::bitcoin::blockdata::constants::ChainHash;
    use lampo_common::bitcoin::hashes::{sha256d, Hash};
    use lampo_common::bitcoin::Network;
    use lampo_common::ldk::util::ser::Writeable;
    use lampo_common::secp256k1::ecdsa::Signature;
    use lampo_common::secp256k1::{Message, Secp256k1, SecretKey};

    use super::*;

    const SCID: u64 = (103u64 << 40) | (1u64 << 16);

    fn contents(flags: u8) -> UnsignedChannelUpdate {
        UnsignedChannelUpdate {
            chain_hash: ChainHash::using_genesis_block(Network::Regtest),
            short_channel_id: SCID,
            timestamp: 1_700_000_000,
            flags,
            cltv_expiry_delta: 34,
            htlc_minimum_msat: 1_000,
            htlc_maximum_msat: 990_000_000,
            fee_base_msat: 6_000,
            fee_proportional_millionths: 2_010,
            excess_data: Vec::new(),
        }
    }

    /// A properly signed update, serialized without its type prefix.
    fn signed_update(flags: u8) -> (ChannelUpdate, Vec<u8>) {
        let contents = contents(flags);
        let secp = Secp256k1::new();
        let key = SecretKey::from_slice(&[0x42; 32]).unwrap();
        let digest = sha256d::Hash::hash(&contents.encode()).to_byte_array();
        let signature = secp.sign_ecdsa(&Message::from_slice(&digest).unwrap(), &key);
        let update = ChannelUpdate {
            signature,
            contents,
        };
        let bytes = update.encode();
        assert_eq!(bytes.len(), 136);
        (update, bytes)
    }

    fn with_type(update: &[u8]) -> Vec<u8> {
        let mut out = vec![0x01, 0x02];
        out.extend_from_slice(update);
        out
    }

    /// `failcode | fields | len | update`.
    fn failure(failcode: u16, fields: &[u8], update: &[u8]) -> Vec<u8> {
        let mut out = failcode.to_be_bytes().to_vec();
        out.extend_from_slice(fields);
        out.extend_from_slice(&(update.len() as u16).to_be_bytes());
        out.extend_from_slice(update);
        out
    }

    fn assert_carries(raw: &[u8], failcode: u16, expected: &ChannelUpdate, prefixed: &[u8]) {
        let parsed = parse_failure_message(raw).unwrap();
        assert_eq!(parsed.failcode, failcode);
        let carried = parsed.channel_update.expect("an update");
        assert_eq!(&carried.update, expected);
        assert_eq!(carried.raw, prefixed);
        assert_eq!(carried.contents().fee_base_msat, 6_000);
    }

    #[test]
    fn layouts_with_type_prefix() {
        let (update, bytes) = signed_update(0);
        let prefixed = with_type(&bytes);
        let htlc_msat = 123_456u64.to_be_bytes();
        let cases: Vec<(u16, &[u8])> = vec![
            (TEMPORARY_CHANNEL_FAILURE, &[]),
            (AMOUNT_BELOW_MINIMUM, &htlc_msat),
            (FEE_INSUFFICIENT, &htlc_msat),
            (INCORRECT_CLTV_EXPIRY, &[0, 0, 0x02, 0x01]),
            (EXPIRY_TOO_SOON, &[]),
            (CHANNEL_DISABLED, &[0, 0]),
        ];
        for (code, fields) in cases {
            let raw = failure(code, fields, &prefixed);
            assert_carries(&raw, code, &update, &prefixed);
        }
    }

    #[test]
    fn layouts_without_type_prefix() {
        let (update, bytes) = signed_update(0);
        let prefixed = with_type(&bytes);
        let htlc_msat = 1u64.to_be_bytes();
        let cases: Vec<(u16, &[u8])> = vec![
            (TEMPORARY_CHANNEL_FAILURE, &[]),
            (AMOUNT_BELOW_MINIMUM, &htlc_msat),
            (FEE_INSUFFICIENT, &htlc_msat),
            (INCORRECT_CLTV_EXPIRY, &[0, 0, 0, 9]),
            (EXPIRY_TOO_SOON, &[]),
            (CHANNEL_DISABLED, &[0, 2]),
        ];
        for (code, fields) in cases {
            let raw = failure(code, fields, &bytes);
            assert_carries(&raw, code, &update, &prefixed);
        }
    }

    #[test]
    fn fee_insufficient_fields_are_skipped() {
        // Wrong field size: the length prefix lands in the middle of the
        // HTLC amount and announces more bytes than present.
        let (_, bytes) = signed_update(0);
        let raw = failure(FEE_INSUFFICIENT, &[0, 0, 0, 0], &bytes);
        assert!(parse_failure_message(&raw).is_err());
    }

    #[test]
    fn prefix_less_update_starting_with_type_bytes() {
        // A signature whose first two bytes happen to be 0x0102.
        let mut compact = [0x11u8; 64];
        compact[0] = 0x01;
        compact[1] = 0x02;
        let update = ChannelUpdate {
            signature: Signature::from_compact(&compact).unwrap(),
            contents: contents(0),
        };
        let bytes = update.encode();
        let raw = failure(TEMPORARY_CHANNEL_FAILURE, &[], &bytes);
        let parsed = parse_failure_message(&raw).unwrap();
        let carried = parsed.channel_update.unwrap();
        assert_eq!(carried.update, update);
        assert_eq!(carried.raw, with_type(&bytes));
    }

    #[test]
    fn empty_update_is_none() {
        let raw = failure(TEMPORARY_CHANNEL_FAILURE, &[], &[]);
        let parsed = parse_failure_message(&raw).unwrap();
        assert_eq!(parsed.failcode, TEMPORARY_CHANNEL_FAILURE);
        assert_eq!(parsed.channel_update, None);
    }

    #[test]
    fn codes_without_update() {
        let raw = UNKNOWN_NEXT_PEER.to_be_bytes();
        let parsed = parse_failure_message(&raw).unwrap();
        assert_eq!(parsed.failcode, UNKNOWN_NEXT_PEER);
        assert_eq!(parsed.channel_update, None);

        // incorrect_or_unknown_payment_details: htlc_msat, height, trailing data.
        let mut raw = (PERM | 15).to_be_bytes().to_vec();
        raw.extend_from_slice(&[0; 12]);
        assert_eq!(parse_failure_message(&raw).unwrap().channel_update, None);
    }

    #[test]
    fn malformed_messages() {
        assert!(parse_failure_message(&[]).is_err());
        assert!(parse_failure_message(&[0x10]).is_err());
        // No length.
        assert!(parse_failure_message(&TEMPORARY_CHANNEL_FAILURE.to_be_bytes()).is_err());
        // Length beyond the message.
        let mut raw = TEMPORARY_CHANNEL_FAILURE.to_be_bytes().to_vec();
        raw.extend_from_slice(&[0, 10, 1, 2]);
        assert!(parse_failure_message(&raw).is_err());
        // Garbage update.
        let raw = failure(FEE_INSUFFICIENT, &[0; 8], &[0xff; 40]);
        assert!(parse_failure_message(&raw).is_err());
        // Truncated signed update, with and without the type prefix.
        let (_, bytes) = signed_update(0);
        let raw = failure(FEE_INSUFFICIENT, &[0; 8], &bytes[..100]);
        assert!(parse_failure_message(&raw).is_err());
        let raw = failure(FEE_INSUFFICIENT, &[0; 8], &with_type(&bytes)[..100]);
        assert!(parse_failure_message(&raw).is_err());
    }

    #[test]
    fn disabled_flag() {
        let (update, bytes) = signed_update(FLAG_DISABLED | 1);
        let raw = failure(TEMPORARY_CHANNEL_FAILURE, &[], &bytes);
        let carried = parse_failure_message(&raw).unwrap().channel_update.unwrap();
        assert_eq!(carried.update, update);
        assert!(carried.disables_channel());
        assert_eq!(carried.contents().flags & 1, 1);
    }

    #[test]
    fn classification() {
        let (_, enabled) = signed_update(0);
        let enabled = parse_failure_message(&failure(TEMPORARY_CHANNEL_FAILURE, &[], &enabled))
            .unwrap()
            .channel_update;
        let (_, disabled) = signed_update(FLAG_DISABLED);
        let disabled = parse_failure_message(&failure(TEMPORARY_CHANNEL_FAILURE, &[], &disabled))
            .unwrap()
            .channel_update;

        let liquidity = [
            classify_failure(Some(TEMPORARY_CHANNEL_FAILURE), None),
            classify_failure(Some(TEMPORARY_CHANNEL_FAILURE), enabled.as_ref()),
        ];
        assert!(liquidity.iter().all(|k| *k == FailureKind::Liquidity));
        assert_eq!(
            classify_failure(Some(TEMPORARY_CHANNEL_FAILURE), disabled.as_ref()),
            FailureKind::Policy
        );
        for code in [
            FEE_INSUFFICIENT,
            AMOUNT_BELOW_MINIMUM,
            INCORRECT_CLTV_EXPIRY,
            EXPIRY_TOO_SOON,
            CHANNEL_DISABLED,
        ] {
            assert_eq!(classify_failure(Some(code), None), FailureKind::Policy);
            assert!(!classify_failure(Some(code), enabled.as_ref()).penalise_liquidity());
        }
        for code in [
            PERMANENT_CHANNEL_FAILURE,
            REQUIRED_CHANNEL_FEATURE_MISSING,
            UNKNOWN_NEXT_PEER,
        ] {
            assert_eq!(
                classify_failure(Some(code), None),
                FailureKind::ChannelUnusable
            );
            assert!(classify_failure(Some(code), None).penalise_liquidity());
        }
        // temporary_node_failure, permanent_node_failure, invalid_onion_hmac,
        // invalid_onion_blinding.
        for code in [
            NODE | 2,
            PERM | NODE | 2,
            BADONION | PERM | 5,
            BADONION | PERM | 24,
        ] {
            assert_eq!(classify_failure(Some(code), None), FailureKind::Node);
        }
        // incorrect_or_unknown_payment_details, final_incorrect_cltv_expiry,
        // mpp_timeout, invalid_realm.
        for code in [PERM | 15, 18, 23, PERM | 1] {
            assert_eq!(classify_failure(Some(code), None), FailureKind::Other);
            assert!(!classify_failure(Some(code), None).penalise_liquidity());
        }
        assert_eq!(classify_failure(None, None), FailureKind::Unknown);
        assert!(FailureKind::Unknown.penalise_liquidity());
    }
}
