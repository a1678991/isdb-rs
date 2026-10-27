//! Load-time configuration checks: typed signalling options, and raw
//! descriptors that would duplicate a typed one in the same loop.

use crate::config::{
    AddDescriptor, AdtsCompliance, AtEof, AudioQualityIndicator, Config, MediaClass, PidMap,
    RateCheck, RawDescriptor, StreamCfg, StreamKind, parse_hex,
};
use crate::error::{MuxError, Result};
use crate::si::{Target, parse_target};
use crate::signalling::{ComponentTypeCfg, VdcMode};
use isdb_si::values::CodedValue;
use isdb_ts::Pid;

/// A descriptor the typed configuration generates in a loop.
struct Typed {
    tag: u8,
    /// component_tag for component / audio component descriptors (one per
    /// component is legitimate; only the same component_tag collides).
    component_tag: Option<u8>,
    name: &'static str,
    /// How to switch the typed one off, for the error message.
    off: String,
}

/// A raw descriptor configured for a loop.
struct Raw {
    tag: u8,
    body: Vec<u8>,
    source: String,
}

fn typed(tag: u8, name: &'static str, off: impl Into<String>) -> Typed {
    Typed {
        tag,
        component_tag: None,
        name,
        off: off.into(),
    }
}

fn raws(list: &[RawDescriptor], source: &str) -> Result<Vec<Raw>> {
    list.iter()
        .map(|d| {
            Ok(Raw {
                tag: d.tag,
                body: parse_hex(&d.hex)?,
                source: source.to_string(),
            })
        })
        .collect()
}

fn adds(list: &[AddDescriptor], source: &str, pred: impl Fn(&Target) -> bool) -> Result<Vec<Raw>> {
    let mut v = Vec::new();
    for a in list {
        if parse_target(&a.target).is_some_and(|t| pred(&t)) {
            v.push(Raw {
                tag: a.tag,
                body: parse_hex(&a.hex)?,
                source: format!("{source} add_descriptors loop {:?}", a.target),
            });
        }
    }
    Ok(v)
}

fn check_loop(where_: &str, typed: &[Typed], raw: &[Raw]) -> Result<()> {
    for r in raw {
        for t in typed {
            let same_component = match t.component_tag {
                None => true,
                // component_tag is the 3rd body byte of 0x50 and 0xC4
                Some(c) => r.body.get(2).is_none_or(|&b| b == c),
            };
            if r.tag == t.tag && same_component {
                return Err(MuxError::Config(format!(
                    "{where_}: raw descriptor tag {:#04x} ({}) duplicates the typed {}; {} to send the raw one, \
                     or drop the raw one",
                    r.tag, r.source, t.name, t.off
                )));
            }
        }
    }
    Ok(())
}

