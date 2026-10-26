//! Opt-in test on recorded TLV stream files (`isdb rx --system isdbs3
//! --out 'x-{tlv_id}.tlv'`): `ISDB_RS_ISDBS3_TLV_FILES` names them,
//! `:`-separated. Without it the test prints a `SKIP` line and passes. See
//! docs/test-data.md.

use isdb_mmt::Decoder;
use isdb_tlv::tlv::FileReader;

#[test]
fn isdbs3_tlv_files_decode_cleanly() {
    let Ok(list) = std::env::var("ISDB_RS_ISDBS3_TLV_FILES") else {
        eprintln!("SKIP isdbs3_tlv_files_decode_cleanly: ISDB_RS_ISDBS3_TLV_FILES not set");
        return;
    };
    for path in list.split(':').filter(|p| !p.is_empty()) {
        let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut r = FileReader::new(std::io::BufReader::new(f));
        let mut d = Decoder::new();
        let mut mfus = Vec::new();
        while let Some(p) = r.next_packet().unwrap() {
            d.push(&p, &mut mfus);
            mfus.clear();
        }
        let c = &d.counters;
        eprintln!(
            "{path}: {} TLV packets, {} tables, {} MPTs, {} MH-EIT sections, {} MH-SDT sections, NTP {}",
            c.tlv_packets,
            c.tables,
            d.mpts.len(),
            d.eits.len(),
            d.sdts.len(),
            c.ntp
        );
        assert_eq!(r.skipped, 0, "{path}: bytes outside TLV packets");
        assert_eq!((c.ip_errors, c.non_mmtp), (0, 0), "{path}: transport");
        assert_eq!((c.crc_errors, c.malformed), (0, 0), "{path}: tables");
        assert!(!d.mpts.is_empty(), "{path}: no MPT");
        assert!(d.tot.is_some(), "{path}: no MH-TOT");
        assert!(c.ntp > 0, "{path}: no NTP");
        assert_eq!(
            isdb_mmt::desc::split_errors(),
            0,
            "{path}: descriptor loops"
        );
        for (pid, s) in d.mpu_stats() {
            let (_, a) = d
                .asset_of(pid)
                .unwrap_or_else(|| panic!("{path}: packet {pid:#06x} in no MPT"));
            assert_eq!(
                (s.packets_lost, s.malformed),
                (0, 0),
                "{path}: asset {pid:#06x}"
            );
            if matches!(&a.asset_type, b"hev1" | b"mp4a" | b"stpp") {
                assert_eq!(
                    s.mpu_sequence_jumps, 0,
                    "{path}: asset {pid:#06x} MPU continuity"
                );
            }
        }
    }
}
