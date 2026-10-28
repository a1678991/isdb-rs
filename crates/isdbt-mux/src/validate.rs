//! Load-time configuration checks: typed signalling options, and raw
//! descriptors that would duplicate a typed one in the same loop.

use crate::config::{
    AddDescriptor, AdtsCompliance, AtEof, AudioQualityIndicator, Config, MediaClass, PidMap,
    RateCheck, RawDescriptor, StreamCfg, StreamKind, parse_hex,
};
use crate::error::{MuxError, Result, key};
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
    /// TOML path of the raw descriptor.
    key: String,
}

fn typed(tag: u8, name: &'static str, off: impl Into<String>) -> Typed {
    Typed {
        tag,
        component_tag: None,
        name,
        off: off.into(),
    }
}

/// The raw descriptors of `list`, the TOML array at `list_key`.
fn raws(list: &[RawDescriptor], list_key: &str) -> Result<Vec<Raw>> {
    list.iter()
        .enumerate()
        .map(|(i, d)| {
            let key = format!("{list_key}[{i}]");
            Ok(Raw {
                tag: d.tag,
                body: parse_hex(&d.hex, &format!("{key}.hex"))?,
                key,
            })
        })
        .collect()
}

/// The `add_descriptors` of the table at `table_key` that `pred` selects.
fn adds(
    list: &[AddDescriptor],
    table_key: &str,
    pred: impl Fn(&Target) -> bool,
) -> Result<Vec<Raw>> {
    let mut v = Vec::new();
    for (i, a) in list.iter().enumerate() {
        if parse_target(&a.target).is_some_and(|t| pred(&t)) {
            let key = format!("{table_key}.add_descriptors[{i}]");
            v.push(Raw {
                tag: a.tag,
                body: parse_hex(&a.hex, &format!("{key}.hex"))?,
                key,
            });
        }
    }
    Ok(v)
}

