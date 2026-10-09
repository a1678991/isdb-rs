//! Spec-value conformance of decoded SI, separate from the lossless codecs.
//!
//! [`check`] lists every place where a decoded section carries a value the
//! ARIB STD-B10 v5.13 text forbids or reserves: reserved /
//! reserved_future_use bits not all '1' (Part 2 §3.1: "Unless otherwise
//! specified … all 'reserved' bits shall be set to '1'" and the same for
//! "reserved_future_use"), lengths over the stated maxima, reserved code
//! points, invalid MJD/BCD times, missing descriptors that Table 6-1 marks
//! mandatory unconditionally, and table_id/PID pairs not in Table 5-1.
//! Only rules stated in the B10 text are checked; operational guidelines
//! are not.

use crate::any::{AnyTable, table_allowed_on_pid};
use crate::descriptor::*;
use crate::tables::*;
use crate::time::{BcdDuration, BcdOffset, MjdTime};
use arib_ts::section::{LongHeader, ShortHeader};

/// Category of a deviation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// reserved / reserved_future_use bits not all '1' (B10 Part 2 §3.1).
    ReservedBits,
    /// A code point the spec marks reserved / undefined.
    ReservedValue,
    /// A value outside its stated range (BCD digits, time of day, …).
    Range,
    /// A length over its stated maximum or inconsistent.
    Length,
    /// A descriptor B10 Part 2 Table 6-1 marks mandatory is absent.
    MandatoryDescriptor,
    /// table_id on a PID Table 5-1 does not assign it to.
    Placement,
}

/// One deviation from a B10-stated value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deviation {
    /// Clause/table stating the rule, e.g. `"B10 Part 2 §5.2.6 Table 5-5"`.
    pub clause: &'static str,
    /// Category.
    pub kind: Kind,
    /// Where (table, ids, loop, descriptor).
    pub location: String,
    /// What is wrong.
    pub message: String,
}

impl std::fmt::Display for Deviation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{:?}] {} — {}: {}",
            self.kind, self.clause, self.location, self.message
        )
    }
}

struct Out<'a> {
    v: &'a mut Vec<Deviation>,
    loc: String,
}

impl Out<'_> {
    fn push(&mut self, clause: &'static str, kind: Kind, message: impl Into<String>) {
        self.v.push(Deviation {
            clause,
            kind,
            location: self.loc.clone(),
            message: message.into(),
        });
    }
    fn ones(&mut self, clause: &'static str, field: &str, value: u64, bits: u32) {
        let all = if bits == 64 {
            u64::MAX
        } else {
            (1 << bits) - 1
        };
        if value != all {
            self.push(
                clause,
                Kind::ReservedBits,
                format!(
                    "{field} = {value:#0w$b}, shall be all '1' ({bits} bits)",
                    w = bits as usize + 2
                ),
            );
        }
    }
    fn at(&mut self, sub: impl std::fmt::Display) -> Out<'_> {
        Out {
            v: self.v,
            loc: format!("{} / {sub}", self.loc),
        }
    }
}

const RESERVED: &str = "B10 Part 2 §3.1 (reserved / reserved_future_use)";

