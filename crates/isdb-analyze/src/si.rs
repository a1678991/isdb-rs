//! Human-readable summary of the decoded SI of a capture: network and TS
//! names, delivery parameters, services, broadcaster information, EIT
//! present/following events and time (ARIB STD-B10 Part 2 §5.2, §6.2).
//! Every string is decoded with `isdb-text`.

use crate::names;
use crate::scan::Capture;
use isdb_si::AnyTable;
use isdb_si::descriptor::{Descriptor, ExtendedBroadcasterBody};
use isdb_si::time::{BcdDuration, MjdTime};
use isdb_ts::Pid;
use serde::Serialize;

fn text(s: &isdb_text::AribString) -> String {
    s.to_string_lossy()
}

/// Render an MJD+BCD JST time (B10 Annex C).
pub fn time_str(t: MjdTime) -> String {
    if t.is_undefined() {
        return "undefined".into();
    }
    t.to_jst()
        .map(|d| format!("{d} JST"))
        .unwrap_or_else(|| format!("invalid({:010x})", t.to_u40()))
}

/// Render a BCD duration.
pub fn duration_str(d: BcdDuration) -> String {
    match d.hms() {
        _ if d.is_undefined() => "undefined".into(),
        Some((h, m, s)) => format!("{h:02}:{m:02}:{s:02}"),
        None => format!("invalid({:06x})", d.0),
    }
}

/// terrestrial_delivery_system_descriptor (B10 §6.2.31).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Delivery {
    /// area_code (12 bits).
    pub area_code: u16,
    /// guard_interval code (Table 6-62).
    pub guard_interval: u8,
    /// Guard interval text.
    pub guard_interval_name: &'static str,
    /// transmission_mode code (Table 6-63).
    pub transmission_mode: u8,
    /// Mode text.
    pub transmission_mode_name: &'static str,
    /// frequency fields (units of 1/7 MHz).
    pub frequencies: Vec<u16>,
    /// Frequencies in MHz.
    pub frequencies_mhz: Vec<f64>,
}

/// TS_information_descriptor (B10 §6.2.42).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TsInfo {
    /// remote_control_key_id.
    pub remote_control_key_id: u8,
    /// ts_name (decoded).
    pub ts_name: String,
    /// (transmission_type_info, service_ids).
    pub transmission_types: Vec<(u8, Vec<u16>)>,
}

/// One TS of the NIT transport stream loop.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NitTs {
    /// transport_stream_id.
    pub transport_stream_id: u16,
    /// original_network_id.
    pub original_network_id: u16,
    /// Delivery descriptor.
    pub delivery: Option<Delivery>,
    /// TS information.
    pub ts_info: Option<TsInfo>,
    /// partial_reception_descriptor service_ids (B10 §6.2.32).
    pub partial_reception: Option<Vec<u16>>,
    /// service_list_descriptor (service_id, service_type) (B10 §6.2.14).
    pub service_list: Option<Vec<(u16, u8)>>,
    /// Other descriptor names.
    pub other_descriptors: Vec<String>,
}

/// NIT summary.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Network {
    /// table_id (0x40/0x41).
    pub table_id: u8,
    /// network_id.
    pub network_id: u16,
    /// version_number.
    pub version: u8,
    /// network_name_descriptor (decoded).
    pub name: Option<String>,
    /// system_management_id of system_management_descriptor (B10 §6.2.21).
    pub system_management_id: Option<u16>,
    /// Other network-loop descriptor names.
    pub other_descriptors: Vec<String>,
    /// TS loop.
    pub transport_streams: Vec<NitTs>,
}

/// SDT service entry.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ServiceInfo {
    /// table_id (0x42/0x46).
    pub table_id: u8,
    /// transport_stream_id (table_id_extension).
    pub transport_stream_id: u16,
    /// original_network_id.
    pub original_network_id: u16,
    /// service_id.
    pub service_id: u16,
    /// service_type (service_descriptor).
    pub service_type: Option<u8>,
    /// provider_name.
    pub provider: Option<String>,
    /// service_name.
    pub name: Option<String>,
    /// EIT_schedule_flag.
    pub eit_schedule: bool,
    /// EIT_present_following_flag.
    pub eit_present_following: bool,
    /// running_status text.
    pub running_status: &'static str,
    /// free_CA_mode.
    pub free_ca_mode: bool,
    /// Other descriptor names.
    pub other_descriptors: Vec<String>,
}

