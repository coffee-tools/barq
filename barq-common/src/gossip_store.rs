//! Feed the LDK network graph from Core Lightning's `gossip_store`.
//!
//! The file is read by the cln4rust [`GossipStoreReader`], which remembers
//! the offset it stopped at, so each sync only returns the records appended
//! since the previous one, and follows the store when `gossipd` rewrites it.
//! Every raw `channel_announcement`, `channel_update` and
//! `node_announcement` is fed to the LDK network graph as is; signatures
//! are not re-verified since they were already checked by `gossipd`.
use std::ops::Deref;

use anyhow::{Context, Result};

pub use clightningrpc_gossip_map::GossipStoreReader;

use lampo_common::ldk::ln::msgs::{ChannelAnnouncement, ChannelUpdate, NodeAnnouncement};
use lampo_common::ldk::routing::gossip::NetworkGraph;
use lampo_common::ldk::util::logger::Logger;
use lampo_common::ldk::util::ser::Readable;

const WIRE_CHANNEL_ANNOUNCEMENT: u16 = 256;
const WIRE_NODE_ANNOUNCEMENT: u16 = 257;
const WIRE_CHANNEL_UPDATE: u16 = 258;

/// Counters describing what a sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GossipStoreStats {
    pub channel_announcements: u64,
    pub channel_updates: u64,
    pub node_announcements: u64,
    /// Records we skipped on purpose (deleted, dying, non BOLT7 types).
    pub skipped: u64,
    /// Messages LDK refused (already known, stale, unknown channel, ...).
    pub rejected: u64,
    /// Bytes of the file consumed so far.
    pub offset: u64,
}

/// Read every record appended since the last sync and feed it to `graph`.
pub fn sync<L: Deref>(
    reader: &mut GossipStoreReader,
    graph: &NetworkGraph<L>,
) -> Result<GossipStoreStats>
where
    L::Target: Logger,
{
    let mut stats = GossipStoreStats::default();
    while let Some(record) = reader
        .next_record()
        .with_context(|| format!("reading gossip store {}", reader.path().display()))?
    {
        if record.is_deleted() || record.is_dying() {
            stats.skipped += 1;
            continue;
        }
        let mut body = &record.msg[2..];
        match record.msg_type() {
            WIRE_CHANNEL_ANNOUNCEMENT => match ChannelAnnouncement::read(&mut body) {
                Ok(ann) => match graph.update_channel_from_announcement_no_lookup(&ann) {
                    Ok(_) => stats.channel_announcements += 1,
                    Err(err) => {
                        log::trace!("gossip_store: channel_announcement rejected: {err:?}");
                        stats.rejected += 1;
                    }
                },
                Err(err) => {
                    log::debug!("gossip_store: cannot parse channel_announcement: {err:?}");
                    stats.rejected += 1;
                }
            },
            WIRE_CHANNEL_UPDATE => match ChannelUpdate::read(&mut body) {
                Ok(upd) => match graph.update_channel_unsigned(&upd.contents) {
                    Ok(_) => stats.channel_updates += 1,
                    Err(err) => {
                        log::trace!("gossip_store: channel_update rejected: {err:?}");
                        stats.rejected += 1;
                    }
                },
                Err(err) => {
                    log::debug!("gossip_store: cannot parse channel_update: {err:?}");
                    stats.rejected += 1;
                }
            },
            WIRE_NODE_ANNOUNCEMENT => match NodeAnnouncement::read(&mut body) {
                Ok(ann) => match graph.update_node_from_unsigned_announcement(&ann.contents) {
                    Ok(_) => stats.node_announcements += 1,
                    Err(err) => {
                        log::trace!("gossip_store: node_announcement rejected: {err:?}");
                        stats.rejected += 1;
                    }
                },
                Err(err) => {
                    log::debug!("gossip_store: cannot parse node_announcement: {err:?}");
                    stats.rejected += 1;
                }
            },
            _ => stats.skipped += 1,
        }
    }
    stats.offset = reader.offset();
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;
    use std::sync::Arc;

    use clightningrpc_gossip_map::flags::GOSSIP_STORE_LEN_DELETED_BIT;
    use lampo_common::conf::Network;
    use lampo_common::utils::logger::LampoLogger;

    use super::*;

    fn record(flags: u16, msg: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&(msg.len() as u16).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(msg);
        out
    }

    #[test]
    fn skips_unknown_and_deleted_records_and_resumes() {
        let dir = std::env::temp_dir().join(format!("barq-gossip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("gossip_store");
        let mut f = File::create(&path).unwrap();
        f.write_all(&[14u8]).unwrap();
        // gossip_store_channel_amount (4101): skipped on purpose.
        f.write_all(&record(0, &[0x10, 0x05, 0, 0, 0, 0, 0, 0, 0, 1]))
            .unwrap();
        // A deleted channel_update: skipped without parsing.
        f.write_all(&record(GOSSIP_STORE_LEN_DELETED_BIT, &[0x01, 0x02, 0xff]))
            .unwrap();
        f.flush().unwrap();

        let logger = Arc::new(LampoLogger::new());
        let graph = NetworkGraph::new(Network::Regtest, logger);
        let mut reader = GossipStoreReader::new(&path);
        let stats = sync(&mut reader, &graph).unwrap();
        assert_eq!(stats.skipped, 2);
        assert_eq!(stats.rejected, 0);
        let first_offset = stats.offset;

        // Nothing new: nothing read.
        let stats = sync(&mut reader, &graph).unwrap();
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.offset, first_offset);

        // Append a garbage channel_update: parsed and rejected, offset moves.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(&record(0, &[0x01, 0x02, 0xff])).unwrap();
        f.flush().unwrap();
        let stats = sync(&mut reader, &graph).unwrap();
        assert_eq!(stats.rejected, 1);
        assert!(stats.offset > first_offset);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
