//! Short channel id helpers.
//!
//! LDK represents a short channel id as a `u64`, while Core Lightning uses
//! the human readable `BLOCKxTXxOUT` form everywhere in its JSON-RPC API
//! (see `short_channel_id_from_str` in `bitcoin/short_channel_id.c`).
use anyhow::Result;

/// Convert a `u64` short channel id into the `BLOCKxTXxOUT` form accepted
/// by Core Lightning.
pub fn to_cln(scid: u64) -> String {
    let block = scid >> 40;
    let tx = (scid >> 16) & 0xFF_FFFF;
    let out = scid & 0xFFFF;
    format!("{block}x{tx}x{out}")
}

/// Parse a Core Lightning `BLOCKxTXxOUT` short channel id into a `u64`.
pub fn from_cln(scid: &str) -> Result<u64> {
    let parts: Vec<&str> = scid.split('x').collect();
    if parts.len() != 3 {
        anyhow::bail!("Invalid short channel id `{scid}`: expected BLOCKxTXxOUT");
    }
    let block: u64 = parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid block number in `{scid}`"))?;
    let tx: u64 = parts[1]
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid tx index in `{scid}`"))?;
    let out: u64 = parts[2]
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid output index in `{scid}`"))?;
    if block > 0xFF_FFFF || tx > 0xFF_FFFF || out > 0xFFFF {
        anyhow::bail!("Short channel id `{scid}` out of range");
    }
    Ok((block << 40) | (tx << 16) | out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for s in [
            "0x0x0",
            "103x1x0",
            "2427540x16x0",
            "16777215x16777215x65535",
        ] {
            let v = from_cln(s).unwrap();
            assert_eq!(to_cln(v), s);
        }
    }

    #[test]
    fn known_value() {
        let expected = (103u64 << 40) | (1u64 << 16);
        assert_eq!(from_cln("103x1x0").unwrap(), expected);
        assert_eq!(to_cln(expected), "103x1x0");
    }

    #[test]
    fn rejects_garbage() {
        assert!(from_cln("12345").is_err());
        assert!(from_cln("1x2").is_err());
        assert!(from_cln("ax2x3").is_err());
        assert!(from_cln("1x2x70000").is_err());
    }
}