/// Check one decoded section carried on `pid`.
pub fn check(pid: u16, table: &AnyTable) -> Vec<Deviation> {
    let mut v = Vec::new();
    let section_length = table
        .encode()
        .map(|b| b.len().saturating_sub(3))
        .unwrap_or(0);
    let loc = format!(
        "{} tid={:#04x} pid={:#06x}",
        table.name(),
        table.table_id(),
        pid
    );
    let mut o = Out { v: &mut v, loc };
    match table {
        AnyTable::Nit(t) => {
            long_header(
                &mut o,
                &t.header,
                section_length,
                1021,
                "B10 Part 2 §5.2.4 Table 5-3",
            );
            check_nit(&mut o, t);
        }
        AnyTable::Bat(t) => {
            long_header(
                &mut o,
                &t.header,
                section_length,
                1021,
                "B10 Part 2 §5.2.5 Table 5-4",
            );
            check_bat(&mut o, t);
        }
        AnyTable::Sdt(t) => {
            long_header(
                &mut o,
                &t.header,
                section_length,
                1021,
                "B10 Part 2 §5.2.6 Table 5-5",
            );
            check_sdt(&mut o, t);
        }
        AnyTable::Eit(t) => {
            long_header(
                &mut o,
                &t.header,
                section_length,
                4093,
                "B10 Part 2 §5.2.7 Table 5-7",
            );
            check_eit(&mut o, t);
        }
        AnyTable::Tdt(t) => {
            short_header(&mut o, &t.header);
            if section_length != 5 {
                o.push(
                    "B10 Part 2 §5.2.8",
                    Kind::Length,
                    format!("section_length {section_length}, shall be 0x005"),
                );
            }
            time(
                &mut o,
                "B10 Part 2 §5.2.8 JST_time",
                "JST_time",
                t.body.jst_time,
                false,
            );
        }
        AnyTable::Tot(t) => {
            short_header(&mut o, &t.header);
            if section_length > 0x3FF {
                o.push(
                    "B10 Part 2 §5.2.9",
                    Kind::Length,
                    format!("section_length {section_length}: first two bits shall be '00'"),
                );
            }
            time(
                &mut o,
                "B10 Part 2 §5.2.9 JST_time",
                "JST_time",
                t.body.jst_time,
                false,
            );
            o.ones(
                "B10 Part 2 §5.2.9 Table 5-9",
                "reserved",
                t.body.reserved as u64,
                4,
            );
            descriptors(&mut o, "TOT", &t.body.descriptors);
        }
        AnyTable::Rst(t) => {
            short_header(&mut o, &t.header);
            if section_length > 1021 {
                o.push(
                    "B10 Part 2 §5.2.10",
                    Kind::Length,
                    format!("section_length {section_length} > 1021"),
                );
            }
            for e in &t.body.entries {
                let mut o = o.at(format_args!(
                    "event sid={:#06x} eid={:#06x}",
                    e.service_id, e.event_id
                ));
                o.ones(
                    "B10 Part 2 §5.2.10 Table 5-10",
                    "reserved_future_use",
                    e.reserved_future_use as u64,
                    5,
                );
                running_status(&mut o, e.running_status);
            }
        }
        AnyTable::St(t) => {
            o.ones(
                RESERVED,
                "reserved_future_use",
                t.reserved_future_use as u64,
                1,
            );
            o.ones(RESERVED, "reserved", t.reserved as u64, 2);
            if section_length > 4093 {
                o.push(
                    "B10 Part 2 §5.2.11",
                    Kind::Length,
                    format!("section_length {section_length} > 4093"),
                );
            }
        }
        AnyTable::Pcat(t) => {
            long_header(
                &mut o,
                &t.header,
                section_length,
                4093,
                "B10 Part 2 §5.2.12 Table 5-12",
            );
            for (i, c) in t.body.content_versions.iter().enumerate() {
                let mut o = o.at(format_args!("content_version[{i}]"));
                o.ones(
                    "B10 Part 2 §5.2.12 Table 5-12",
                    "reserved_future_use",
                    c.reserved_future_use_1 as u64,
                    2,
                );
                o.ones(
                    "B10 Part 2 §5.2.12 Table 5-12",
                    "reserved_future_use",
                    c.reserved_future_use_2 as u64,
                    4,
                );
                for s in &c.schedules {
                    time(
                        &mut o,
                        "B10 Part 2 §5.2.12 start_time",
                        "start_time",
                        s.start_time,
                        false,
                    );
                    duration(&mut o, "B10 Part 2 §5.2.12 duration", s.duration);
                }
                descriptors(&mut o, "content_version", &c.descriptors);
            }
        }
        AnyTable::Bit(t) => {
            long_header(
                &mut o,
                &t.header,
                section_length,
                4093,
                "B10 Part 2 §5.2.13 Table 5-13",
            );
            o.ones(
                "B10 Part 2 §5.2.13 Table 5-13",
                "reserved_future_use",
                t.body.reserved_future_use as u64,
                3,
            );
            descriptors(&mut o, "first", &t.body.first_descriptors);
            for b in &t.body.broadcasters {
                let mut o = o.at(format_args!("broadcaster_id={:#04x}", b.broadcaster_id));
                o.ones(
                    "B10 Part 2 §5.2.13 Table 5-13",
                    "reserved_future_use",
                    b.reserved_future_use as u64,
                    4,
                );
                descriptors(&mut o, "broadcaster", &b.descriptors);
            }
        }
        AnyTable::Nbit(t) => {
            long_header(
                &mut o,
                &t.header,
                section_length,
                4093,
                "B10 Part 2 §5.2.14 Table 5-14",
            );
            for i in &t.body.informations {
                let mut o = o.at(format_args!("information_id={:#06x}", i.information_id));
                o.ones(
                    "B10 Part 2 §5.2.14 Table 5-14",
                    "reserved_future_use",
                    i.reserved_future_use_1 as u64,
                    2,
                );
                o.ones(
                    "B10 Part 2 §5.2.14 Table 5-14",
                    "reserved_future_use",
                    i.reserved_future_use_2 as u64,
                    4,
                );
                if i.information_type >= 0x4 {
                    o.push(
                        "B10 Part 2 Table 5-15",
                        Kind::ReservedValue,
                        format!(
                            "information_type {:#x} reserved for future use",
                            i.information_type
                        ),
                    );
                }
                if i.description_body_location == 0b11 {
                    o.push(
                        "B10 Part 2 Table 5-16",
                        Kind::ReservedValue,
                        "description_body_location '11' reserved for future use",
                    );
                }
                descriptors(&mut o, "information", &i.descriptors);
            }
        }
        AnyTable::Ldt(t) => {
            long_header(
                &mut o,
                &t.header,
                section_length,
                4093,
                "B10 Part 2 §5.2.15 Table 5-17",
            );
            for d in &t.body.descriptions {
                let mut o = o.at(format_args!("description_id={:#06x}", d.description_id));
                o.ones(
                    "B10 Part 2 §5.2.15 Table 5-17",
                    "reserved_future_use",
                    d.reserved_future_use as u64,
                    12,
                );
                descriptors(&mut o, "description", &d.descriptors);
            }
        }
        AnyTable::Unknown(r) => {
            let si = matches!(r.table_id, 0x40..=0x4A | 0x4E..=0x73 | 0xC2 | 0xC4..=0xC7);
            if si && !table_allowed_on_pid(r.table_id, pid) {
                o.push(
                    "B10 Part 2 §5.1.3 Table 5-1",
                    Kind::Placement,
                    format!(
                        "table_id {:#04x} is not assigned to PID {pid:#06x}",
                        r.table_id
                    ),
                );
            }
        }
    }
    v
}

