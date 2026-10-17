//! Build the per-hop onion payloads for an LDK [`Path`].
//!
//! Core Lightning builds the onion for us (`createonion`) as long as we
//! provide the length-prefixed TLV payload for every hop, so this module
//! mirrors LDK's `build_onion_payloads` and serializes the payloads
//! following BOLT4.
use anyhow::Result;

use lampo_common::ldk::blinded_path::BlindedPath;
use lampo_common::ldk::ln::features::{ChannelFeatures, NodeFeatures};
use lampo_common::ldk::offers::invoice::BlindedPayInfo;
use lampo_common::ldk::routing::router::{BlindedTail, Path, RouteHop};
use lampo_common::ldk::util::ser::{BigSize, Writeable};
use lampo_common::secp256k1::PublicKey;

/// BOLT1 truncated integer (`tu64`/`tu32`): big endian with the leading
/// zero bytes dropped, so zero is encoded as an empty value.
fn truncated_be(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    bytes[first..].to_vec()
}

/// BOLT4 payload TLV types.
const TLV_AMT_TO_FORWARD: u64 = 2;
const TLV_OUTGOING_CLTV_VALUE: u64 = 4;
const TLV_SHORT_CHANNEL_ID: u64 = 6;
const TLV_PAYMENT_DATA: u64 = 8;
const TLV_ENCRYPTED_RECIPIENT_DATA: u64 = 10;
const TLV_CURRENT_PATH_KEY: u64 = 12;
const TLV_PAYMENT_METADATA: u64 = 16;
const TLV_TOTAL_AMOUNT_MSAT: u64 = 18;

/// Information that only the final recipient should see.
#[derive(Debug, Clone, Default)]
pub struct RecipientFields {
    /// BOLT11 `payment_secret`.
    pub payment_secret: Option<[u8; 32]>,
    /// BOLT11 `payment_metadata`.
    pub payment_metadata: Option<Vec<u8>>,
}

/// One hop as expected by Core Lightning `createonion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnionHop {
    pub pubkey: PublicKey,
    /// Length prefixed TLV payload.
    pub payload: Vec<u8>,
}

/// The hops of the onion plus what the first HTLC must carry.
#[derive(Debug, Clone)]
pub struct OnionHops {
    pub hops: Vec<OnionHop>,
    /// Amount of the HTLC we send to the first hop.
    pub first_hop_amount_msat: u64,
    /// Absolute CLTV expiry of the HTLC we send to the first hop.
    pub first_hop_cltv_expiry: u32,
}

struct TlvStream(Vec<u8>);

impl TlvStream {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn push(&mut self, ty: u64, value: &[u8]) {
        self.0.extend(BigSize(ty).encode());
        self.0.extend(BigSize(value.len() as u64).encode());
        self.0.extend_from_slice(value);
    }

    fn push_tu64(&mut self, ty: u64, value: u64) {
        self.push(ty, &truncated_be(value));
    }

    fn push_tu32(&mut self, ty: u64, value: u32) {
        self.push(ty, &truncated_be(value as u64));
    }

    /// Finish the stream, prefixing it with its length as required by
    /// `createonion`.
    fn finish(self) -> Vec<u8> {
        let mut out = BigSize(self.0.len() as u64).encode();
        out.extend(self.0);
        out
    }
}