/// BIT broadcaster.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Broadcaster {
    /// broadcaster_id.
    pub broadcaster_id: u8,
    /// broadcaster_name_descriptor.
    pub name: Option<String>,
    /// extended_broadcaster_descriptor summary.
    pub extended: Option<String>,
    /// SI_parameter_descriptor: (table_id, description hex).
    pub si_parameter: Vec<(u8, String)>,
    /// Other descriptor names.
    pub other_descriptors: Vec<String>,
}

/// BIT summary.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BitInfo {
    /// original_network_id.
    pub original_network_id: u16,
    /// version.
    pub version: u8,
    /// broadcast_view_propriety.
    pub broadcast_view_propriety: bool,
    /// First-loop descriptor names.
    pub first_descriptors: Vec<String>,
    /// SI_parameter_descriptor of the first loop.
    pub si_parameter: Vec<(u8, String)>,
    /// Broadcasters.
    pub broadcasters: Vec<Broadcaster>,
}

/// EIT event.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EventInfo {
    /// table_id.
    pub table_id: u8,
    /// service_id.
    pub service_id: u16,
    /// section_number.
    pub section_number: u8,
    /// event_id.
    pub event_id: u16,
    /// start_time (JST).
    pub start: String,
    /// duration.
    pub duration: String,
    /// running_status text.
    pub running_status: &'static str,
    /// short_event event_name.
    pub title: Option<String>,
    /// short_event text.
    pub text: Option<String>,
    /// Descriptor names.
    pub descriptors: Vec<String>,
}

/// EIT schedule sub_table summary.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScheduleInfo {
    /// table_id.
    pub table_id: u8,
    /// service_id.
    pub service_id: u16,
    /// Sections seen.
    pub sections: usize,
    /// Events.
    pub events: usize,
    /// Earliest start.
    pub first_start: Option<String>,
    /// Latest start.
    pub last_start: Option<String>,
}

/// TOT/TDT.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TimeInfo {
    /// Latest TOT JST_time.
    pub tot: Option<String>,
    /// Latest TDT JST_time.
    pub tdt: Option<String>,
    /// local_time_offset entries: country, region, polarity, offset, change, next.
    pub local_time_offsets: Vec<String>,
}

/// Decoded SI summary.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct SiSummary {
    /// PAT transport_stream_id.
    pub transport_stream_id: Option<u16>,
    /// PAT network_PID.
    pub network_pid: Option<Pid>,
    /// PAT programs (program_number, PMT PID).
    pub programs: Vec<(u16, Pid)>,
    /// NITs (actual first).
    pub networks: Vec<Network>,
    /// SDT services.
    pub services: Vec<ServiceInfo>,
    /// BITs.
    pub bits: Vec<BitInfo>,
    /// EIT p/f events (actual and other).
    pub present_following: Vec<EventInfo>,
    /// EIT schedule sub_tables.
    pub schedules: Vec<ScheduleInfo>,
    /// Time.
    pub time: Option<TimeInfo>,
}

fn delivery(d: &isdb_si::descriptor::TerrestrialDeliverySystem) -> Delivery {
    Delivery {
        area_code: d.area_code,
        guard_interval: d.guard_interval,
        guard_interval_name: names::guard_interval_name(d.guard_interval),
        transmission_mode: d.transmission_mode,
        transmission_mode_name: names::transmission_mode_name(d.transmission_mode),
        frequencies: d.frequencies.clone(),
        frequencies_mhz: d.frequencies.iter().map(|&f| f64::from(f) / 7.0).collect(),
    }
}

fn si_param(ts: &[isdb_si::descriptor::TableDescription]) -> Vec<(u8, String)> {
    ts.iter()
        .map(|t| (t.table_id, crate::tree::hex(&t.description)))
        .collect()
}