fn long_header(
    o: &mut Out<'_>,
    h: &LongHeader,
    section_length: usize,
    max: usize,
    clause: &'static str,
) {
    o.ones(
        clause,
        "reserved_future_use (after section_syntax_indicator)",
        h.private_indicator as u64,
        1,
    );
    o.ones(
        clause,
        "reserved (before section_length)",
        h.reserved1 as u64,
        2,
    );
    o.ones(
        clause,
        "reserved (before version_number)",
        h.reserved2 as u64,
        2,
    );
    if section_length > max {
        o.push(
            clause,
            Kind::Length,
            format!("section_length {section_length} > {max}"),
        );
    }
    if h.section_number > h.last_section_number {
        o.push(
            clause,
            Kind::Range,
            format!(
                "section_number {} > last_section_number {}",
                h.section_number, h.last_section_number
            ),
        );
    }
}

fn short_header(o: &mut Out<'_>, h: &ShortHeader) {
    o.ones(
        RESERVED,
        "reserved_future_use (after section_syntax_indicator)",
        h.private_indicator as u64,
        1,
    );
    o.ones(
        RESERVED,
        "reserved (before section_length)",
        h.reserved1 as u64,
        2,
    );
}

fn has(ds: &[Descriptor], tag: u8) -> bool {
    ds.iter().any(|d| d.tag() == tag)
}

