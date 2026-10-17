//! Incremental reader for Core Lightning's `gossip_store` file.
//!
//! The file layout (see `common/gossip_store.h` in Core Lightning) is a
//! single version byte followed by records of:
//!
//! ```text
//! u16 flags | u16 len | u32 crc | u32 timestamp | len bytes of BOLT7 message
//! ```
//!
//! Core Lightning only appends to the file (and occasionally rewrites it
//! during compaction), so we remember the offset we stopped at and only
//! parse new records on the next sync. Every raw `channel_announcement`,
//! `channel_update` and `node_announcement` is fed to the LDK network
//! graph as is; signatures are not re-verified since they were already
//! checked by `gossipd`.
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::ops::Deref;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use lampo_common::ldk::ln::msgs::{ChannelAnnouncement, ChannelUpdate, NodeAnnouncement};
use lampo_common::ldk::routing::gossip::NetworkGraph;
use lampo_common::ldk::util::logger::Logger;
use lampo_common::ldk::util::ser::Readable;

const GOSSIP_STORE_MAJOR_VERSION_MASK: u8 = 0xE0;
const GOSSIP_STORE_DELETED_BIT: u16 = 0x8000;
const GOSSIP_STORE_DYING_BIT: u16 = 0x0800;
const HEADER_LEN: u64 = 12;

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

/// Keeps track of how far into the `gossip_store` we have read.
#[derive(Debug)]
pub struct GossipStoreReader {
    path: PathBuf,
    offset: u64,
    version: Option<u8>,
}

impl GossipStoreReader {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            offset: 0,
            version: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Forget the position: the next sync re-reads the whole file.
    pub fn reset(&mut self) {
        self.offset = 0;
        self.version = None;
    }

    /// Read every record appended since the last sync and feed it to
    /// `graph`.
    pub fn sync<L: Deref>(&mut self, graph: &NetworkGraph<L>) -> Result<GossipStoreStats>
    where
        L::Target: Logger,
    {
        let mut file = File::open(&self.path)
            .with_context(|| format!("opening gossip store {}", self.path.display()))?;
        let file_len = file.metadata()?.len();
        if file_len == 0 {
            return Ok(GossipStoreStats::default());
        }

        let mut version = [0u8; 1];
        file.read_exact(&mut version)?;
        let version = version[0];
        if version & GOSSIP_STORE_MAJOR_VERSION_MASK != 0 {
            anyhow::bail!("Unsupported gossip store major version {}", version >> 5);
        }

        // The store was rewritten (compaction) or truncated: start over.
        if self.version != Some(version) || file_len < self.offset || self.offset < 1 {
            self.version = Some(version);
            self.offset = 1;
        }

        let mut stats = GossipStoreStats::default();
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file);
        let mut header = [0u8; HEADER_LEN as usize];

        loop {
            if self.offset + HEADER_LEN > file_len {
                break;
            }
            if reader.read_exact(&mut header).is_err() {
                break;
            }
            let flags = u16::from_be_bytes([header[0], header[1]]);
            let len = u16::from_be_bytes([header[2], header[3]]) as u64;
            if self.offset + HEADER_LEN + len > file_len {
                // Partially written record: retry on the next sync.
                break;
            }
            let mut msg = vec![0u8; len as usize];
            reader.read_exact(&mut msg)?;
            self.offset += HEADER_LEN + len;

            if flags & (GOSSIP_STORE_DELETED_BIT | GOSSIP_STORE_DYING_BIT) != 0 || msg.len() < 2 {
                stats.skipped += 1;
                continue;
            }
            let ty = u16::from_be_bytes([msg[0], msg[1]]);
            let mut body = &msg[2..];
            match ty {
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
        stats.offset = self.offset;
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::Arc;

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
        f.write_all(&record(GOSSIP_STORE_DELETED_BIT, &[0x01, 0x02, 0xff]))
            .unwrap();
        f.flush().unwrap();

        let logger = Arc::new(LampoLogger::new());
        let graph = NetworkGraph::new(Network::Regtest, logger);
        let mut reader = GossipStoreReader::new(&path);
        let stats = reader.sync(&graph).unwrap();
        assert_eq!(stats.skipped, 2);
        assert_eq!(stats.rejected, 0);
        let first_offset = stats.offset;

        // Nothing new: nothing read.
        let stats = reader.sync(&graph).unwrap();
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.offset, first_offset);

        // Append a garbage channel_update: parsed and rejected, offset moves.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(&record(0, &[0x01, 0x02, 0xff])).unwrap();
        f.flush().unwrap();
        let stats = reader.sync(&graph).unwrap();
        assert_eq!(stats.rejected, 1);
        assert!(stats.offset > first_offset);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