/// The `ts-program` keys are only meaningful on `kind = "ts-program"`; a
/// `ts-program` entry takes its PIDs, stream types, component tags and ES
/// descriptors from the source PMT.
fn check_ts_program_keys(st: &StreamCfg, what: &str) -> Result<()> {
    let relay_keys_set = st.program.is_some()
        || st.service_name.is_some()
        || st.stream_types.is_some()
        || st.component_tags.is_some()
        || st.include_carousels
        || !st.pid_map.is_auto()
        || st.pids.is_some()
        || st.at_eof != AtEof::Loop
        || st.eit_from_source
        || st.rate_check != RateCheck::Error
        || st.max_early_ms != 0
        || st.probe_seconds.is_some()
        || !st.ca.is_keep();
    if st.kind != StreamKind::TsProgram {
        if relay_keys_set {
            return Err(MuxError::Config(format!(
                "{what} {:#06x}: program / service_name / stream_types / component_tags / include_carousels / pid_map / pids / \
                 at_eof / eit_from_source / rate_check / max_early_ms / probe_seconds / ca apply only to kind = \"ts-program\"",
                st.pid
            )));
        }
        return Ok(());
    }
    if st.relay_es.is_some() {
        return Ok(()); // expanded by the multiplexer
    }
    let fail = |m: &str| Err(MuxError::Config(format!("{what} (ts-program): {m}")));
    if st.source.is_none() {
        return fail("needs source = the recorded TS file");
    }
    if st.pid != Pid::PAT || st.stream_type.is_some() || st.component_tag.is_some() {
        return fail(
            "pid / stream_type / component_tag come from the source PMT; output PIDs are assigned by role (pid_map = \"auto\", pids = { video = …, audio = […], captions = …, superimpose = … }) or listed (pid_map = [[source, output], …])",
        );
    }
    if !st.descriptors.is_empty() || st.hierarchical.is_some() {
        return fail(
            "ES descriptors are carried over from the source PMT (stream_identifier, video_decode_control, data_component)",
        );
    }
    if st.video_decode_control.mode != VdcMode::Derived
        || st.component_type != ComponentTypeCfg::Derived
    {
        return fail(
            "video_decode_control / component_type are taken from the source PMT or derived from the relayed streams",
        );
    }
    if let Some(p) = st.probe_seconds
        && !(1.0..=600.0).contains(&p)
    {
        return fail("probe_seconds must be 1..600");
    }
    if st.program.is_some() && st.service_name.is_some() {
        return fail(
            "program (service_id) and service_name both select the source program; give one",
        );
    }
    if let PidMap::List(list) = &st.pid_map {
        for &[a, b] in list {
            if !b.is_assignable() || a == Pid::NULL {
                return fail(&format!(
                    "pid_map [{a:#06x}, {b:#06x}]: output PID outside 0x0010–0x1FFE (H.222.0 Table 2-3)"
                ));
            }
        }
        for (i, x) in list.iter().enumerate() {
            if list[..i].iter().any(|y| y[0] == x[0]) {
                return fail(&format!("pid_map lists source PID {:#06x} twice", x[0]));
            }
        }
    }
    if let Some(plan) = &st.pids {
        if !st.pid_map.is_auto() {
            return fail("pids (the role PID plan) applies only to pid_map = \"auto\"");
        }
        for (role, l) in [
            ("video", &plan.video),
            ("audio", &plan.audio),
            ("captions", &plan.captions),
            ("superimpose", &plan.superimpose),
            ("data", &plan.data),
        ] {
            if l.0.is_empty() {
                return fail(&format!("pids.{role}: give at least one PID"));
            }
            if let Some(p) = l.0.iter().find(|p| !p.is_assignable()) {
                return fail(&format!(
                    "pids.{role}: PID {p:#06x} outside 0x0010–0x1FFE (H.222.0 Table 2-3)"
                ));
            }
        }
        let all: Vec<Pid> = plan.all().collect();
        if let Some(p) = (0..all.len()).find_map(|i| all[..i].contains(&all[i]).then_some(all[i])) {
            return fail(&format!(
                "pids: PID {p:#06x} is given to two roles (or twice)"
            ));
        }
    }
    Ok(())
}