/// Build the onion payloads for `path`, following the same algorithm LDK
/// uses internally (`onion_utils::build_onion_payloads`).
///
/// `total_msat` is the full amount the recipient expects (the invoice
/// amount), `starting_cltv` is the block height the CLTV values are
/// relative to: Core Lightning uses `current_height + 1`.
pub fn build_onion_hops(
    path: &Path,
    total_msat: u64,
    recipient: &RecipientFields,
    starting_cltv: u32,
) -> Result<OnionHops> {
    if path.hops.is_empty() {
        anyhow::bail!("Cannot build an onion for an empty path");
    }

    let mut cur_value_msat = 0u64;
    let mut cur_cltv = starting_cltv;
    let mut last_short_channel_id = 0u64;
    let mut hops: Vec<OnionHop> = Vec::with_capacity(
        path.hops.len() + path.blinded_tail.as_ref().map_or(0, |t| t.hops.len()),
    );

    for (idx, hop) in path.hops.iter().rev().enumerate() {
        let value_msat = if cur_value_msat == 0 {
            hop.fee_msat
        } else {
            cur_value_msat
        };
        let cltv = if cur_cltv == starting_cltv {
            hop.cltv_expiry_delta + starting_cltv
        } else {
            cur_cltv
        };

        if idx == 0 {
            if let Some(tail) = &path.blinded_tail {
                let mut blinding_point = Some(tail.blinding_point);
                for (i, blinded_hop) in tail.hops.iter().enumerate() {
                    let mut tlv = TlvStream::new();
                    if i == tail.hops.len() - 1 {
                        cur_value_msat += tail.final_value_msat;
                        tlv.push_tu64(TLV_AMT_TO_FORWARD, tail.final_value_msat);
                        tlv.push_tu32(
                            TLV_OUTGOING_CLTV_VALUE,
                            cur_cltv + tail.excess_final_cltv_expiry_delta,
                        );
                        tlv.push(TLV_ENCRYPTED_RECIPIENT_DATA, &blinded_hop.encrypted_payload);
                        if let Some(point) = blinding_point.take() {
                            tlv.push(TLV_CURRENT_PATH_KEY, &point.serialize());
                        }
                        tlv.push_tu64(TLV_TOTAL_AMOUNT_MSAT, total_msat);
                    } else {
                        tlv.push(TLV_ENCRYPTED_RECIPIENT_DATA, &blinded_hop.encrypted_payload);
                        if let Some(point) = blinding_point.take() {
                            tlv.push(TLV_CURRENT_PATH_KEY, &point.serialize());
                        }
                    }
                    // The introduction node is the last unblinded hop: its
                    // onion layer is encrypted to its real key, the
                    // following hops to their blinded ids.
                    let pubkey = if i == 0 {
                        hop.pubkey
                    } else {
                        blinded_hop.blinded_node_id
                    };
                    hops.push(OnionHop {
                        pubkey,
                        payload: tlv.finish(),
                    });
                }
            } else {
                let mut tlv = TlvStream::new();
                tlv.push_tu64(TLV_AMT_TO_FORWARD, value_msat);
                tlv.push_tu32(TLV_OUTGOING_CLTV_VALUE, cltv);
                if let Some(secret) = recipient.payment_secret {
                    let mut data = secret.to_vec();
                    data.extend(truncated_be(total_msat));
                    tlv.push(TLV_PAYMENT_DATA, &data);
                }
                if let Some(metadata) = &recipient.payment_metadata {
                    tlv.push(TLV_PAYMENT_METADATA, metadata);
                }
                hops.push(OnionHop {
                    pubkey: hop.pubkey,
                    payload: tlv.finish(),
                });
            }
        } else {
            let mut tlv = TlvStream::new();
            tlv.push_tu64(TLV_AMT_TO_FORWARD, value_msat);
            tlv.push_tu32(TLV_OUTGOING_CLTV_VALUE, cltv);
            tlv.push(TLV_SHORT_CHANNEL_ID, &last_short_channel_id.to_be_bytes());
            hops.insert(
                0,
                OnionHop {
                    pubkey: hop.pubkey,
                    payload: tlv.finish(),
                },
            );
        }

        cur_value_msat += hop.fee_msat;
        if cur_value_msat >= 21_000_000 * 100_000_000 * 1000 {
            anyhow::bail!("Channel fees overflowed");
        }
        cur_cltv += hop.cltv_expiry_delta;
        if cur_cltv >= 500_000_000 {
            anyhow::bail!("Channel CLTV overflowed");
        }
        last_short_channel_id = hop.short_channel_id;
    }

    Ok(OnionHops {
        hops,
        first_hop_amount_msat: cur_value_msat,
        first_hop_cltv_expiry: cur_cltv,
    })
}

/// The [`Path`] to pay through a blinded path whose introduction node is
/// our own node, which LDK refuses to route ("we are the introduction
/// node"). It has one clear hop, our node, carrying the blinded path's
/// aggregated fee and CLTV delta like the last clear hop of any LDK path
/// to a blinded tail. [`build_onion_hops`] then encrypts the first layer to
/// our real key, and Core Lightning's `injectpaymentonion` unwraps it as
/// if a peer had sent it.
pub fn path_from_introduction(
    our_id: PublicKey,
    payinfo: &BlindedPayInfo,
    path: &BlindedPath,
    amount_msat: u64,
) -> Path {
    let proportional = (amount_msat as u128 * payinfo.fee_proportional_millionths as u128)
        .div_ceil(1_000_000) as u64;
    Path {
        hops: vec![RouteHop {
            pubkey: our_id,
            node_features: NodeFeatures::empty(),
            short_channel_id: 0,
            channel_features: ChannelFeatures::empty(),
            fee_msat: payinfo.fee_base_msat as u64 + proportional,
            cltv_expiry_delta: payinfo.cltv_expiry_delta as u32,
            maybe_announced_channel: false,
        }],
        blinded_tail: Some(BlindedTail {
            hops: path.blinded_hops.clone(),
            blinding_point: path.blinding_point,
            excess_final_cltv_expiry_delta: 0,
            final_value_msat: amount_msat,
        }),
    }
}

