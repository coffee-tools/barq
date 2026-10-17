//! Payment history (issues #7 and #8).
//!
//! Every `barqpay` call is recorded as a [`PaymentRecord`]: the invoice,
//! the amounts, every route tried with its outcome, and a journal of what
//! barq did. Records are stored as one JSON file each under
//! `<lightning-dir>/<network>/barq/payments/`, written atomically, so a
//! crash never leaves a half-written record. See `docs/persistence.md` for
//! why these fields were chosen.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One hop of a route, in Core Lightning terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HopRecord {
    pub id: String,
    pub channel: String,
    pub fee_msat: u64,
    pub cltv_expiry_delta: u32,
}

/// One route tried for a payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRecord {
    pub strategy: String,
    pub route: Vec<HopRecord>,
    pub fee_msat: u64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erring_channel: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PaymentStatus {
    Pending,
    Complete,
    Failed,
}

/// The final error of a failed payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentError {
    pub code: i32,
    pub message: String,
}

/// A line of the payment's journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Unix time in milliseconds.
    pub at: u64,
    pub level: String,
    pub message: String,
}

/// Everything barq knows about one `barqpay` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentRecord {
    /// Assigned by the store, increasing.
    pub id: u64,
    pub payment_hash: String,
    pub invoice: String,
    pub invoice_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    pub amount_msat: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_sent_msat: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_msat: Option<u64>,
    pub status: PaymentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payment_preimage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Unix time in milliseconds.
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PaymentError>,
    pub attempts: Vec<AttemptRecord>,
    pub events: Vec<Event>,
}

/// The short form returned when listing payments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentSummary {
    pub id: u64,
    pub payment_hash: String,
    pub invoice_type: String,
    pub amount_msat: u64,
    pub status: PaymentStatus,
    pub attempts: usize,
    pub created_at: u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl PaymentRecord {
    pub fn new(
        payment_hash: &str,
        invoice: &str,
        invoice_type: &str,
        destination: Option<String>,
        amount_msat: u64,
        label: Option<String>,
    ) -> Self {
        Self {
            id: 0,
            payment_hash: payment_hash.to_owned(),
            invoice: invoice.to_owned(),
            invoice_type: invoice_type.to_owned(),
            destination,
            amount_msat,
            amount_sent_msat: None,
            fee_msat: None,
            status: PaymentStatus::Pending,
            payment_preimage: None,
            label,
            created_at: now_ms(),
            completed_at: None,
            error: None,
            attempts: vec![],
            events: vec![],
        }
    }

    /// Add a line to the journal and to the plugin log.
    pub fn event(&mut self, level: log::Level, message: impl Into<String>) {
        let message = message.into();
        log::log!(level, "payment {}: {message}", self.id);
        self.events.push(Event {
            at: now_ms(),
            level: level.as_str().to_lowercase(),
            message,
        });
    }

    pub fn complete(&mut self, preimage: Option<String>, amount_sent_msat: u64) {
        self.status = PaymentStatus::Complete;
        self.payment_preimage = preimage;
        self.amount_sent_msat = Some(amount_sent_msat);
        self.fee_msat = Some(amount_sent_msat.saturating_sub(self.amount_msat));
        self.completed_at = Some(now_ms());
    }

    pub fn fail(&mut self, code: i32, message: &str) {
        self.status = PaymentStatus::Failed;
        self.error = Some(PaymentError {
            code,
            message: message.to_owned(),
        });
        self.completed_at = Some(now_ms());
    }

    pub fn summary(&self) -> PaymentSummary {
        PaymentSummary {
            id: self.id,
            payment_hash: self.payment_hash.clone(),
            invoice_type: self.invoice_type.clone(),
            amount_msat: self.amount_msat,
            status: self.status,
            attempts: self.attempts.len(),
            created_at: self.created_at,
        }
    }
}

/// JSON files, one per payment, named by id.
#[derive(Debug)]
pub struct PaymentStore {
    dir: PathBuf,
    next_id: AtomicU64,
}