/// `loop_name`: the descriptor loop, for the message.
fn check_loop(loop_name: &str, typed: &[Typed], raw: &[Raw]) -> Result<()> {
    for r in raw {
        for t in typed {
            let same_component = match t.component_tag {
                None => true,
                // component_tag is the 3rd body byte of 0x50 and 0xC4
                Some(c) => r.body.get(2).is_none_or(|&b| b == c),
            };
            if r.tag == t.tag && same_component {
                return Err(MuxError::config(
                    r.key.clone(),
                    format!(
                        "raw descriptor tag {:#04x} duplicates the typed {} of the {loop_name}; {} to send the raw one, \
                         or drop the raw one",
                        r.tag, t.name, t.off
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// The first `ts-program` key set on `st`, if any.
fn relay_key_set(st: &StreamCfg) -> Option<&'static str> {
    [
        ("program", st.program.is_some()),
        ("service_name", st.service_name.is_some()),
        ("stream_types", st.stream_types.is_some()),
        ("component_tags", st.component_tags.is_some()),
        ("include_carousels", st.include_carousels),
        ("pid_map", !st.pid_map.is_auto()),
        ("pids", st.pids.is_some()),
        ("at_eof", st.at_eof != AtEof::Loop),
        ("eit_from_source", st.eit_from_source),
        ("rate_check", st.rate_check != RateCheck::Error),
        ("max_early_ms", st.max_early_ms != 0),
        ("probe_seconds", st.probe_seconds.is_some()),
        ("ca", !st.ca.is_keep()),
    ]
    .into_iter()
    .find_map(|(k, set)| set.then_some(k))
}

/// The `ts-program` keys are only meaningful on `kind = "ts-program"`; a
/// `ts-program` entry takes its PIDs, stream types, component tags and ES
/// descriptors from the source PMT. `stk` is the TOML path of the stream.
fn check_ts_program_keys(st: &StreamCfg, stk: &str) -> Result<()> {
    if st.kind != StreamKind::TsProgram {
        if let Some(k) = relay_key_set(st) {
            return Err(MuxError::NotApplicable {
                key: format!("{stk}.{k}"),
                applies_to: "kind = \"ts-program\"".to_string(),
            });
        }
        return Ok(());
    }
    if st.relay_es.is_some() {
        return Ok(()); // expanded by the multiplexer
    }
    let fail = |k: &str, m: &str| Err(MuxError::config(format!("{stk}.{k}"), m));
    if st.source.is_none() {
        return Err(MuxError::Missing {
            key: format!("{stk}.source"),
            needed_by: "kind = \"ts-program\" (the recorded TS file)".to_string(),
        });
    }
    let from_pmt = [
        ("pid", st.pid != Pid::PAT),
        ("stream_type", st.stream_type.is_some()),
        ("component_tag", st.component_tag.is_some()),
    ];
    if let Some((k, _)) = from_pmt.iter().find(|(_, set)| *set) {
        return fail(
            k,
            "a ts-program takes pid / stream_type / component_tag from the source PMT; output PIDs are assigned by role (pid_map = \"auto\", pids = { video = …, audio = […], captions = …, superimpose = … }) or listed (pid_map = [[source, output], …])",
        );
    }
    if !st.descriptors.is_empty() || st.hierarchical.is_some() {
        return fail(
            if st.descriptors.is_empty() {
                "hierarchical"
            } else {
                "descriptors"
            },
            "a ts-program carries the ES descriptors over from the source PMT (stream_identifier, video_decode_control, data_component)",
        );
    }
    if st.video_decode_control.mode != VdcMode::Derived
        || st.component_type != ComponentTypeCfg::Derived
    {
        return fail(
            if st.video_decode_control.mode != VdcMode::Derived {
                "video_decode_control"
            } else {
                "component_type"
            },
            "a ts-program takes video_decode_control / component_type from the source PMT or derives them from the relayed streams",
        );
    }
    if let Some(p) = st.probe_seconds
        && !(1.0..=600.0).contains(&p)
    {
        return fail("probe_seconds", &format!("{p} outside 1..=600 s"));
    }
    if st.program.is_some() && st.service_name.is_some() {
        return Err(MuxError::Conflict {
            key: format!("{stk}.program"),
            other: "service_name (both select the source program)".to_string(),
        });
    }
    if let PidMap::List(list) = &st.pid_map {
        for (i, &[a, b]) in list.iter().enumerate() {
            if !b.is_assignable() {
                return Err(MuxError::PidRange {
                    key: format!("{stk}.pid_map[{i}]"),
                    pid: b,
                });
            }
            if a == Pid::NULL {
                return fail(
                    &format!("pid_map[{i}]"),
                    "source PID 0x1fff is the null packet PID (H.222.0 Table 2-3)",
                );
            }
        }
        for (i, x) in list.iter().enumerate() {
            if list[..i].iter().any(|y| y[0] == x[0]) {
                return Err(MuxError::Duplicate {
                    key: format!("{stk}.pid_map[{i}]"),
                    value: format!("source PID {:#06x}", x[0]),
                });
            }
        }
    }
    if let Some(plan) = &st.pids {
        if !st.pid_map.is_auto() {
            return Err(MuxError::NotApplicable {
                key: format!("{stk}.pids"),
                applies_to: "pid_map = \"auto\" (the role PID plan)".to_string(),
            });
        }
        for (role, l) in [
            ("video", &plan.video),
            ("audio", &plan.audio),
            ("captions", &plan.captions),
            ("superimpose", &plan.superimpose),
            ("data", &plan.data),
        ] {
            if l.0.is_empty() {
                return fail(&format!("pids.{role}"), "give at least one PID");
            }
            if let Some(&p) = l.0.iter().find(|p| !p.is_assignable()) {
                return Err(MuxError::PidRange {
                    key: format!("{stk}.pids.{role}"),
                    pid: p,
                });
            }
        }
        let all: Vec<Pid> = plan.all().collect();
        if let Some(p) = (0..all.len()).find_map(|i| all[..i].contains(&all[i]).then_some(all[i])) {
            return Err(MuxError::Duplicate {
                key: format!("{stk}.pids"),
                value: format!("PID {p:#06x}"),
            });
        }
    }
    Ok(())
}

impl Config {
    /// Check the typed signalling options and reject raw descriptors that
    /// duplicate an active typed descriptor in the same loop (same tag; for
    /// component descriptors also the same component_tag).  Called by
    /// [`Config::from_toml`] and [`crate::Multiplexer::new`].
    ///
    /// # Errors
    ///
    /// The first rule broken, as a configuration variant of [`MuxError`]
    /// naming the TOML key.
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
            &adds(&self.nit.add_descriptors, "nit", |t| *t == Target::Network)?,
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
            &adds(&self.nit.add_descriptors, "nit", |t| *t == Target::Ts)?,
        )?;

        for (si, s) in self.services.iter().enumerate() {
            let sid = s.service_id;
            let svc = key::service(si);
            if let Some(cc) = &s.copy_control {
                cc.check(&format!("{svc}.copy_control"))?;
                if cc.recording_control == crate::signalling::RecordingControlCfg::Source
                    && !s
                        .streams
                        .iter()
                        .any(|st| st.kind == StreamKind::TsProgram || st.relay_es.is_some())
                {
                    return Err(MuxError::config(
                        format!("{svc}.copy_control.recording_control"),
                        "recording_control = \"source\" takes the value of a relayed \
                         source program; the service has no ts-program stream (give copy-free, \
                         copy-one-generation, copy-never or provider-defined)",
                    ));
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
            let mut raw = raws(
                &s.program_descriptors,
                &format!("{svc}.program_descriptors"),
            )?;
            raw.extend(adds(&s.pmt.add_descriptors, &format!("{svc}.pmt"), |t| {
                *t == Target::Program
            })?);
            check_loop(&format!("PMT program loop of {svc}"), &prog, &raw)?;

            // ---- Hybridcast
            if let Some(h) = &s.hybridcast {
                let hk = format!("{svc}.hybridcast");
                crate::hybridcast::check(h, &hk)?;
                let has_carousel = s
                    .streams
                    .iter()
                    .any(|st| st.kind == StreamKind::BmlCarousel);
                match h.carriage {
                    crate::config::AitCarriage::Carousel => {
                        if !has_carousel {
                            return Err(MuxError::config(
                                format!("{hk}.carriage"),
                                "carriage = \"carousel\" puts the AIT into the service's bml-carousel; add a [[service.stream]] kind = \"bml-carousel\" (or set carriage = \"es\" with pid and data_component_id)",
                            ));
                        }
                        if h.pid.is_some() || h.data_component_id.is_some() {
                            return Err(MuxError::NotApplicable {
                                key: format!(
                                    "{hk}.{}",
                                    if h.pid.is_some() {
                                        "pid"
                                    } else {
                                        "data_component_id"
                                    }
                                ),
                                applies_to: "carriage = \"es\"".to_string(),
                            });
                        }
                    }
                    crate::config::AitCarriage::Es => {
                        let pid = h.pid.ok_or_else(|| MuxError::Missing {
                            key: format!("{hk}.pid"),
                            needed_by: "carriage = \"es\"".to_string(),
                        })?;
                        if !pid.is_assignable() {
                            return Err(MuxError::PidRange {
                                key: format!("{hk}.pid"),
                                pid,
                            });
                        }
                        if h.data_component_id.is_none() {
                            return Err(MuxError::Missing {
                                key: format!("{hk}.data_component_id"),
                                needed_by: "carriage = \"es\" (the value of ARIB TR-B14 / B24 Vol. 4 §7.3; B24 v6.4 leaves it \"[T B D]\")".to_string(),
                            });
                        }
                        if let Some(k) = s.streams.iter().position(|st| st.pid == pid) {
                            return Err(MuxError::PidInUse {
                                key: format!("{hk}.pid"),
                                pid,
                                by: key::stream(si, s, k),
                                other_layer: false,
                            });
                        }
                    }
                }
            }
            // ---- PMT ES loops, typed option sanity
            for (k, st) in s.streams.iter().enumerate() {
                let stk = key::stream(si, s, k);
                check_ts_program_keys(st, &stk)?;
                if st.bml.is_some() && st.kind != StreamKind::BmlCarousel {
                    return Err(MuxError::NotApplicable {
                        key: format!("{stk}.bml"),
                        applies_to: "kind = \"bml-carousel\"".to_string(),
                    });
                }
                if st.kind == StreamKind::BmlCarousel {
                    let bml = st.bml.clone().unwrap_or_default();
                    let range = |field: &str, value: u32, min: i64, max: i64, note| {
                        let v = i64::from(value);
                        if (min..=max).contains(&v) {
                            Ok(())
                        } else {
                            Err(MuxError::Range {
                                key: format!("{stk}.bml.{field}"),
                                value: v,
                                min,
                                max,
                                note,
                            })
                        }
                    };
                    range(
                        "block_size",
                        u32::from(bml.block_size),
                        1,
                        i64::from(isdb_si::dsmcc::MAX_BLOCK_SIZE),
                        " (a DDB section is at most 4096 bytes, B24 Vol. 3 §6.5)",
                    )?;
                    range(
                        "data_event_id",
                        u32::from(bml.data_event_id),
                        0,
                        0xF,
                        " (4 bits)",
                    )?;
                    range(
                        "pmt_data_event_id",
                        u32::from(bml.pmt_data_event_id),
                        0,
                        0xF,
                        " (4 bits)",
                    )?;
                    range(
                        "download_id",
                        bml.download_id,
                        0,
                        0x0FFF_FFFF,
                        " (28 bits: data_event_id takes bits 31–28)",
                    )?;
                    range(
                        "transaction_number",
                        bml.transaction_number,
                        0,
                        0x3FFF_FFFF,
                        " (30 bits)",
                    )?;
                    range(
                        "reserved_future_use",
                        u32::from(bml.reserved_future_use),
                        0,
                        0xF,
                        " (4 bits)",
                    )?;
                    if bml.document_resolution.name() == "reserved" {
                        return Err(MuxError::Reserved {
                            key: format!("{stk}.bml.document_resolution"),
                            value: u32::from(bml.document_resolution.code()),
                            note: " (B24 Vol. 2 §9.3.2 Table 9-12)",
                        });
                    }
                    for (field, v) in [
                        ("cycle_ms", bml.cycle_ms),
                        ("dii_interval_ms", bml.dii_interval_ms),
                        ("bitrate_kbps", bml.bitrate_kbps),
                    ] {
                        if v == 0 {
                            return Err(MuxError::config(
                                format!("{stk}.bml.{field}"),
                                "must be > 0",
                            ));
                        }
                    }
                    crate::carousel::bml_versions(&bml, &stk)?;
                    if st.source.is_none()
                        && bml.modules.is_empty()
                        && !s.hybridcast.as_ref().is_some_and(|h| {
                            h.launcher && h.carriage == crate::config::AitCarriage::Carousel
                        })
                    {
                        return Err(MuxError::Missing {
                            key: format!("{stk}.source"),
                            needed_by: "kind = \"bml-carousel\" (the directory of BML files; or bml.modules, or a [service.hybridcast] launcher)".to_string(),
                        });
                    }
                    if let Some(t) = st.stream_type
                        && !matches!(t, 0x0B | 0x0D)
                    {
                        return Err(MuxError::config(
                            format!("{stk}.stream_type"),
                            format!(
                                "a data carousel has stream_type 0x0B or 0x0D (B24 Vol. 3 Table 4-1), not {t:#04x}"
                            ),
                        ));
                    }
                }
                st.video_decode_control.check(&stk)?;
                if st.kind == StreamKind::AacAdts
                    && st.video_decode_control.mode == VdcMode::Explicit
                {
                    return Err(MuxError::NotApplicable {
                        key: format!("{stk}.video_decode_control"),
                        applies_to: "video streams (B10 §6.2.30), not kind = \"aac-adts\""
                            .to_string(),
                    });
                }
                if st.kind != StreamKind::AacAdts
                    && (st.audio_quality_indicator != AudioQualityIndicator::Derived
                        || st.adts_compliance != AdtsCompliance::Conform)
                {
                    return Err(MuxError::NotApplicable {
                        key: format!(
                            "{stk}.{}",
                            if st.audio_quality_indicator != AudioQualityIndicator::Derived {
                                "audio_quality_indicator"
                            } else {
                                "adts_compliance"
                            }
                        ),
                        applies_to: "kind = \"aac-adts\"".to_string(),
                    });
                }
                let component_type =
                    |problem: &str| Err(MuxError::config(format!("{stk}.component_type"), problem));
                match (st.kind, st.component_type) {
                    (_, ComponentTypeCfg::Derived)
                    | (StreamKind::Mpeg2Video, ComponentTypeCfg::Video(_))
                    | (StreamKind::AacAdts, ComponentTypeCfg::Audio(_)) => {}
                    // declared streams have no media and get no EIT component
                    // descriptor; their component_type is unused
                    (StreamKind::Declared, _) => {}
                    (StreamKind::BmlCarousel, _) => {
                        return component_type(
                            "a bml-carousel has no component_type (its EIT descriptor is the data content descriptor)",
                        );
                    }
                    (StreamKind::TsProgram, _) => {
                        return component_type(
                            "component_type of a ts-program stream is derived from the relayed streams",
                        );
                    }
                    (StreamKind::Mpeg2Video, _) => {
                        return component_type(
                            "mpeg2-video needs a video component_type name (B10 Table 6-5), not an audio table",
                        );
                    }
                    (StreamKind::AacAdts, _) => {
                        return component_type(
                            "aac-adts needs an audio component_type table { audio_mode = … } (B10 Table 6-43)",
                        );
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
                let mut raw = raws(&st.descriptors, &format!("{stk}.descriptors"))?;
                raw.extend(adds(&s.pmt.add_descriptors, &format!("{svc}.pmt"), |t| {
                    *t == Target::Es(st.pid)
                })?);
                check_loop(&format!("PMT ES loop of {stk}"), &es, &raw)?;
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
            let mut raw = raws(&s.sdt_descriptors, &format!("{svc}.sdt_descriptors"))?;
            raw.extend(adds(&self.sdt.add_descriptors, "sdt", |t| {
                matches!(t, Target::Service(None)) || *t == Target::Service(Some(sid))
            })?);
            check_loop(&format!("SDT service loop of {svc}"), &sdt, &raw)?;

            // ---- EIT event loops
            for (ei, e) in s.events.iter().enumerate() {
                let evk = key::event(si, ei);
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
                let mut raw = raws(&e.descriptors, &format!("{evk}.descriptors"))?;
                raw.extend(adds(&self.eit.add_descriptors, "eit", pred)?);
                raw.extend(adds(
                    &self.eit_schedule.add_descriptors,
                    "eit_schedule",
                    pred,
                )?);
                check_loop(&format!("EIT event loop of {evk}"), &ev, &raw)?;
            }
        }

        // ---- BIT broadcaster loops
        for (bi, bc) in self.bit.broadcasters.iter().enumerate() {
            let bk = format!("bit.broadcasters[{bi}]");
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
            let mut raw = raws(&bc.descriptors, &format!("{bk}.descriptors"))?;
            raw.extend(adds(&self.bit.add_descriptors, "bit", |t| {
                matches!(t, Target::Broadcaster(None)) || *t == Target::Broadcaster(Some(id))
            })?);
            check_loop(&format!("BIT broadcaster loop of {bk}"), &t, &raw)?;
        }
        // ---- raw tables: the sections are the table
        for (i, t) in self.raw_tables.iter().enumerate() {
            if t.sections.is_empty() {
                return Err(MuxError::Missing {
                    key: format!("raw_table[{i}].sections"),
                    needed_by: "[[raw_table]] (one section at least)".to_string(),
                });
            }
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
                &adds(&self.tot.add_descriptors, "tot", |t| *t == Target::Main)?,
            )?;
        }
        Ok(())
    }
}