impl Config {
    /// Check the typed signalling options and reject raw descriptors that
    /// duplicate an active typed descriptor in the same loop (same tag; for
    /// component descriptors also the same component_tag).  Called by
    /// [`Config::from_toml`] and [`crate::Multiplexer::new`].
    pub fn validate(&self) -> Result<()> {
        // ---- NIT
        let n = &self.network;
        let mut net = vec![typed(
            0x40,
            "network_name_descriptor",
            "it is always generated; use [nit] byte_patches/replace_sections",
        )];
        if n.system_management {
            net.push(typed(
                0xFE,
                "system_management_descriptor",
                "set network.system_management = false",
            ));
        }
        check_loop(
            "NIT network loop",
            &net,
            &adds(&self.nit.add_descriptors, "[nit]", |t| {
                *t == Target::Network
            })?,
        )?;
        let always = "it is always generated; use [nit] byte_patches/replace_sections";
        let mut ts = vec![
            typed(0xFA, "terrestrial_delivery_system_descriptor", always),
            typed(
                0xCD,
                "TS_information_descriptor (remote_control_key_id)",
                always,
            ),
            typed(0x41, "service_list_descriptor", always),
        ];
        if n.partial_reception_descriptor
            && self
                .services
                .iter()
                .any(|s| s.layer == crate::config::LayerName::A)
        {
            ts.push(typed(
                0xFB,
                "partial_reception_descriptor",
                "set network.partial_reception_descriptor = false",
            ));
        }
        check_loop(
            "NIT TS loop",
            &ts,
            &adds(&self.nit.add_descriptors, "[nit]", |t| *t == Target::Ts)?,
        )?;

        for s in &self.services {
            let sid = s.service_id;
            let svc = format!("service {sid:#06x}");
            if let Some(cc) = &s.copy_control {
                cc.check(&format!("{svc} copy_control"))?;
                if cc.recording_control == crate::signalling::RecordingControlCfg::Source
                    && !s
                        .streams
                        .iter()
                        .any(|st| st.kind == StreamKind::TsProgram || st.relay_es.is_some())
                {
                    return Err(MuxError::Config(format!(
                        "{svc} copy_control: recording_control = \"source\" takes the value of a relayed \
                         source program; the service has no ts-program stream (give copy-free, \
                         copy-one-generation, copy-never or provider-defined)"
                    )));
                }
            }
            // ---- PMT program loop
            let mut prog = Vec::new();
            if let Some(cc) = &s.copy_control
                && cc.in_pmt
            {
                prog.push(typed(
                    0xC1,
                    "digital_copy_control_descriptor (copy_control)",
                    "set copy_control.in_pmt = false",
                ));
            }
            if let Some(ca) = &s.content_availability
                && ca.in_pmt
            {
                prog.push(typed(
                    0xDE,
                    "content_availability_descriptor",
                    "set content_availability.in_pmt = false",
                ));
            }
            let mut raw = raws(&s.program_descriptors, "program_descriptors")?;
            raw.extend(adds(&s.pmt.add_descriptors, "[service.pmt]", |t| {
                *t == Target::Program
            })?);
            check_loop(&format!("{svc} PMT program loop"), &prog, &raw)?;

            // ---- Hybridcast
            if let Some(h) = &s.hybridcast {
                let what = format!("{svc} hybridcast");
                crate::hybridcast::check(h, &what)?;
                let has_carousel = s
                    .streams
                    .iter()
                    .any(|st| st.kind == StreamKind::BmlCarousel);
                match h.carriage {
                    crate::config::AitCarriage::Carousel => {
                        if !has_carousel {
                            return Err(MuxError::Config(format!(
                                "{what}: carriage = \"carousel\" puts the AIT into the service's bml-carousel; add a [[service.stream]] kind = \"bml-carousel\" (or set carriage = \"es\" with pid and data_component_id)"
                            )));
                        }
                        if h.pid.is_some() || h.data_component_id.is_some() {
                            return Err(MuxError::Config(format!(
                                "{what}: pid / data_component_id apply to carriage = \"es\""
                            )));
                        }
                    }
                    crate::config::AitCarriage::Es => {
                        let pid = h.pid.ok_or_else(|| {
                            MuxError::Config(format!("{what}: carriage = \"es\" needs pid"))
                        })?;
                        if !pid.is_assignable() {
                            return Err(MuxError::Config(format!(
                                "{what}: pid {pid:#06x} outside 0x0010–0x1FFE (H.222.0 Table 2-3)"
                            )));
                        }
                        if h.data_component_id.is_none() {
                            return Err(MuxError::Config(format!(
                                "{what}: carriage = \"es\" needs data_component_id (the value of ARIB TR-B14 / B24 Vol. 4 §7.3; B24 v6.4 leaves it \"[T B D]\")"
                            )));
                        }
                        if s.streams.iter().any(|st| st.pid == pid) {
                            return Err(MuxError::Config(format!(
                                "{what}: pid {pid:#06x} is also a stream of the service"
                            )));
                        }
                    }
                }
            }
            // ---- PMT ES loops, typed option sanity
            for st in &s.streams {
                let what = format!("{svc} stream {:#06x}", st.pid);
                check_ts_program_keys(st, &format!("{svc} stream"))?;
                if st.bml.is_some() && st.kind != StreamKind::BmlCarousel {
                    return Err(MuxError::Config(format!(
                        "{what}: bml options apply only to kind = \"bml-carousel\""
                    )));
                }
                if st.kind == StreamKind::BmlCarousel {
                    let bml = st.bml.clone().unwrap_or_default();
                    if bml.block_size == 0 || bml.block_size > isdb_si::dsmcc::MAX_BLOCK_SIZE {
                        return Err(MuxError::Config(format!(
                            "{what}: bml.block_size {} outside 1..={} (a DDB section is at most 4096 bytes, B24 Vol. 3 §6.5)",
                            bml.block_size,
                            isdb_si::dsmcc::MAX_BLOCK_SIZE
                        )));
                    }
                    if bml.data_event_id > 0xF || bml.pmt_data_event_id > 0xF {
                        return Err(MuxError::Config(format!(
                            "{what}: bml.data_event_id / pmt_data_event_id are 4 bits (0..15)"
                        )));
                    }
                    if bml.download_id > 0x0FFF_FFFF {
                        return Err(MuxError::Config(format!(
                            "{what}: bml.download_id is 28 bits (data_event_id takes bits 31–28)"
                        )));
                    }
                    if bml.transaction_number > 0x3FFF_FFFF {
                        return Err(MuxError::Config(format!(
                            "{what}: bml.transaction_number is 30 bits"
                        )));
                    }
                    if bml.reserved_future_use > 0xF {
                        return Err(MuxError::Config(format!(
                            "{what}: bml.reserved_future_use is 4 bits"
                        )));
                    }
                    if bml.document_resolution.name() == "reserved" {
                        return Err(MuxError::Config(format!(
                            "{what}: bml.document_resolution is a reserved code"
                        )));
                    }
                    if bml.cycle_ms == 0 || bml.dii_interval_ms == 0 || bml.bitrate_kbps == 0 {
                        return Err(MuxError::Config(format!(
                            "{what}: bml.cycle_ms, dii_interval_ms and bitrate_kbps must be > 0"
                        )));
                    }
                    crate::carousel::bml_versions(&bml, &what)?;
                    if st.source.is_none()
                        && bml.modules.is_empty()
                        && !s.hybridcast.as_ref().is_some_and(|h| {
                            h.launcher && h.carriage == crate::config::AitCarriage::Carousel
                        })
                    {
                        return Err(MuxError::Config(format!(
                            "{what}: bml-carousel needs source = <directory of BML files> (or a [service.hybridcast] launcher)"
                        )));
                    }
                    if let Some(t) = st.stream_type
                        && !matches!(t, 0x0B | 0x0D)
                    {
                        return Err(MuxError::Config(format!(
                            "{what}: a data carousel has stream_type 0x0B or 0x0D (B24 Vol. 3 Table 4-1), not {t:#04x}"
                        )));
                    }
                }
                st.video_decode_control.check(&what)?;
                if st.kind == StreamKind::AacAdts
                    && st.video_decode_control.mode == VdcMode::Explicit
                {
                    return Err(MuxError::Config(format!(
                        "{what}: video_decode_control on an aac-adts stream (B10 §6.2.30 describes video)"
                    )));
                }
                if st.kind != StreamKind::AacAdts
                    && (st.audio_quality_indicator != AudioQualityIndicator::Derived
                        || st.adts_compliance != AdtsCompliance::Conform)
                {
                    return Err(MuxError::Config(format!(
                        "{what}: audio_quality_indicator / adts_compliance apply only to aac-adts streams"
                    )));
                }
                match (st.kind, st.component_type) {
                    (_, ComponentTypeCfg::Derived)
                    | (StreamKind::Mpeg2Video, ComponentTypeCfg::Video(_))
                    | (StreamKind::AacAdts, ComponentTypeCfg::Audio(_)) => {}
                    // declared streams have no media and get no EIT component
                    // descriptor; their component_type is unused
                    (StreamKind::Declared, _) => {}
                    (StreamKind::BmlCarousel, _) => {
                        return Err(MuxError::Config(format!(
                            "{what}: a bml-carousel has no component_type (its EIT descriptor is the data content descriptor)"
                        )));
                    }
                    (StreamKind::TsProgram, _) => {
                        return Err(MuxError::Config(format!(
                            "{what}: component_type of a ts-program stream is derived from the relayed streams"
                        )));
                    }
                    (StreamKind::Mpeg2Video, _) => {
                        return Err(MuxError::Config(format!(
                            "{what}: mpeg2-video needs a video component_type name (B10 Table 6-5), not an audio table"
                        )));
                    }
                    (StreamKind::AacAdts, _) => {
                        return Err(MuxError::Config(format!(
                            "{what}: aac-adts needs an audio component_type table {{ audio_mode = … }} (B10 Table 6-43)"
                        )));
                    }
                }
                let mut es = Vec::new();
                if st.component_tag.is_some() || st.kind == StreamKind::BmlCarousel {
                    es.push(typed(
                        0x52,
                        "stream_identifier_descriptor (component_tag)",
                        "remove component_tag",
                    ));
                }
                if st.kind == StreamKind::BmlCarousel {
                    es.push(typed(
                        0xFD,
                        "data_component_descriptor (bml)",
                        "it is always generated for a bml-carousel; use [service.pmt] byte_patches/replace_sections",
                    ));
                }
                let vdc_active = st.video_decode_control.mode == VdcMode::Explicit
                    || (st.video_decode_control.mode == VdcMode::Derived
                        && (st.kind == StreamKind::Mpeg2Video
                            || (st.kind == StreamKind::TsProgram
                                && st.media_class() == MediaClass::Video)));
                if vdc_active {
                    es.push(typed(
                        0xC8,
                        "video_decode_control_descriptor",
                        "set video_decode_control = { mode = \"off\" }",
                    ));
                }
                if st.hierarchical.is_some() {
                    es.push(typed(
                        0xC0,
                        "hierarchical_transmission_descriptor",
                        "remove hierarchical",
                    ));
                }
                let mut raw = raws(&st.descriptors, "stream descriptors")?;
                raw.extend(adds(&s.pmt.add_descriptors, "[service.pmt]", |t| {
                    *t == Target::Es(st.pid)
                })?);
                check_loop(&format!("{what} PMT ES loop"), &es, &raw)?;
            }

            // ---- SDT service loop
            let mut sdt = vec![typed(
                0x48,
                "service_descriptor",
                "it is always generated; use [sdt] byte_patches/replace_sections",
            )];
            if let Some(cc) = &s.copy_control
                && cc.in_sdt
            {
                sdt.push(typed(
                    0xC1,
                    "digital_copy_control_descriptor (copy_control)",
                    "set copy_control.in_sdt = false",
                ));
            }
            let mut raw = raws(&s.sdt_descriptors, "sdt_descriptors")?;
            raw.extend(adds(&self.sdt.add_descriptors, "[sdt]", |t| {
                matches!(t, Target::Service(None)) || *t == Target::Service(Some(sid))
            })?);
            check_loop(&format!("{svc} SDT service loop"), &sdt, &raw)?;

            // ---- EIT event loops
            for e in &s.events {
                let mut ev = vec![typed(
                    0x4D,
                    "short_event_descriptor",
                    "it is always generated; use [eit] byte_patches/replace_sections",
                )];
                for st in &s.streams {
                    if !st.eit_component {
                        continue;
                    }
                    let (tag, name) = match st.media_class() {
                        MediaClass::Video => (0x50, "component_descriptor"),
                        MediaClass::Audio => (0xC4, "audio_component_descriptor"),
                        MediaClass::Other => continue,
                    };
                    // an unexpanded ts-program entry has no media of its own
                    if st.kind == StreamKind::TsProgram && st.relay_es.is_none() {
                        continue;
                    }
                    ev.push(Typed {
                        tag,
                        component_tag: Some(st.component_tag.unwrap_or(0)),
                        name,
                        off: format!("set eit_component = false on stream {:#06x}", st.pid),
                    });
                }
                if s.streams.iter().any(|st| {
                    st.kind == StreamKind::BmlCarousel
                        && st.bml.clone().unwrap_or_default().entry_point
                        && st.bml.clone().unwrap_or_default().eit_data_content
                }) {
                    ev.push(typed(
                        0xC7,
                        "data_content_descriptor (bml)",
                        "set bml.eit_data_content = false on the bml-carousel stream",
                    ));
                }
                if !e.content.is_empty() {
                    ev.push(typed(
                        0x54,
                        "content_descriptor (content)",
                        "remove the event's content list",
                    ));
                }
                if let Some(cc) = &s.copy_control
                    && cc.in_eit
                {
                    ev.push(typed(
                        0xC1,
                        "digital_copy_control_descriptor (copy_control)",
                        "set copy_control.in_eit = false",
                    ));
                }
                if let Some(ca) = &s.content_availability
                    && ca.in_eit
                {
                    ev.push(typed(
                        0xDE,
                        "content_availability_descriptor",
                        "set content_availability.in_eit = false",
                    ));
                }
                let eid = e.event_id;
                let pred = |t: &Target| match t {
                    Target::Event(None, None) => true,
                    Target::Event(Some(x), None) => *x == sid,
                    Target::Event(Some(x), Some(y)) => *x == sid && *y == eid,
                    _ => false,
                };
                let mut raw = raws(&e.descriptors, "event descriptors")?;
                raw.extend(adds(&self.eit.add_descriptors, "[eit]", pred)?);
                raw.extend(adds(
                    &self.eit_schedule.add_descriptors,
                    "[eit_schedule]",
                    pred,
                )?);
                check_loop(&format!("{svc} event {eid:#06x} EIT event loop"), &ev, &raw)?;
            }
        }

        // ---- BIT broadcaster loops
        for bc in &self.bit.broadcasters {
            let mut t = Vec::new();
            if bc.name.is_some() {
                t.push(typed(
                    0xD8,
                    "broadcaster_name_descriptor (name)",
                    "remove name",
                ));
            }
            if bc.terrestrial.is_some() {
                t.push(typed(
                    0xCE,
                    "extended_broadcaster_descriptor (terrestrial)",
                    "remove terrestrial",
                ));
            }
            let id = bc.broadcaster_id;
            let mut raw = raws(&bc.descriptors, "broadcaster descriptors")?;
            raw.extend(adds(&self.bit.add_descriptors, "[bit]", |t| {
                matches!(t, Target::Broadcaster(None)) || *t == Target::Broadcaster(Some(id))
            })?);
            check_loop(&format!("BIT broadcaster {id:#04x} loop"), &t, &raw)?;
        }
        // ---- TOT
        if !self.tot.local_time_offset.is_empty() {
            check_loop(
                "TOT descriptor loop",
                &[typed(
                    0x58,
                    "local_time_offset_descriptor",
                    "remove [tot] local_time_offset",
                )],
                &adds(&self.tot.add_descriptors, "[tot]", |t| *t == Target::Main)?,
            )?;
        }
        Ok(())
    }
}