fn network_loops(o: &mut Out<'_>, n: &NetworkLoops, clause: &'static str, first: &'static str) {
    o.ones(
        clause,
        "reserved_future_use (first loop length)",
        n.reserved_future_use_1 as u64,
        4,
    );
    o.ones(
        clause,
        "reserved_future_use (transport_stream_loop_length)",
        n.reserved_future_use_2 as u64,
        4,
    );
    descriptors(o, first, &n.descriptors);
    for ts in &n.transport_streams {
        let mut o = o.at(format_args!(
            "ts={:#06x} onid={:#06x}",
            ts.transport_stream_id, ts.original_network_id
        ));
        o.ones(
            clause,
            "reserved_future_use (transport_descriptors_length)",
            ts.reserved_future_use as u64,
            4,
        );
        descriptors(&mut o, "transport_stream", &ts.descriptors);
    }
}

fn all_nit_descriptors(n: &NetworkLoops) -> impl Iterator<Item = &Descriptor> {
    n.descriptors.iter().chain(
        n.transport_streams
            .iter()
            .flat_map(|t| t.descriptors.iter()),
    )
}

fn check_nit(o: &mut Out<'_>, t: &Nit) {
    network_loops(o, &t.body.0, "B10 Part 2 §5.2.4 Table 5-3", "network");
    if !all_nit_descriptors(&t.body.0).any(|d| d.tag() == 0x40) {
        o.push(
            "B10 Part 2 §6.1 Table 6-1",
            Kind::MandatoryDescriptor,
            "network_name_descriptor (Mandatory) absent from NIT",
        );
    }
    if t.header.table_id == 0x40 && !all_nit_descriptors(&t.body.0).any(|d| d.tag() == 0x41) {
        o.push(
            "B10 Part 2 §6.1 Table 6-1",
            Kind::MandatoryDescriptor,
            "service_list_descriptor (Mandatory in NIT actual network) absent",
        );
    }
}

fn check_bat(o: &mut Out<'_>, t: &Bat) {
    network_loops(o, &t.body.0, "B10 Part 2 §5.2.5 Table 5-4", "bouquet");
    if !all_nit_descriptors(&t.body.0).any(|d| d.tag() == 0x47) {
        o.push(
            "B10 Part 2 §6.1 Table 6-1",
            Kind::MandatoryDescriptor,
            "bouquet_name_descriptor (Mandatory in BAT) absent",
        );
    }
    if !all_nit_descriptors(&t.body.0).any(|d| d.tag() == 0x41) {
        o.push(
            "B10 Part 2 §6.1 Table 6-1",
            Kind::MandatoryDescriptor,
            "service_list_descriptor (Mandatory in BAT) absent",
        );
    }
}

fn check_sdt(o: &mut Out<'_>, t: &Sdt) {
    o.ones(
        "B10 Part 2 §5.2.6 Table 5-5",
        "reserved_future_use (after original_network_id)",
        t.body.reserved_future_use as u64,
        8,
    );
    for s in &t.body.services {
        let mut o = o.at(format_args!("service_id={:#06x}", s.service_id));
        o.ones(
            "B10 Part 2 §5.2.6 Table 5-5",
            "reserved_future_use",
            s.reserved_future_use as u64,
            3,
        );
        running_status(&mut o, s.running_status);
        if t.header.table_id == 0x42 && !has(&s.descriptors, 0x48) {
            o.push(
                "B10 Part 2 §6.1 Table 6-1",
                Kind::MandatoryDescriptor,
                "service_descriptor (Mandatory in SDT actual stream) absent",
            );
        }
        descriptors(&mut o, "service", &s.descriptors);
    }
}