impl PaymentStore {
    /// Open (or create) the store in `dir`, continuing after the highest
    /// id already stored.
    pub fn open<P: AsRef<Path>>(dir: P) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let highest = Self::ids_in(&dir)?.into_iter().max().unwrap_or(0);
        Ok(Self {
            dir,
            next_id: AtomicU64::new(highest + 1),
        })
    }

    fn ids_in(dir: &Path) -> Result<Vec<u64>> {
        let mut ids = vec![];
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = name.strip_suffix(".json").and_then(|n| n.parse().ok()) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn path(&self, id: u64) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Give `record` the next id and store it.
    pub fn create(&self, record: &mut PaymentRecord) -> Result<u64> {
        record.id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.save(record)?;
        Ok(record.id)
    }

    /// Write `record`, replacing any previous version atomically.
    pub fn save(&self, record: &PaymentRecord) -> Result<()> {
        let path = self.path(record.id);
        let tmp = path.with_extension("json.tmp");
        {
            let mut file =
                fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
            file.write_all(&serde_json::to_vec_pretty(record)?)?;
            file.sync_all()?;
        }
        fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn get(&self, id: u64) -> Result<Option<PaymentRecord>> {
        let path = self.path(id);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        Ok(Some(
            serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", path.display()))?,
        ))
    }

    /// Every readable record, newest first. Unreadable files are skipped.
    pub fn all(&self) -> Result<Vec<PaymentRecord>> {
        let mut ids = Self::ids_in(&self.dir)?;
        ids.sort_unstable_by(|a, b| b.cmp(a));
        Ok(ids
            .into_iter()
            .filter_map(|id| match self.get(id) {
                Ok(record) => record,
                Err(err) => {
                    log::warn!("skipping unreadable payment record {id}: {err:#}");
                    None
                }
            })
            .collect())
    }

    /// Records of payments to `payment_hash`, newest first.
    pub fn by_payment_hash(&self, payment_hash: &str) -> Result<Vec<PaymentRecord>> {
        Ok(self
            .all()?
            .into_iter()
            .filter(|r| r.payment_hash == payment_hash)
            .collect())
    }

    /// The `limit` most recent payments.
    pub fn list(&self, limit: usize) -> Result<Vec<PaymentSummary>> {
        Ok(self
            .all()?
            .into_iter()
            .take(limit)
            .map(|r| r.summary())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("barq-history-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn record(hash: &str) -> PaymentRecord {
        PaymentRecord::new(
            hash,
            "lnbcrt1...",
            "bolt11",
            Some("02ab".into()),
            1_000,
            None,
        )
    }

    #[test]
    fn create_get_and_update() {
        let dir = temp_dir("crud");
        let store = PaymentStore::open(&dir).unwrap();
        let mut r = record("aa");
        let id = store.create(&mut r).unwrap();
        assert_eq!(id, 1);
        assert_eq!(
            store.get(1).unwrap().unwrap().status,
            PaymentStatus::Pending
        );

        r.event(log::Level::Info, "attempt 1");
        r.attempts.push(AttemptRecord {
            strategy: "direct".into(),
            route: vec![HopRecord {
                id: "02ab".into(),
                channel: "103x1x0".into(),
                fee_msat: 1_000,
                cltv_expiry_delta: 18,
            }],
            fee_msat: 0,
            status: "complete".into(),
            error: None,
            erring_channel: None,
        });
        r.complete(Some("ff".into()), 1_000);
        store.save(&r).unwrap();

        let stored = store.get(1).unwrap().unwrap();
        assert_eq!(stored, r);
        assert_eq!(stored.status, PaymentStatus::Complete);
        assert_eq!(stored.fee_msat, Some(0));
        assert_eq!(stored.events[0].message, "attempt 1");
        assert!(store.get(2).unwrap().is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ids_keep_increasing_after_reopen() {
        let dir = temp_dir("reopen");
        {
            let store = PaymentStore::open(&dir).unwrap();
            store.create(&mut record("aa")).unwrap();
            store.create(&mut record("bb")).unwrap();
        }
        let store = PaymentStore::open(&dir).unwrap();
        assert_eq!(store.create(&mut record("cc")).unwrap(), 3);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn list_and_lookup_by_hash_newest_first() {
        let dir = temp_dir("list");
        let store = PaymentStore::open(&dir).unwrap();
        for hash in ["aa", "bb", "aa"] {
            store.create(&mut record(hash)).unwrap();
        }
        let mut failed = record("cc");
        store.create(&mut failed).unwrap();
        failed.fail(204, "WIRE_TEMPORARY_CHANNEL_FAILURE");
        store.save(&failed).unwrap();

        let ids: Vec<_> = store.list(10).unwrap().iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![4, 3, 2, 1]);
        assert_eq!(store.list(2).unwrap().len(), 2);
        let aa: Vec<_> = store
            .by_payment_hash("aa")
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert_eq!(aa, vec![3, 1]);
        let cc = &store.by_payment_hash("cc").unwrap()[0];
        assert_eq!(cc.status, PaymentStatus::Failed);
        assert_eq!(cc.error.as_ref().unwrap().code, 204);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_records_are_skipped() {
        let dir = temp_dir("corrupt");
        let store = PaymentStore::open(&dir).unwrap();
        store.create(&mut record("aa")).unwrap();
        fs::write(dir.join("2.json"), b"{ not json").unwrap();
        fs::write(dir.join("notes.txt"), b"ignored").unwrap();
        assert_eq!(store.list(10).unwrap().len(), 1);
        assert!(store.get(2).is_err());
        // The next id still goes past the corrupt file.
        let store = PaymentStore::open(&dir).unwrap();
        assert_eq!(store.create(&mut record("bb")).unwrap(), 3);
        fs::remove_dir_all(dir).unwrap();
    }
}