/// Where a payment failed, reconstructed from Core Lightning's
/// `erring_index`.
///
/// `sendonion` never sees the route, so Core Lightning can only report the
/// position of the failing onion layer: `0` is a local failure on our
/// first channel, `N` is the node that peeled layer `N - 1`. barq built the
/// onion, so it can map that position back onto the LDK [`Path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailurePoint {
    /// The node that returned the error. `None` for a local failure or for
    /// a hop hidden inside a blinded path.
    pub node: Option<PublicKey>,
    /// The channel the failing node could not use. For a failure at the
    /// final node of a clear path this is the channel into the recipient,
    /// like Core Lightning reports it.
    pub channel: Option<u64>,
    /// The error comes from the recipient side: the final node, or any hop
    /// of the blinded path (whose errors are masked by design).
    pub at_recipient: bool,
    /// The error comes from the introduction node or beyond.
    pub in_blinded_path: bool,
}

impl FailurePoint {
    /// The channel to penalise in the scorer, if the failure says
    /// something about the route rather than the recipient.
    pub fn penalised_channel(&self) -> Option<u64> {
        if self.at_recipient || self.node.is_none() {
            return None;
        }
        self.channel
    }
}

/// Map Core Lightning's `erring_index` onto `path`.
pub fn locate_failure(path: &Path, erring_index: u32) -> Option<FailurePoint> {
    let hops = &path.hops;
    if hops.is_empty() {
        return None;
    }
    if erring_index == 0 {
        return Some(FailurePoint {
            node: None,
            channel: Some(hops[0].short_channel_id),
            at_recipient: false,
            in_blinded_path: false,
        });
    }
    let position = (erring_index - 1) as usize;
    match &path.blinded_tail {
        None => {
            let hop = hops.get(position)?;
            let last = position == hops.len() - 1;
            Some(FailurePoint {
                node: Some(hop.pubkey),
                channel: Some(if last {
                    hop.short_channel_id
                } else {
                    hops[position + 1].short_channel_id
                }),
                at_recipient: last,
                in_blinded_path: false,
            })
        }
        Some(tail) => {
            // The last clear hop is the introduction node, whose onion
            // layer carries the first blinded payload.
            let intro = hops.len() - 1;
            if position >= intro + tail.hops.len() {
                return None;
            }
            if position < intro {
                return Some(FailurePoint {
                    node: Some(hops[position].pubkey),
                    channel: Some(hops[position + 1].short_channel_id),
                    at_recipient: false,
                    in_blinded_path: false,
                });
            }
            Some(FailurePoint {
                node: (position == intro).then(|| hops[intro].pubkey),
                channel: None,
                at_recipient: true,
                in_blinded_path: true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use lampo_common::ldk::blinded_path::BlindedHop;
    use lampo_common::ldk::ln::features::{ChannelFeatures, NodeFeatures};
    use lampo_common::ldk::routing::router::{BlindedTail, RouteHop};

    use super::*;

    fn pk(i: u8) -> PublicKey {
        let hex = match i {
            1 => "02eec7245d6b7d2ccb30380bfbe2a3648cd7a942653f5aa340edcea1f283686619",
            2 => "0324653eac434488002cc06bbfb7f10fe18991e35f9fe4302dbea6d2353dc0ab1c",
            _ => "027f31ebc5462c1fdce1b737ecff52d37d75dea43ce11c74d25aa297165faa2007",
        };
        PublicKey::from_str(hex).unwrap()
    }

    fn hop(i: u8, scid: u64, fee_msat: u64, cltv: u32) -> RouteHop {
        RouteHop {
            pubkey: pk(i),
            node_features: NodeFeatures::empty(),
            short_channel_id: scid,
            channel_features: ChannelFeatures::empty(),
            fee_msat,
            cltv_expiry_delta: cltv,
            maybe_announced_channel: true,
        }
    }

    #[test]
    fn truncated_integers() {
        assert_eq!(truncated_be(0), Vec::<u8>::new());
        assert_eq!(truncated_be(1), vec![1]);
        assert_eq!(truncated_be(0x0100), vec![1, 0]);
        assert_eq!(truncated_be(u64::MAX), vec![0xff; 8]);
    }

    #[test]
    fn single_hop_bolt11() {
        let path = Path {
            hops: vec![hop(1, 0x1234, 1000, 18)],
            blinded_tail: None,
        };
        let secret = [0x11u8; 32];
        let onion = build_onion_hops(
            &path,
            1000,
            &RecipientFields {
                payment_secret: Some(secret),
                payment_metadata: None,
            },
            100,
        )
        .unwrap();
        assert_eq!(onion.hops.len(), 1);
        assert_eq!(onion.first_hop_amount_msat, 1000);
        assert_eq!(onion.first_hop_cltv_expiry, 118);

        // amt_to_forward: type 2, len 2, 0x03e8
        // outgoing_cltv: type 4, len 1, 0x76
        // payment_data: type 8, len 34, secret || tu64(1000)
        let mut expected = vec![2u8, 2, 0x03, 0xe8, 4, 1, 0x76, 8, 34];
        expected.extend_from_slice(&secret);
        expected.extend_from_slice(&[0x03, 0xe8]);
        let mut with_len = vec![expected.len() as u8];
        with_len.extend(expected);
        assert_eq!(onion.hops[0].payload, with_len);
        assert_eq!(onion.hops[0].pubkey, pk(1));
    }

    #[test]
    fn two_hops_accumulate_fees_and_cltv() {
        // us -> 1 -> 2 (payee). Hop 1 charges 10 msat and 40 blocks.
        let path = Path {
            hops: vec![hop(1, 0xaa, 10, 40), hop(2, 0xbb, 5000, 9)],
            blinded_tail: None,
        };
        let onion = build_onion_hops(&path, 5000, &RecipientFields::default(), 100).unwrap();
        assert_eq!(onion.hops.len(), 2);
        assert_eq!(onion.first_hop_amount_msat, 5010);
        assert_eq!(onion.first_hop_cltv_expiry, 149);

        // Forward payload for hop 1: amt 5000 (0x1388), cltv 109 (0x6d),
        // scid 0xbb
        let expected = vec![
            2, 2, 0x13, 0x88, 4, 1, 0x6d, 6, 8, 0, 0, 0, 0, 0, 0, 0, 0xbb,
        ];
        let mut with_len = vec![expected.len() as u8];
        with_len.extend(expected);
        assert_eq!(onion.hops[0].payload, with_len);
        assert_eq!(onion.hops[0].pubkey, pk(1));
        assert_eq!(onion.hops[1].pubkey, pk(2));
    }

    #[test]
    fn blinded_tail_payloads() {
        // us -> 1 (introduction node) -> blinded hop (recipient)
        let path = Path {
            hops: vec![hop(1, 0xaa, 20, 50)],
            blinded_tail: Some(BlindedTail {
                hops: vec![
                    BlindedHop {
                        blinded_node_id: pk(2),
                        encrypted_payload: vec![0xde, 0xad],
                    },
                    BlindedHop {
                        blinded_node_id: pk(3),
                        encrypted_payload: vec![0xbe, 0xef],
                    },
                ],
                blinding_point: pk(3),
                excess_final_cltv_expiry_delta: 0,
                final_value_msat: 1000,
            }),
        };
        let onion = build_onion_hops(&path, 1000, &RecipientFields::default(), 100).unwrap();
        assert_eq!(onion.hops.len(), 2);
        assert_eq!(onion.first_hop_amount_msat, 1020);
        assert_eq!(onion.first_hop_cltv_expiry, 150);

        // Introduction node: encrypted data + path key only.
        let mut expected = vec![10u8, 2, 0xde, 0xad, 12, 33];
        expected.extend_from_slice(&pk(3).serialize());
        let mut with_len = vec![expected.len() as u8];
        with_len.extend(expected);
        assert_eq!(onion.hops[0].payload, with_len);
        // Encrypted to the introduction node's real key, not its blinded id.
        assert_eq!(onion.hops[0].pubkey, pk(1));

        // Final blinded hop: amount, cltv, encrypted data, total.
        let expected = vec![
            2, 2, 0x03, 0xe8, 4, 1, 0x64, 10, 2, 0xbe, 0xef, 18, 2, 0x03, 0xe8,
        ];
        let mut with_len = vec![expected.len() as u8];
        with_len.extend(expected);
        assert_eq!(onion.hops[1].payload, with_len);
        assert_eq!(onion.hops[1].pubkey, pk(3));
    }

    fn three_hop_path() -> Path {
        Path {
            hops: vec![
                hop(1, 0xa1, 10, 40),
                hop(2, 0xb2, 10, 40),
                hop(3, 0xc3, 1000, 18),
            ],
            blinded_tail: None,
        }
    }

    #[test]
    fn locate_failure_clear_path() {
        let path = three_hop_path();
        // Local failure on our first channel.
        let local = locate_failure(&path, 0).unwrap();
        assert_eq!(local.node, None);
        assert_eq!(local.channel, Some(0xa1));
        assert_eq!(local.penalised_channel(), None);

        // The first remote node could not forward on its outgoing channel.
        let first = locate_failure(&path, 1).unwrap();
        assert_eq!(first.node, Some(pk(1)));
        assert_eq!(first.channel, Some(0xb2));
        assert!(!first.at_recipient);
        assert_eq!(first.penalised_channel(), Some(0xb2));

        let second = locate_failure(&path, 2).unwrap();
        assert_eq!(second.node, Some(pk(2)));
        assert_eq!(second.channel, Some(0xc3));

        // The recipient rejected the payment: nothing to learn about the route.
        let recipient = locate_failure(&path, 3).unwrap();
        assert_eq!(recipient.node, Some(pk(3)));
        assert!(recipient.at_recipient);
        assert_eq!(recipient.penalised_channel(), None);

        assert_eq!(locate_failure(&path, 4), None);
    }

    #[test]
    fn locate_failure_blinded_path() {
        // us -> 1 -> 2 (introduction) -> blinded hop -> blinded recipient
        let path = Path {
            hops: vec![hop(1, 0xa1, 10, 40), hop(2, 0xb2, 10, 40)],
            blinded_tail: Some(BlindedTail {
                hops: vec![
                    BlindedHop {
                        blinded_node_id: pk(3),
                        encrypted_payload: vec![1],
                    },
                    BlindedHop {
                        blinded_node_id: pk(3),
                        encrypted_payload: vec![2],
                    },
                ],
                blinding_point: pk(3),
                excess_final_cltv_expiry_delta: 0,
                final_value_msat: 1000,
            }),
        };
        let first = locate_failure(&path, 1).unwrap();
        assert_eq!(first.node, Some(pk(1)));
        assert_eq!(first.channel, Some(0xb2));
        assert_eq!(first.penalised_channel(), Some(0xb2));

        let intro = locate_failure(&path, 2).unwrap();
        assert_eq!(intro.node, Some(pk(2)));
        assert!(intro.in_blinded_path && intro.at_recipient);
        assert_eq!(intro.penalised_channel(), None);

        let hidden = locate_failure(&path, 3).unwrap();
        assert_eq!(hidden.node, None);
        assert!(hidden.in_blinded_path);

        assert_eq!(locate_failure(&path, 4), None);
    }

    #[test]
    fn path_from_our_introduction_node() {
        use lampo_common::ldk::blinded_path::IntroductionNode;
        use lampo_common::ldk::ln::features::BlindedHopFeatures;

        let payinfo = BlindedPayInfo {
            fee_base_msat: 1000,
            fee_proportional_millionths: 100,
            htlc_minimum_msat: 1,
            htlc_maximum_msat: u64::MAX,
            cltv_expiry_delta: 40,
            features: BlindedHopFeatures::empty(),
        };
        let blinded = BlindedPath {
            introduction_node: IntroductionNode::NodeId(pk(1)),
            blinding_point: pk(3),
            blinded_hops: vec![
                BlindedHop {
                    blinded_node_id: pk(2),
                    encrypted_payload: vec![0xde, 0xad],
                },
                BlindedHop {
                    blinded_node_id: pk(3),
                    encrypted_payload: vec![0xbe, 0xef],
                },
            ],
        };
        // 1000 base + ceil(15_000 * 100 / 10^6) = 1000 + 2.
        let path = path_from_introduction(pk(1), &payinfo, &blinded, 15_000);
        assert_eq!(path.hops.len(), 1);
        assert_eq!(path.hops[0].fee_msat, 1002);

        let onion = build_onion_hops(&path, 15_000, &RecipientFields::default(), 100).unwrap();
        // What our node "receives": the amount plus the path's fee, with a
        // CLTV covering the path.
        assert_eq!(onion.first_hop_amount_msat, 16_002);
        assert_eq!(onion.first_hop_cltv_expiry, 140);
        // We are the first layer, under our real key; then the blinded hops.
        assert_eq!(onion.hops.len(), 2);
        assert_eq!(onion.hops[0].pubkey, pk(1));
        assert_eq!(onion.hops[1].pubkey, pk(3));
    }
}