fn check_eit(o: &mut Out<'_>, t: &Eit) {
    let h = &t.header;
    let b = &t.body;
    if b.segment_last_section_number < h.section_number
        || b.segment_last_section_number > h.last_section_number
    {
        o.push(
            "B10 Part 2 §5.2.7",
            Kind::Range,
            format!(
                "segment_last_section_number {} outside [section_number {}, last_section_number {}]",
                b.segment_last_section_number, h.section_number, h.last_section_number
            ),
        );
    }
    let tid = h.table_id;
    let last_ok = match tid {
        0x4E | 0x4F => b.last_table_id == tid,
        0x50..=0x5F => (tid..=0x5F).contains(&b.last_table_id),
        _ => (tid..=0x6F).contains(&b.last_table_id),
    };
    if !last_ok {
        o.push(
            "B10 Part 2 §5.2.7 / Table 5-2",
            Kind::Range,
            format!(
                "last_table_id {:#04x} not a later table of the same EIT kind as {tid:#04x}",
                b.last_table_id
            ),
        );
    }
    for e in &b.events {
        let mut o = o.at(format_args!(
            "sid={:#06x} event_id={:#06x}",
            h.table_id_extension, e.event_id
        ));
        time(
            &mut o,
            "B10 Part 2 §5.2.7 start_time",
            "start_time",
            e.start_time,
            true,
        );
        duration(&mut o, "B10 Part 2 §5.2.7 duration", e.duration);
        running_status(&mut o, e.running_status);
        if !has(&e.descriptors, 0x4D) {
            let mut present: Vec<&str> = Vec::new();
            for d in &e.descriptors {
                if !present.contains(&d.name()) {
                    present.push(d.name());
                }
            }
            o.push(
                "B10 Part 2 §6.1 Table 6-1",
                Kind::MandatoryDescriptor,
                format!(
                    "short_event_descriptor (Mandatory in EIT) absent; event has [{}]",
                    present.join(", ")
                ),
            );
        }
        descriptors(&mut o, "event", &e.descriptors);
    }
}

fn running_status(o: &mut Out<'_>, rs: u8) {
    if rs >= 5 {
        o.push(
            "B10 Part 2 Table 5-6",
            Kind::ReservedValue,
            format!("running_status {rs} reserved for future use"),
        );
    }
}

fn time(o: &mut Out<'_>, clause: &'static str, field: &str, t: MjdTime, undefined_ok: bool) {
    if t.is_undefined() {
        if !undefined_ok {
            o.push(
                clause,
                Kind::Range,
                format!("{field} is all '1' (undefined)"),
            );
        }
        return;
    }
    if t.to_jst().is_none() {
        o.push(
            clause,
            Kind::Range,
            format!("{field} {:#012x}: invalid MJD/BCD date-time", t.to_u40()),
        );
    }
}

fn duration(o: &mut Out<'_>, clause: &'static str, d: BcdDuration) {
    if !d.is_undefined() && d.hms().is_none() {
        o.push(
            clause,
            Kind::Range,
            format!("duration {:#08x}: invalid BCD hhmmss", d.0),
        );
    }
}

fn offset(o: &mut Out<'_>, field: &str, v: BcdOffset) {
    match v.hm() {
        Some((h, m)) if h <= 12 && m < 60 => {}
        _ => o.push(
            "B10 Part 2 §6.2.25",
            Kind::Range,
            format!("{field} {:#06x}: not a BCD offset within ±12 h", v.0),
        ),
    }
}

/// Check the descriptors of one loop on their own (`location` labels the
/// findings).
pub fn check_descriptors(location: &str, ds: &[Descriptor]) -> Vec<Deviation> {
    let mut v = Vec::new();
    descriptors(
        &mut Out {
            v: &mut v,
            loc: location.to_string(),
        },
        "descriptors",
        ds,
    );
    v
}

fn descriptors(o: &mut Out<'_>, loop_name: &str, ds: &[Descriptor]) {
    for (i, d) in ds.iter().enumerate() {
        let mut o = o.at(format_args!("{loop_name}[{i}] {}", d.name()));
        descriptor(&mut o, d);
    }
}