/// Build the summary from the latest version of every SI sub_table.
pub fn summarize(cap: &Capture) -> SiSummary {
    let mut s = SiSummary::default();
    if let Some(p) = cap.pat() {
        s.transport_stream_id = Some(p.header.table_id_extension);
        s.network_pid = p.body.network_pid();
        s.programs = p.body.programs().collect();
    }
    let mut tot = None;
    let mut tdt = None;
    let mut lto = Vec::new();
    for (k, slot) in &cap.slots {
        let Ok(t) = AnyTable::decode(k.pid, &slot.latest) else {
            continue;
        };
        match t {
            AnyTable::Nit(n) => {
                let ext = n.header.table_id_extension;
                let net = match s
                    .networks
                    .iter_mut()
                    .position(|x| x.network_id == ext && x.table_id == n.header.table_id)
                {
                    Some(i) => &mut s.networks[i],
                    None => {
                        s.networks.push(Network {
                            table_id: n.header.table_id,
                            network_id: ext,
                            version: n.header.version_number,
                            name: None,
                            system_management_id: None,
                            other_descriptors: vec![],
                            transport_streams: vec![],
                        });
                        s.networks.last_mut().unwrap()
                    }
                };
                for d in &n.body.0.descriptors {
                    match d {
                        Descriptor::NetworkName(x) => net.name = Some(text(&x.name)),
                        Descriptor::SystemManagement(x) => {
                            net.system_management_id = Some(x.system_management_id)
                        }
                        o => net
                            .other_descriptors
                            .push(format!("{} ({:#04x})", o.name(), o.tag())),
                    }
                }
                for ts in &n.body.0.transport_streams {
                    let mut e = NitTs {
                        transport_stream_id: ts.transport_stream_id,
                        original_network_id: ts.original_network_id,
                        delivery: None,
                        ts_info: None,
                        partial_reception: None,
                        service_list: None,
                        other_descriptors: vec![],
                    };
                    for d in &ts.descriptors {
                        match d {
                            Descriptor::TerrestrialDeliverySystem(x) => {
                                e.delivery = Some(delivery(x))
                            }
                            Descriptor::TsInformation(x) => {
                                e.ts_info = Some(TsInfo {
                                    remote_control_key_id: x.remote_control_key_id,
                                    ts_name: text(&x.ts_name),
                                    transmission_types: x
                                        .transmission_types
                                        .iter()
                                        .map(|t| (t.transmission_type_info, t.service_ids.clone()))
                                        .collect(),
                                })
                            }
                            Descriptor::PartialReception(x) => {
                                e.partial_reception = Some(x.service_ids.clone())
                            }
                            Descriptor::ServiceList(x) => {
                                e.service_list = Some(
                                    x.services
                                        .iter()
                                        .map(|s| (s.service_id, s.service_type))
                                        .collect(),
                                )
                            }
                            o => {
                                e.other_descriptors
                                    .push(format!("{} ({:#04x})", o.name(), o.tag()))
                            }
                        }
                    }
                    net.transport_streams
                        .retain(|x| x.transport_stream_id != e.transport_stream_id);
                    net.transport_streams.push(e);
                }
            }
            AnyTable::Sdt(t) => {
                for sv in &t.body.services {
                    let mut i = ServiceInfo {
                        table_id: t.header.table_id,
                        transport_stream_id: t.header.table_id_extension,
                        original_network_id: t.body.original_network_id,
                        service_id: sv.service_id,
                        service_type: None,
                        provider: None,
                        name: None,
                        eit_schedule: sv.eit_schedule_flag,
                        eit_present_following: sv.eit_present_following_flag,
                        running_status: names::running_status_name(sv.running_status),
                        free_ca_mode: sv.free_ca_mode,
                        other_descriptors: vec![],
                    };
                    for d in &sv.descriptors {
                        match d {
                            Descriptor::Service(x) => {
                                i.service_type = Some(x.service_type);
                                i.provider = Some(text(&x.provider_name));
                                i.name = Some(text(&x.service_name));
                            }
                            o => {
                                i.other_descriptors
                                    .push(format!("{} ({:#04x})", o.name(), o.tag()))
                            }
                        }
                    }
                    s.services.retain(|x| {
                        !(x.service_id == i.service_id
                            && x.transport_stream_id == i.transport_stream_id
                            && x.table_id == i.table_id)
                    });
                    s.services.push(i);
                }
            }
            AnyTable::Bit(b) => {
                let mut bi = BitInfo {
                    original_network_id: b.header.table_id_extension,
                    version: b.header.version_number,
                    broadcast_view_propriety: b.body.broadcast_view_propriety,
                    first_descriptors: vec![],
                    si_parameter: vec![],
                    broadcasters: vec![],
                };
                for d in &b.body.first_descriptors {
                    if let Descriptor::SiParameter(x) = d {
                        bi.si_parameter = si_param(&x.tables);
                    }
                    bi.first_descriptors
                        .push(format!("{} ({:#04x})", d.name(), d.tag()));
                }
                for br in &b.body.broadcasters {
                    let mut e = Broadcaster {
                        broadcaster_id: br.broadcaster_id,
                        name: None,
                        extended: None,
                        si_parameter: vec![],
                        other_descriptors: vec![],
                    };
                    for d in &br.descriptors {
                        match d {
                            Descriptor::BroadcasterName(x) => e.name = Some(text(&x.name)),
                            Descriptor::ExtendedBroadcaster(x) => {
                                e.extended = Some(match &x.body {
                                    ExtendedBroadcasterBody::TerrestrialTv(t)
                                    | ExtendedBroadcasterBody::TerrestrialSound(t) => format!(
                                        "type {:#x} terrestrial_broadcaster_id {:#06x} affiliations {:02x?} broadcasters {:?}",
                                        x.broadcaster_type(),
                                        t.terrestrial_broadcaster_id,
                                        t.affiliation_ids,
                                        t.broadcasters
                                            .iter()
                                            .map(|n| format!(
                                                "{:#06x}/{:#04x}",
                                                n.original_network_id, n.broadcaster_id
                                            ))
                                            .collect::<Vec<_>>()
                                    ),
                                    ExtendedBroadcasterBody::Other {
                                        broadcaster_type, ..
                                    } => format!("type {broadcaster_type:#x}"),
                                })
                            }
                            Descriptor::SiParameter(x) => e.si_parameter = si_param(&x.tables),
                            o => {
                                e.other_descriptors
                                    .push(format!("{} ({:#04x})", o.name(), o.tag()))
                            }
                        }
                    }
                    bi.broadcasters.push(e);
                }
                s.bits
                    .retain(|x| x.original_network_id != bi.original_network_id);
                s.bits.push(bi);
            }
            AnyTable::Eit(e) if matches!(e.header.table_id, 0x4E | 0x4F) => {
                for ev in &e.body.events {
                    let mut title = None;
                    let mut txt = None;
                    for d in &ev.descriptors {
                        if let Descriptor::ShortEvent(x) = d {
                            title = Some(text(&x.event_name));
                            txt = Some(text(&x.text));
                        }
                    }
                    s.present_following.push(EventInfo {
                        table_id: e.header.table_id,
                        service_id: e.header.table_id_extension,
                        section_number: e.header.section_number,
                        event_id: ev.event_id,
                        start: time_str(ev.start_time),
                        duration: duration_str(ev.duration),
                        running_status: names::running_status_name(ev.running_status),
                        title,
                        text: txt,
                        descriptors: ev
                            .descriptors
                            .iter()
                            .map(|d| format!("{} ({:#04x})", d.name(), d.tag()))
                            .collect(),
                    });
                }
            }
            AnyTable::Eit(e) => {
                let key = (e.header.table_id, e.header.table_id_extension);
                let i = match s
                    .schedules
                    .iter()
                    .position(|x| (x.table_id, x.service_id) == key)
                {
                    Some(i) => i,
                    None => {
                        s.schedules.push(ScheduleInfo {
                            table_id: key.0,
                            service_id: key.1,
                            sections: 0,
                            events: 0,
                            first_start: None,
                            last_start: None,
                        });
                        s.schedules.len() - 1
                    }
                };
                let sc = &mut s.schedules[i];
                sc.sections += 1;
                sc.events += e.body.events.len();
                for ev in &e.body.events {
                    if ev.start_time.is_undefined() {
                        continue;
                    }
                    let t = time_str(ev.start_time);
                    if sc.first_start.as_ref().is_none_or(|f| t < *f) {
                        sc.first_start = Some(t.clone());
                    }
                    if sc.last_start.as_ref().is_none_or(|f| t > *f) {
                        sc.last_start = Some(t);
                    }
                }
            }
            AnyTable::Tot(t) => {
                tot = Some(time_str(t.body.jst_time));
                lto.clear();
                for d in &t.body.descriptors {
                    if let Descriptor::LocalTimeOffset(x) = d {
                        for e in &x.entries {
                            lto.push(format!(
                                "{} region {} {}{:04x} change {} next {:04x}",
                                String::from_utf8_lossy(&e.country_code),
                                e.country_region_id,
                                if e.local_time_offset_polarity {
                                    "-"
                                } else {
                                    "+"
                                },
                                e.local_time_offset.0,
                                time_str(e.time_of_change),
                                e.next_time_offset.0
                            ));
                        }
                    }
                }
            }
            AnyTable::Tdt(t) => tdt = Some(time_str(t.body.jst_time)),
            _ => {}
        }
    }
    s.networks.sort_by_key(|n| (n.table_id, n.network_id));
    s.present_following
        .sort_by_key(|e| (e.table_id, e.service_id, e.section_number));
    if tot.is_some() || tdt.is_some() {
        s.time = Some(TimeInfo {
            tot,
            tdt,
            local_time_offsets: lto,
        });
    }
    s
}
