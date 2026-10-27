//! Measurement of a produced transport stream: section repetition, PCR,
//! PES timestamps versus arrival times, continuity counters. Used by the
//! tests and by `isdb mux --report`.

use std::collections::{BTreeMap, HashMap, HashSet};

use isdb_ts::Pid;
use isdb_ts::cc::{CcChecker, CcEvent};
use isdb_ts::demux::{Assembled, SectionDemux};
use isdb_ts::packet::Packet as TsPacket;
use isdb_ts::pes::{PesAssembler, PesOutput, PesPacket};
use isdb_ts::psi::{Pat, Pmt};
use isdb_ts::section::Long;

use crate::mux::Packet;

/// One reassembled PES packet with its arrival times.
#[derive(Debug, Clone)]
pub struct PesInfo {
    /// stream_id.
    pub stream_id: u8,
    /// Arrival (27 MHz) of the first packet.
    pub first_arrival: u64,
    /// Arrival (27 MHz) of the end of the last packet.
    pub last_arrival: u64,
    /// PTS (90 kHz).
    pub pts: Option<u64>,
    /// DTS (90 kHz).
    pub dts: Option<u64>,
    /// Bytes.
    pub len: usize,
}

/// Measurements of a stream.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Sections per PID: (arrival of first packet, bytes).
    pub sections: BTreeMap<Pid, Vec<(u64, Vec<u8>)>>,
    /// PCRs per PID: (packet index, arrival of packet start, PCR ticks).
    pub pcrs: BTreeMap<Pid, Vec<(u64, u64, u64)>>,
    /// PES packets per PID.
    pub pes: BTreeMap<Pid, Vec<PesInfo>>,
    /// Continuity events.
    pub cc_events: Vec<CcEvent>,
    /// Packets per PID.
    pub pid_counts: BTreeMap<Pid, u64>,
    /// Problems found while parsing.
    pub problems: Vec<String>,
}

/// Analyse `packets` whose first bytes arrive at `times` (27 MHz).
/// `packet_ticks` is the duration of one packet (for the last-byte time).
pub fn analyze(packets: &[Packet], times: &[u64], packet_ticks: u64) -> Report {
    let mut r = Report::default();
    let mut demux = SectionDemux::new();
    for p in [
        0x0000u16, 0x0001, 0x0010, 0x0011, 0x0012, 0x0013, 0x0014, 0x0024, 0x0025, 0x0026, 0x0027,
    ] {
        demux.add_pid(Pid::new(p).expect("Table 5-1 PID"));
    }
    let mut pmt_pids: HashSet<Pid> = HashSet::new();
    let mut es_pids: HashSet<Pid> = HashSet::new();
    let mut pes_asm: HashMap<Pid, PesAssembler> = HashMap::new();
    let mut cc = CcChecker::new();
    let finish_pes = |r: &mut Report, out: Vec<PesOutput>| {
        for o in out {
            match o {
                PesOutput::Pes(a) => match PesPacket::decode(&a.data) {
                    Ok(p) => {
                        let h = p.header();
                        r.pes.entry(a.pid).or_default().push(PesInfo {
                            stream_id: p.stream_id,
                            first_arrival: times[a.start_packet as usize],
                            last_arrival: times[a.end_packet as usize] + packet_ticks,
                            pts: h.and_then(|h| h.pts_dts.pts()),
                            dts: h.and_then(|h| h.pts_dts.dts()),
                            len: a.data.len(),
                        });
                    }
                    Err(e) => r.problems.push(format!("PES on {:#06x}: {e}", a.pid)),
                },
                PesOutput::Event(e) => r.problems.push(format!("PES event {e:?}")),
            }
        }
    };
    for (i, pkt) in packets.iter().enumerate() {
        let i = i as u64;
        let pid = Pid::of_packet(pkt).expect("188-byte packet");
        *r.pid_counts.entry(pid).or_default() += 1;
        if pid == Pid::NULL {
            continue;
        }
        if let Some(ev) = cc.push(i, pkt) {
            r.cc_events.push(ev);
        }
        if let Ok(p) = TsPacket::decode(pkt)
            && let Some(pcr) = p.adaptation_field.as_ref().and_then(|a| a.pcr())
        {
            r.pcrs
                .entry(pid)
                .or_default()
                .push((i, times[i as usize], pcr.ticks()));
        }
        for a in demux.push(i, pkt) {
            match a {
                Assembled::Section(s) => {
                    if s.pid == Pid::PAT
                        && let Ok(pat) = Long::<Pat>::decode(&s.data)
                    {
                        for (_, pmt) in pat.body.programs() {
                            if pmt_pids.insert(pmt) {
                                demux.add_pid(pmt);
                            }
                        }
                    }
                    if pmt_pids.contains(&s.pid)
                        && let Ok(pmt) = Long::<Pmt>::decode(&s.data)
                    {
                        for st in &pmt.body.streams {
                            if es_pids.insert(st.elementary_pid) {
                                pes_asm.insert(
                                    st.elementary_pid,
                                    PesAssembler::new(st.elementary_pid),
                                );
                            }
                        }
                    }
                    r.sections
                        .entry(s.pid)
                        .or_default()
                        .push((times[s.start_packet as usize], s.data));
                }
                Assembled::Event(e) => r.problems.push(format!("section event {e:?}")),
            }
        }
        if let Some(a) = pes_asm.get_mut(&pid) {
            let out = a.push(i, pkt);
            finish_pes(&mut r, out);
        }
    }
    r
}

/// Key identifying a repeated section: (PID, table_id, table_id_extension, section_number).
pub type SectionKey = (Pid, u8, u16, u8);

impl Report {
    /// Largest gap (27 MHz ticks) between successive transmissions of each
    /// section, and the number of transmissions.
    pub fn section_intervals(&self) -> BTreeMap<SectionKey, (u64, usize)> {
        let mut last: HashMap<SectionKey, u64> = HashMap::new();
        let mut out: BTreeMap<SectionKey, (u64, usize)> = BTreeMap::new();
        for (&pid, secs) in &self.sections {
            for (t, s) in secs {
                let long = s.len() >= 8 && s[1] & 0x80 != 0;
                let key = (
                    pid,
                    s[0],
                    if long {
                        u16::from_be_bytes([s[3], s[4]])
                    } else {
                        0
                    },
                    if long { s[6] } else { 0 },
                );
                let e = out.entry(key).or_insert((0, 0));
                e.1 += 1;
                if let Some(p) = last.insert(key, *t) {
                    e.0 = e.0.max(t - p);
                }
            }
        }
        out
    }
}