fn descriptor(o: &mut Out<'_>, d: &Descriptor) {
    match d {
        Descriptor::ConditionalAccess(x) => o.ones(
            "H.222.0 §2.6.16 Table 2-58",
            "reserved",
            x.reserved as u64,
            3,
        ),
        Descriptor::CountryAvailability(x) => o.ones(
            "B10 Part 2 §6.2.5 Table 6-7",
            "reserved_future_use",
            x.reserved_future_use as u64,
            7,
        ),
        Descriptor::Linkage(x) => {
            if x.linkage_type == 0x04 && x.service_id != 0 {
                o.push(
                    "B10 Part 2 §6.2.8",
                    Kind::Range,
                    format!(
                        "linkage_type 0x04 requires service_id 0x0000, found {:#06x}",
                        x.service_id
                    ),
                );
            }
        }
        Descriptor::ExtendedEvent(x) => {
            if x.descriptor_number > x.last_descriptor_number {
                o.push(
                    "B10 Part 2 §6.2.7",
                    Kind::Range,
                    format!(
                        "descriptor_number {} > last_descriptor_number {}",
                        x.descriptor_number, x.last_descriptor_number
                    ),
                );
            }
        }
        Descriptor::Component(x) => o.ones(
            "B10 Part 2 §6.2.3 Table 6-4",
            "reserved_future_use",
            x.reserved_future_use as u64,
            4,
        ),
        Descriptor::LocalTimeOffset(x) => {
            for e in &x.entries {
                o.ones(
                    "B10 Part 2 §6.2.25 Table 6-41",
                    "reserved",
                    e.reserved as u64,
                    1,
                );
                offset(o, "local_time_offset", e.local_time_offset);
                offset(o, "next_time_offset", e.next_time_offset);
                time(
                    o,
                    "B10 Part 2 §6.2.25 time_of_change",
                    "time_of_change",
                    e.time_of_change,
                    false,
                );
            }
        }
        Descriptor::HierarchicalTransmission(x) => {
            o.ones(
                "B10 Part 2 §6.2.22 Table 6-37",
                "reserved_future_use",
                x.reserved_future_use_1 as u64,
                7,
            );
            o.ones(
                "B10 Part 2 §6.2.22 Table 6-37",
                "reserved_future_use",
                x.reserved_future_use_2 as u64,
                3,
            );
        }
        Descriptor::DigitalCopyControl(x) => {
            for c in x.component_control.iter().flatten() {
                o.ones(
                    "B10 Part 2 §6.2.23 Table 6-38",
                    "reserved_future_use",
                    c.reserved_future_use as u64,
                    1,
                );
            }
        }
        Descriptor::AudioComponent(x) => {
            o.ones(
                "B10 Part 2 §6.2.26 Table 6-42",
                "reserved_future_use",
                x.reserved_future_use_1 as u64,
                4,
            );
            o.ones(
                "B10 Part 2 §6.2.26 Table 6-42",
                "reserved_future_use",
                x.reserved_future_use_2 as u64,
                1,
            );
            if x.quality_indicator == 0 {
                o.push(
                    "B10 Part 2 Table 6-44",
                    Kind::ReservedValue,
                    "quality_indicator '00' reserved for future use",
                );
            }
            if x.sampling_rate == 0b000 || x.sampling_rate == 0b100 {
                o.push(
                    "B10 Part 2 Table 6-45",
                    Kind::ReservedValue,
                    format!("sampling_rate {:03b} reserved", x.sampling_rate),
                );
            }
        }
        Descriptor::TsInformation(x) => {
            if x.reserved_future_use.iter().any(|&b| b != 0xFF) {
                o.push(
                    "B10 Part 2 §6.2.42 Table 6-82",
                    Kind::ReservedBits,
                    "trailing reserved_future_use bytes not all 0xFF",
                );
            }
        }
        Descriptor::ExtendedBroadcaster(x) => {
            o.ones(
                "B10 Part 2 §6.2.43 Table 6-83",
                "reserved_future_use",
                x.reserved_future_use as u64,
                4,
            );
            if let ExtendedBroadcasterBody::Other {
                broadcaster_type,
                data,
            } = &x.body
            {
                o.push(
                    "B10 Part 2 Table 6-84",
                    Kind::ReservedValue,
                    format!("broadcaster_type {broadcaster_type:#x} not defined"),
                );
                if data.iter().any(|&b| b != 0xFF) {
                    o.push(
                        "B10 Part 2 §6.2.43 Table 6-83",
                        Kind::ReservedBits,
                        "reserved_future_use bytes not all 0xFF",
                    );
                }
            }
        }
        Descriptor::LogoTransmission(x) => match &x.body {
            LogoTransmissionBody::Cdt1 {
                reserved_future_use_1,
                reserved_future_use_2,
                ..
            } => {
                o.ones(
                    "B10 Part 2 §6.2.44 Table 6-85",
                    "reserved_future_use",
                    *reserved_future_use_1 as u64,
                    7,
                );
                o.ones(
                    "B10 Part 2 §6.2.44 Table 6-85",
                    "reserved_future_use",
                    *reserved_future_use_2 as u64,
                    4,
                );
            }
            LogoTransmissionBody::Cdt2 {
                reserved_future_use,
                ..
            } => o.ones(
                "B10 Part 2 §6.2.44 Table 6-85",
                "reserved_future_use",
                *reserved_future_use as u64,
                7,
            ),
            LogoTransmissionBody::Simple(_) => {}
            LogoTransmissionBody::Other {
                logo_transmission_type,
                data,
            } => {
                o.push(
                    "B10 Part 2 Table 6-86",
                    Kind::ReservedValue,
                    format!("logo_transmission_type {logo_transmission_type:#04x} reserved"),
                );
                if data.iter().any(|&b| b != 0xFF) {
                    o.push(
                        "B10 Part 2 §6.2.44 Table 6-85",
                        Kind::ReservedBits,
                        "reserved_future_use bytes not all 0xFF",
                    );
                }
            }
        },
        Descriptor::LdtLinkage(x) => {
            for e in &x.descriptions {
                o.ones(
                    "B10 Part 2 §6.2.40 Table 6-77",
                    "reserved_future_use",
                    e.reserved_future_use as u64,
                    4,
                );
            }
        }
        Descriptor::ContentAvailability(x) => {
            o.ones(
                "B10 Part 2 §6.2.45 Table 6-87",
                "reserved_future_use",
                x.reserved_future_use as u64,
                1,
            );
            if x.reserved_future_use_bytes.iter().any(|&b| b != 0xFF) {
                o.push(
                    "B10 Part 2 §6.2.45 Table 6-87",
                    Kind::ReservedBits,
                    "trailing reserved_future_use bytes not all 0xFF",
                );
            }
        }
        Descriptor::TerrestrialDeliverySystem(x) => {
            if x.transmission_mode == 0b11 {
                o.push(
                    "B10 Part 2 Table 6-63",
                    Kind::ReservedValue,
                    "transmission_mode '11' undefined",
                );
            }
        }
        Descriptor::EmergencyInformation(x) => {
            for s in &x.services {
                o.ones(
                    "B10 Part 2 §6.2.24 Table 6-40",
                    "reserved_future_use",
                    s.reserved_future_use as u64,
                    6,
                );
                for a in &s.area_codes {
                    o.ones(
                        "B10 Part 2 §6.2.24 Table 6-40",
                        "reserved_future_use",
                        a.reserved_future_use as u64,
                        4,
                    );
                }
            }
        }
        Descriptor::Service(x) if matches!(x.service_type, 0x00 | 0x03..=0x7F | 0xAE..=0xBF | 0xC3..=0xFF) =>
        {
            o.push(
                "B10 Part 2 Table 6-25",
                Kind::ReservedValue,
                format!("service_type {:#04x} not defined", x.service_type),
            );
        }
        _ => {}
    }
}
