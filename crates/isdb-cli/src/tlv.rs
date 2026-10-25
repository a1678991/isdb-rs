//! `isdb tlv inspect`: the content of a TLV stream file (TLV packets back
//! to back, as `isdb rx --system isdbs3` writes them): packets by type,
//! IP / compressed IP, MMTP packet ids, MMT-SI messages and tables, TLV-SI
//! sections, and the share of MPU payloads that carry a scrambling
//! extension header. No descrambling.

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};
use isdb_tlv::stats::StreamStats;
use isdb_tlv::tlv::{Packet, PacketType, SYNC};
use isdb_tlv::{mmtp, si};
use serde_json::json;
use std::path::PathBuf;

/// `isdb tlv` arguments.
#[derive(Debug, Args)]
pub struct TlvArgs {
    #[command(subcommand)]
    cmd: TlvCmd,
}

#[derive(Debug, Subcommand)]
enum TlvCmd {
    /// Summarise a TLV stream file.
    Inspect(InspectArgs),
}

#[derive(Debug, Args)]
struct InspectArgs {
    /// The TLV stream file.
    file: PathBuf,
    /// Print JSON.
    #[arg(long)]
    json: bool,
}

/// Split a TLV stream file into packets; stops at the first byte that is
/// not a TLV header (returns the offset).
pub fn packets(b: &[u8]) -> (Vec<Packet>, Option<usize>) {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 4 <= b.len() {
        if b[p] != SYNC {
            return (out, Some(p));
        }
        let len = usize::from(u16::from_be_bytes([b[p + 2], b[p + 3]]));
        if p + 4 + len > b.len() {
            return (out, Some(p));
        }
        out.push(Packet {
            kind: PacketType::from_byte(b[p + 1]),
            data: b[p + 4..p + 4 + len].to_vec(),
        });
        p += 4 + len;
    }
    (out, (p != b.len()).then_some(p))
}

/// Run `isdb tlv`.
pub fn run(a: &TlvArgs) -> Result<()> {
    let TlvCmd::Inspect(a) = &a.cmd;
    let b = std::fs::read(&a.file).with_context(|| format!("read {}", a.file.display()))?;
    let (ps, stop) = packets(&b);
    if ps.is_empty() {
        bail!("{}: no TLV packet at the start", a.file.display());
    }
    let mut st = StreamStats::default();
    for p in &ps {
        st.add(p);
    }
    let scrambled = if st.mpu > 0 {
        100.0 * st.mpu_with_extension as f64 / st.mpu as f64
    } else {
        0.0
    };
    if a.json {
        let j = json!({
            "file": a.file.display().to_string(),
            "bytes": b.len(),
            "tlv_packets": ps.len(),
            "trailing_garbage_at": stop,
            "by_type": st.by_type.iter().map(|(k, (n, b))| (k.clone(), json!({"packets": n, "bytes": b}))).collect::<serde_json::Map<_, _>>(),
            "ip_errors": st.ip_errors,
            "non_mmtp_udp": st.non_mmtp,
            "hcfb_contexts": st.contexts.keys().collect::<Vec<_>>(),
            "mmtp_packet_ids": st.mmtp.iter().map(|(k, (n, t))| (format!("{k:#06x}"), json!({"packets": n, "payload_type": t}))).collect::<serde_json::Map<_, _>>(),
            "mmt_si_messages": st.messages.iter().map(|(k, n)| (format!("{k:#06x} {}", mmtp::message_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
            "mmt_si_tables": st.mmt_tables.iter().map(|(k, n)| (format!("{k:#04x} {}", mmtp::table_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
            "tlv_si_tables": st.tlv_si.iter().map(|(k, n)| (format!("{k:#04x} {}", si::table_name(*k)), json!(n))).collect::<serde_json::Map<_, _>>(),
            "mpu_payloads": st.mpu,
            "mpu_with_scramble_info": st.mpu_with_extension,
        });
        println!("{}", serde_json::to_string_pretty(&j)?);
        return Ok(());
    }
    println!(
        "{}: {} bytes, {} TLV packets",
        a.file.display(),
        b.len(),
        ps.len()
    );
    if let Some(p) = stop {
        println!("  stops at byte {p}: not a TLV header (truncated or not a TLV stream)");
    }
    for (k, (n, b)) in &st.by_type {
        println!("  {k}: {n} packets, {b} bytes");
    }
    println!(
        "  IP errors {}, non-MMTP UDP payloads {}, HCfB contexts {:?}",
        st.ip_errors,
        st.non_mmtp,
        st.contexts.keys().collect::<Vec<_>>()
    );
    let ids: Vec<String> = st
        .mmtp
        .iter()
        .map(|(k, (n, t))| format!("{k:#06x} ×{n} (payload type {t})"))
        .collect();
    println!("  MMTP packet ids: {}", ids.join(", "));
    for (k, n) in &st.messages {
        println!("  MMT-SI {k:#06x} {}: {n}", mmtp::message_name(*k));
    }
    for (k, n) in &st.mmt_tables {
        println!("  MMT-SI table {k:#04x} {}: {n}", mmtp::table_name(*k));
    }
    for (k, n) in &st.tlv_si {
        println!("  TLV-SI table {k:#04x} {}: {n}", si::table_name(*k));
    }
    println!(
        "  MPU payloads {}, with scramble information in the MMTP header {} ({scrambled:.1} %; B60 Table 6-9 type 0001h, content per STD-B61, not interpreted)",
        st.mpu, st.mpu_with_extension
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_tlv_file() {
        let a = Packet {
            kind: PacketType::Null,
            data: vec![0xFF; 5],
        };
        let mut b = a.bytes();
        b.extend(a.bytes());
        let (ps, stop) = packets(&b);
        assert_eq!(ps.len(), 2);
        assert_eq!(stop, None);
        b.push(0x00);
        assert_eq!(packets(&b).1, Some(18));
    }
}
