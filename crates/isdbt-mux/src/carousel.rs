//! BML data broadcasting content: files → modules → DSM-CC data carousel
//! sections (ARIB STD-B24 Vol. 2 §9.1–9.2 resource mapping and
//! namespace, Vol. 3 Ch. 6 DII / DDB), for `kind = "bml-carousel"`.
//!
//! A module is either one file sent as is (Vol. 2 §9.1.2 (1) "Direct
//! mapping from one resource to one module", Type and Name descriptors in
//! the DII module information) or an HTTP/1.1 `multipart/mixed` entity
//! (§9.1.2 (2), syntax of §9.1.2.2) holding several resources, each
//! body-part with `Content-Type`, `Content-Location` and `Content-Length`,
//! with the `application/X-arib-resourceList` of §9.1.2.3 as its first
//! body-part. Module 0x0000 holds `startup.bml`, the document a receiver
//! launches first (§9.2.2). Resources are addressed as
//! `arib-dc://…/<component_tag>/<moduleId>/<resourceName>` (§9.2.1.2).
//!
//! The carousel of one component is one DII (table_id 0x3B) and the DDBs
//! (0x3C) of every module on one PID (Vol. 3 Annex C: "The DII and DDB
//! belonging to one carousel are transmitted in one ES"); the DII is
//! repeated between the DDBs at the configured interval.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use isdb_si::bxml::{BmlControl, BmlVersions};
use isdb_si::dsmcc::{
    DiiBody, DiiModule, ModuleDescriptor, download_id, module_blocks, transaction_id,
};
use isdb_si::values::CodedValue;

use crate::config::{BmlCfg, BmlModuleCfg, HybridcastCfg, SourceEncoding, StreamCfg};
use crate::error::{MuxError, Result};

/// component_tag of a `bml-carousel` without one: the entry component of
/// terrestrial data broadcasting (an ARIB TR-B14 convention, from
/// knowledge; B10 §6.2.16 leaves component_tag to the operator).
pub const DEFAULT_COMPONENT_TAG: u8 = 0x40;

/// Resource name of the document launched first (B24 Vol. 2 §9.2.2).
pub const STARTUP: &str = "startup.bml";

/// Media type of a section-format AIT resource in a module. Not in the
/// local texts (STD-0013 gives `application/X-arib-ait+xml` for the XML
/// form only); the section form takes the same name without the suffix.
pub const AIT_MEDIA_TYPE: &str = "application/X-arib-ait";

/// Media type of the resource list (B24 Vol. 2 §9.1.2.3).
pub const RESOURCE_LIST_MEDIA_TYPE: &str = "application/X-arib-resourceList";
/// Content-Location of the resource list body-part: the name a live
/// terrestrial carousel uses (the texts fix none).
pub const RESOURCE_LIST_NAME: &str = "resourceList";

/// Built-in file extension → media type (B24 Vol. 2 Table 9-6 names, the
/// set the basic-service operation of App. 2 Table 4-2 marks as operated;
/// the `charset` parameter written without a space as live terrestrial
/// carousels do).
const MEDIA_TYPES: &[(&str, &str)] = &[
    ("bml", "text/X-arib-bml;charset=\"euc-jp\""),
    ("css", "text/css"),
    ("js", "text/X-arib-ecmascript;charset=\"euc-jp\""),
    ("es", "text/X-arib-ecmascript;charset=\"euc-jp\""),
    ("ecm", "text/X-arib-ecmascript;charset=\"euc-jp\""),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("png", "image/X-arib-png"),
    ("mng", "image/X-arib-mng"),
    ("aif", "audio/X-arib-aiff"),
    ("aiff", "audio/X-arib-aiff"),
    ("aac", "audio/X-arib-mpeg2-aac"),
    ("clt", "application/X-arib-bmlclut"),
    ("clut", "application/X-arib-bmlclut"),
    ("btable", "application/X-arib-btable"),
    ("btb", "application/X-arib-btable"),
    ("drcs", "application/X-arib-drcs"),
    ("ait", AIT_MEDIA_TYPE),
];

/// Media type → resourceTypeValue (B24 Vol. 2 Tables 9-3 … 9-6: typeValue
/// (4) | subtypeValue (12)). Matched on the type/subtype before any
/// parameter, case-insensitively.
const RESOURCE_TYPE_VALUES: &[(&str, u16)] = &[
    ("multipart/mixed", 0x0000),
    ("text/css", 0x1020),
    ("text/x-arib-bml", 0x1401),
    ("text/x-arib-ecmascript", 0x1421),
    ("image/jpeg", 0x2000),
    ("image/png", 0x2001),
    ("image/x-arib-png", 0x2401),
    ("image/x-arib-mng", 0x2402),
    ("image/x-arib-mpeg2-i", 0x2403),
    ("audio/x-arib-aiff", 0x3401),
    ("audio/x-arib-mpeg2-aac", 0x3411),
    ("application/x-arib-bmlclut", 0x5431),
    ("application/x-arib-btable", 0x5441),
    ("application/x-arib-drcs", 0x5451),
    ("application/x-arib-resourcelist", 0x5471),
];

/// Media type for a file name from its extension: the configured `types`
/// first, then the built-in Table 9-6 mapping.
pub fn media_type<'a>(
    file: &str,
    overrides: &'a std::collections::BTreeMap<String, String>,
) -> Option<&'a str> {
    let ext = file.rsplit_once('.').map(|x| x.1).unwrap_or("");
    let ext_lower = ext.to_ascii_lowercase();
    if let Some(t) = overrides
        .iter()
        .find(|(k, _)| k.trim_start_matches('.').eq_ignore_ascii_case(&ext_lower))
    {
        return Some(t.1.as_str());
    }
    MEDIA_TYPES
        .iter()
        .find(|(e, _)| *e == ext_lower)
        .map(|(_, t)| *t)
}

/// The known extensions, for error messages.
pub fn known_extensions() -> String {
    let mut v: Vec<&str> = MEDIA_TYPES.iter().map(|(e, _)| *e).collect();
    v.dedup();
    v.join(", ")
}

/// resourceTypeValue of a media type (Table 9-6), if the table lists it.
pub fn resource_type_value(media_type: &str) -> Option<u16> {
    let base = media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    RESOURCE_TYPE_VALUES
        .iter()
        .find(|(m, _)| *m == base)
        .map(|(_, v)| *v)
}

/// Check a resource name against the B24 Vol. 2 §9.2.1.2 grammar
/// (`startChar *echar`, ≤ 127 bytes).
pub fn check_resource_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 127
        && (name.as_bytes()[0].is_ascii_alphanumeric() || name.starts_with('_'))
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.!~'();/@=+$,".contains(&b));
    if ok {
        Ok(())
    } else {
        Err(MuxError::Config(format!(
            "resource name {name:?} is not a B24 Vol. 2 §9.2.1.2 resourceName (alphanumeric or '_' first, then also -.!~'();/@=+$, up to 127 bytes)"
        )))
    }
}

/// One resource (file) of a module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    /// resourceName / Content-Location.
    pub name: String,
    /// Content-Type.
    pub media_type: String,
    /// Bytes.
    pub data: Vec<u8>,
}

/// One module of the carousel.
#[derive(Debug, Clone)]
pub struct Module {
    /// moduleId.
    pub id: u16,
    /// Resources (one for a directly mapped module).
    pub resources: Vec<Resource>,
    /// Multipart entity (false: the single resource is the module).
    pub multipart: bool,
    /// Name descriptor.
    pub name: Option<String>,
    /// The module bytes.
    pub bytes: Vec<u8>,
    /// Module information descriptors sent in the DII.
    pub info: Vec<ModuleDescriptor>,
}

/// The multipart boundary live terrestrial carousels use.
pub const BOUNDARY: &str = "IETF_RFC1945_Multipart_Mixed";
const CRLF: &str = "\r\n";

/// Content-Type of a multipart module (the DII Type descriptor and the
/// module's own entity header): `multipart/mixed; boundary="…"`.
pub fn multipart_content_type(boundary: &str) -> String {
    format!("multipart/mixed; boundary=\"{boundary}\"")
}

/// Build the multipart/mixed entity of `resources` (B24 Vol. 2 §9.1.2.2),
/// with the resource list (§9.1.2.3 Tables 9-1/9-2) as the first
/// body-part when `with_list`. Returns the entity and the boundary used.
pub fn multipart_entity(resources: &[Resource], with_list: bool) -> Result<(Vec<u8>, String)> {
    // a boundary that no resource contains
    let mut boundary = BOUNDARY.to_string();
    let mut n = 0u32;
    while resources
        .iter()
        .any(|r| contains(&r.data, format!("--{boundary}").as_bytes()))
    {
        n += 1;
        boundary = format!("{BOUNDARY}-{n}");
    }
    let delimiter = format!("--{boundary}{CRLF}");
    let close = format!("--{boundary}--{CRLF}");
    let top = format!(
        "Content-Type: {}{CRLF}{CRLF}",
        multipart_content_type(&boundary)
    );

    // body-part headers; the list's length is fixed by the names
    let header = |media_type: &str, name: &str, len: usize| {
        format!(
            "Content-Type: {media_type}{CRLF}Content-Location: {name}{CRLF}Content-Length: {len}{CRLF}"
        )
    };
    let list_len: usize = if with_list {
        1 + resources
            .iter()
            .map(|r| 1 + 13 + r.name.len())
            .sum::<usize>()
    } else {
        0
    };
    let mut parts: Vec<(String, usize)> = Vec::new(); // (header, data length)
    if with_list {
        parts.push((
            header(RESOURCE_LIST_MEDIA_TYPE, RESOURCE_LIST_NAME, list_len),
            list_len,
        ));
    }
    for r in resources {
        parts.push((header(&r.media_type, &r.name, r.data.len()), r.data.len()));
    }
    // offsets: body-part starts after the delimiter line
    let mut offsets = Vec::with_capacity(parts.len());
    let mut pos = top.len();
    for (h, len) in &parts {
        pos += delimiter.len();
        offsets.push((pos, h.len()));
        pos += h.len() + CRLF.len() + len + CRLF.len();
    }
    let total = pos + close.len();

    let list = if with_list {
        let mut l = Vec::with_capacity(list_len);
        l.push(resources.len() as u8);
        for (i, r) in resources.iter().enumerate() {
            let (off, hl) = offsets[i + 1];
            let tv = resource_type_value(&r.media_type).ok_or_else(|| {
                MuxError::Config(format!(
                    "resource {:?}: media type {:?} has no resourceTypeValue in B24 Vol. 2 Table 9-6; send the module without a resource list (resource_list = false)",
                    r.name, r.media_type
                ))
            })?;
            l.push((13 + r.name.len()) as u8);
            l.extend_from_slice(&(off as u32).to_be_bytes());
            l.extend_from_slice(&(hl as u16).to_be_bytes());
            l.extend_from_slice(&(r.data.len() as u32).to_be_bytes());
            l.extend_from_slice(&tv.to_be_bytes());
            l.push(0x80 | r.name.len() as u8); // reserved_future_use '1', resourceNameLength
            l.extend_from_slice(r.name.as_bytes());
        }
        debug_assert_eq!(l.len(), list_len);
        l
    } else {
        Vec::new()
    };

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(top.as_bytes());
    let mut datas: Vec<&[u8]> = Vec::new();
    if with_list {
        datas.push(&list);
    }
    datas.extend(resources.iter().map(|r| r.data.as_slice()));
    for ((h, _), d) in parts.iter().zip(datas) {
        out.extend_from_slice(delimiter.as_bytes());
        out.extend_from_slice(h.as_bytes());
        out.extend_from_slice(CRLF.as_bytes());
        out.extend_from_slice(d);
        out.extend_from_slice(CRLF.as_bytes());
    }
    out.extend_from_slice(close.as_bytes());
    debug_assert_eq!(out.len(), total);
    Ok((out, boundary))
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Parsed multipart entity (for tests and inspection): the resources in
/// order, the resource list excluded.
pub fn parse_multipart(entity: &[u8]) -> Result<Vec<Resource>> {
    let bad = |m: &str| MuxError::Config(format!("multipart entity: {m}"));
    let hdr_end = find(entity, b"\r\n\r\n", 0).ok_or_else(|| bad("no entity header end"))?;
    let top = std::str::from_utf8(&entity[..hdr_end]).map_err(|_| bad("header not ASCII"))?;
    let boundary = top
        .split("boundary=")
        .nth(1)
        .ok_or_else(|| bad("no boundary"))?
        .trim()
        .trim_matches('"');
    let delimiter = format!("--{boundary}\r\n");
    let close = format!("--{boundary}--\r\n");
    let mut pos = hdr_end + 4;
    let mut out = Vec::new();
    loop {
        if entity[pos..].starts_with(close.as_bytes()) {
            break;
        }
        if !entity[pos..].starts_with(delimiter.as_bytes()) {
            return Err(bad("expected a delimiter"));
        }
        pos += delimiter.len();
        let he = find(entity, b"\r\n\r\n", pos).ok_or_else(|| bad("no body-part header end"))?;
        let h = std::str::from_utf8(&entity[pos..he]).map_err(|_| bad("body-part header"))?;
        let field = |name: &str| {
            h.lines()
                .find_map(|l| l.strip_prefix(name))
                .map(|v| v.trim().to_string())
        };
        let media_type = field("Content-Type:").ok_or_else(|| bad("no Content-Type"))?;
        let name = field("Content-Location:").ok_or_else(|| bad("no Content-Location"))?;
        let len: usize = field("Content-Length:")
            .ok_or_else(|| bad("no Content-Length"))?
            .parse()
            .map_err(|_| bad("Content-Length"))?;
        let start = he + 4;
        let data = entity
            .get(start..start + len)
            .ok_or_else(|| bad("body-part truncated"))?
            .to_vec();
        pos = start + len;
        if !entity[pos..].starts_with(b"\r\n") {
            return Err(bad("no CRLF after body-part"));
        }
        pos += 2;
        if media_type != RESOURCE_LIST_MEDIA_TYPE {
            out.push(Resource {
                name,
                media_type,
                data,
            });
        }
    }
    Ok(out)
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}

/// What a carousel is built from, kept so that it can be rebuilt when
/// its files change (`bml.watch`).
#[derive(Debug, Clone)]
pub struct CarouselSource {
    /// The stream configuration (`bml` options, `source`, component_tag).
    pub stream: StreamCfg,
    /// The service's Hybridcast configuration, if any.
    pub hybridcast: Option<HybridcastCfg>,
    /// Directory `source` is relative to.
    pub base_dir: PathBuf,
    /// Name for messages.
    pub what: String,
}

/// The files a carousel was built from: (name, modification time, size).
pub type Snapshot = Vec<(String, Option<SystemTime>, u64)>;

/// A loaded carousel: its modules and the sections of one cycle.
#[derive(Debug, Clone)]
pub struct Carousel {
    /// component_tag of the component.
    pub component_tag: u8,
    /// downloadId.
    pub download_id: u32,
    /// transaction_id of the DII.
    pub transaction_id: u32,
    /// Transaction Number (transaction_id bits 29–0) of this build.
    pub transaction_number: u32,
    /// Modules.
    pub modules: Vec<Module>,
    /// moduleVersion per moduleId of this build.
    pub module_versions: BTreeMap<u16, u8>,
    /// The DII section.
    pub dii: Vec<u8>,
    /// Sections of one cycle: the DDBs of every module in order with the
    /// DII inserted at the configured spacing.
    pub sections: Vec<Vec<u8>>,
    /// Bytes of all modules (component_size of the EIT carousel info).
    pub component_size: u32,
    /// Lines for the start-up report.
    pub report: Vec<String>,
    /// What it was built from.
    pub source: CarouselSource,
    /// The source files as they were when it was built.
    pub snapshot: Snapshot,
}

/// Modification time and size of every source file of `src` (the listed
/// files, or every file of the directory for the default layout).
pub fn snapshot(src: &CarouselSource) -> Result<Snapshot> {
    let bml = src.stream.bml.clone().unwrap_or_default();
    let Some(dir) = src.stream.source.as_ref().map(|d| src.base_dir.join(d)) else {
        return Ok(Vec::new());
    };
    let io = |e: std::io::Error| MuxError::Media {
        path: dir.display().to_string(),
        msg: e.to_string(),
    };
    let mut names: Vec<String> = if bml.modules.is_empty() {
        let mut v = Vec::new();
        for e in std::fs::read_dir(&dir).map_err(io)? {
            let e = e.map_err(io)?;
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                v.push(e.file_name().to_string_lossy().into_owned());
            }
        }
        v
    } else {
        bml.modules
            .iter()
            .flat_map(|m| m.files.iter().cloned())
            .collect()
    };
    names.sort();
    let mut out = Vec::with_capacity(names.len());
    for n in names {
        let p = dir.join(&n);
        match std::fs::metadata(&p) {
            Ok(m) => out.push((n, m.modified().ok(), m.len())),
            Err(_) => out.push((n, None, 0)),
        }
    }
    Ok(out)
}

/// Read the files of a `bml-carousel` stream and build its sections.
/// `hybridcast` adds the AIT module (and the launcher `startup.bml`
/// unless the files have one) when its carriage is the carousel.
pub fn load(
    st: &StreamCfg,
    hybridcast: Option<&HybridcastCfg>,
    base_dir: &Path,
    what: &str,
) -> Result<Carousel> {
    build(
        &CarouselSource {
            stream: st.clone(),
            hybridcast: hybridcast.cloned(),
            base_dir: base_dir.to_path_buf(),
            what: what.to_string(),
        },
        None,
    )
}

/// Build (or, with `prev`, rebuild) a carousel from its source. On a
/// rebuild every module whose bytes or descriptors changed gets
/// moduleVersion + 1 (a new module starts at the configured version), and
/// the DII gets transaction number + 1 when anything in it changed; an
/// unchanged carousel keeps its versions (B24 Vol. 3 §6.2: "the version
/// number is incremented by one when content of one or more DII messages
/// have been changed"; §6.3.1 moduleVersion).
pub fn build(src: &CarouselSource, prev: Option<&Carousel>) -> Result<Carousel> {
    let st = &src.stream;
    let hybridcast = src.hybridcast.as_ref();
    let base_dir = src.base_dir.as_path();
    let what = src.what.as_str();
    let snap = snapshot(src)?;
    let bml = st.bml.clone().unwrap_or_default();
    let component_tag = st.component_tag.unwrap_or(DEFAULT_COMPONENT_TAG);
    let dir = st.source.as_ref().map(|s| base_dir.join(s));
    let read = |name: &str| -> Result<Vec<u8>> {
        let d = dir.as_ref().ok_or_else(|| {
            MuxError::Config(format!(
                "{what}: module file {name:?} needs source = the directory it is in"
            ))
        })?;
        let p = d.join(name);
        std::fs::read(&p).map_err(|e| MuxError::Media {
            path: p.display().to_string(),
            msg: e.to_string(),
        })
    };
    let resource = |name: &str, data: Vec<u8>| -> Result<Resource> {
        check_resource_name(name)?;
        let media_type = media_type(name, &bml.types).ok_or_else(|| {
            MuxError::Config(format!(
                "{what}: no media type for {name:?}; known extensions: {} (B24 Vol. 2 Table 9-6); add [service.stream.bml] types = {{ \"<ext>\" = \"<media type>\" }}",
                known_extensions()
            ))
        })?;
        let data = if bml.source_encoding == SourceEncoding::Utf8
            && media_type.to_ascii_lowercase().starts_with("text/")
        {
            transcode_utf8_to_euc_jp(&data, &format!("{what}: {name}"))?
        } else {
            data
        };
        Ok(Resource {
            name: name.to_string(),
            media_type: media_type.to_string(),
            data,
        })
    };

    // ---- module layout
    let mut layout: Vec<BmlModuleCfg> = bml.modules.clone();
    if layout.is_empty() {
        let mut files = Vec::new();
        if let Some(d) = &dir {
            let rd = std::fs::read_dir(d).map_err(|e| MuxError::Media {
                path: d.display().to_string(),
                msg: e.to_string(),
            })?;
            for e in rd {
                let e = e.map_err(|e| MuxError::Media {
                    path: d.display().to_string(),
                    msg: e.to_string(),
                })?;
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    files.push(e.file_name().to_string_lossy().into_owned());
                }
            }
            files.sort();
        }
        // startup.bml first (the receiver reads it first)
        if let Some(i) = files.iter().position(|f| f == STARTUP) {
            let s = files.remove(i);
            files.insert(0, s);
        }
        layout.push(BmlModuleCfg {
            id: 0,
            files,
            multipart: true,
            resource_list: true,
            name: None,
        });
    }
    for (i, m) in layout.iter().enumerate() {
        if layout[..i].iter().any(|x| x.id == m.id) {
            return Err(MuxError::Config(format!(
                "{what}: moduleId {:#06x} is listed twice",
                m.id
            )));
        }
    }

    // ---- hybridcast: AIT module and launcher
    let hc = hybridcast.filter(|h| h.carriage == crate::config::AitCarriage::Carousel);
    let mut generated: Vec<(u16, Resource)> = Vec::new();
    let mut report = Vec::new();
    if let Some(h) = hc {
        if layout.iter().any(|m| m.id == h.module_id) {
            return Err(MuxError::Config(format!(
                "{what}: hybridcast module_id {:#06x} is also a bml module; choose another",
                h.module_id
            )));
        }
        let ait = crate::hybridcast::ait_section(h)?;
        layout.push(BmlModuleCfg {
            id: h.module_id,
            files: Vec::new(),
            multipart: true,
            resource_list: false,
            name: None,
        });
        generated.push((
            h.module_id,
            Resource {
                name: h.resource.clone(),
                media_type: AIT_MEDIA_TYPE.to_string(),
                data: ait,
            },
        ));
        let has_startup = layout.iter().any(|m| m.files.iter().any(|f| f == STARTUP));
        if h.launcher && !has_startup {
            let Some(m0) = layout.iter().find(|m| m.id == 0) else {
                return Err(MuxError::Config(format!(
                    "{what}: the hybridcast launcher startup.bml goes into module 0x0000; list a module with id = 0"
                )));
            };
            if !m0.multipart && !m0.files.is_empty() {
                return Err(MuxError::Config(format!(
                    "{what}: module 0x0000 is a single-file module; the hybridcast launcher needs a multipart module 0 (multipart = true) or your own startup.bml"
                )));
            }
            let doc = crate::hybridcast::launcher_bml(
                h,
                component_tag,
                bml.document_resolution
                    .name()
                    .trim_end_matches("-16:9")
                    .trim_end_matches("-4:3"),
            );
            generated.push((
                0,
                Resource {
                    name: STARTUP.to_string(),
                    media_type: media_type(STARTUP, &bml.types)
                        .unwrap_or("text/X-arib-bml;charset=\"euc-jp\"")
                        .to_string(),
                    data: doc.into_bytes(),
                },
            ));
            report.push(format!(
                "{what}: generated {STARTUP} (Hybridcast launcher: startAITControlledApp({}, {}, arib-dc://-1.-1.-1/{component_tag:02X}/{:04X}/{}))",
                h.organization_id, h.application_id, h.module_id, h.resource
            ));
        }
        report.push(format!(
            "{what}: AIT module {:#06x}/{} → {} ({})",
            h.module_id,
            h.resource,
            h.url,
            h.control_code.name()
        ));
    }

    // ---- build the modules
    let versions = bml_versions(&bml, what)?;
    let mut modules = Vec::new();
    for m in &layout {
        let mut resources = Vec::new();
        // generated resources first in module 0 (startup.bml), the files after
        for (_, r) in generated.iter().filter(|(id, _)| *id == m.id) {
            resources.push(r.clone());
        }
        for f in &m.files {
            let data = read(f)?;
            let name = Path::new(f)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| f.clone());
            if resources.iter().any(|r| r.name == name) {
                return Err(MuxError::Config(format!(
                    "{what}: module {:#06x} lists resource {name:?} twice",
                    m.id
                )));
            }
            resources.push(resource(&name, data)?);
        }
        if resources.is_empty() {
            return Err(MuxError::Config(format!(
                "{what}: module {:#06x} has no files (give source = <directory> holding startup.bml and the other BML files, or modules = [{{ id = …, files = […] }}])",
                m.id
            )));
        }
        let multipart = m.multipart || resources.len() > 1;
        let (bytes, mut info) = if multipart {
            let (bytes, boundary) = multipart_entity(&resources, m.resource_list)?;
            (
                bytes,
                vec![ModuleDescriptor::Type {
                    media_type: multipart_content_type(&boundary).into_bytes(),
                }],
            )
        } else {
            let r = &resources[0];
            (
                r.data.clone(),
                vec![
                    ModuleDescriptor::Type {
                        media_type: r.media_type.clone().into_bytes(),
                    },
                    ModuleDescriptor::Name {
                        name: m
                            .name
                            .clone()
                            .unwrap_or_else(|| r.name.clone())
                            .into_bytes(),
                    },
                ],
            )
        };
        if multipart && let Some(n) = &m.name {
            info.push(ModuleDescriptor::Name {
                name: n.clone().into_bytes(),
            });
        }
        if bml.control_descriptor
            && let Some(v) = &versions
            && resources.iter().any(|r| {
                r.media_type
                    .to_ascii_lowercase()
                    .starts_with("text/x-arib-bml")
            })
        {
            info.push(ModuleDescriptor::Control {
                data: BmlControl {
                    use_xml: false,
                    reserved: 0x7F,
                    versions: *v,
                }
                .encode()?,
            });
        }
        modules.push(Module {
            id: m.id,
            resources,
            multipart,
            name: m.name.clone(),
            bytes,
            info,
        });
    }
    if bml.entry_point && hc.is_none() {
        let has = modules.iter().find(|m| m.id == 0).is_some_and(|m| {
            m.resources.iter().any(|r| r.name == STARTUP)
                || (!m.multipart && m.name.as_deref() == Some(STARTUP))
        });
        if !has {
            return Err(MuxError::Config(format!(
                "{what}: entry_point = true needs {STARTUP} in module 0x0000 (B24 Vol. 2 §9.2.2); add the file, set bml.entry_point = false, or configure [service.hybridcast] to generate a launcher"
            )));
        }
    }

    // ---- versions: changed modules + 1, the DII transaction + 1 on any change
    let mut module_versions = BTreeMap::new();
    let mut changed = Vec::new();
    for m in &modules {
        let v = match prev.and_then(|p| {
            p.modules
                .iter()
                .find(|x| x.id == m.id)
                .map(|x| (x, p.module_versions.get(&m.id).copied()))
        }) {
            Some((old, Some(v))) if old.bytes == m.bytes && old.info == m.info => v,
            Some((_, Some(v))) => {
                changed.push(m.id);
                v.wrapping_add(1)
            }
            _ => {
                if prev.is_some() {
                    changed.push(m.id);
                }
                bml.module_version
            }
        };
        module_versions.insert(m.id, v);
    }
    let removed = prev.is_some_and(|p| {
        p.modules
            .iter()
            .any(|x| !modules.iter().any(|m| m.id == x.id))
    });
    let transaction_number = match prev {
        Some(p) if changed.is_empty() && !removed => p.transaction_number,
        Some(p) => (p.transaction_number + 1) & 0x3FFF_FFFF,
        None => bml.transaction_number,
    };

    // ---- sections
    let did = download_id(bml.data_event_id, bml.download_id);
    let tid = transaction_id(transaction_number);
    let component_size: usize = modules.iter().map(|m| m.bytes.len()).sum();
    let bytes_per_s = f64::from(bml.bitrate_kbps.max(1)) * 1000.0 / 8.0;
    let cycle_s = (component_size as f64 * 1.1 / bytes_per_s).max(f64::from(bml.cycle_ms) / 1000.0);
    let mut dii = DiiBody::new(tid, did, bml.block_size);
    // live terrestrial carousels send 0xFFFFFFFF (no timeout)
    dii.t_c_download_scenario = u32::MAX;
    for m in &modules {
        dii.modules.push(DiiModule {
            module_id: m.id,
            module_size: m.bytes.len() as u32,
            module_version: module_versions[&m.id],
            info: m.info.clone(),
        });
    }
    if bml.return_to_entry.unwrap_or(bml.entry_point) {
        dii.private.push(ModuleDescriptor::AribBxmlPrivateData {
            return_to_entry: true,
            reserved: 0x7F,
        });
    }
    let dii_bytes = dii.section().encode().map_err(|e| {
        MuxError::Config(format!(
            "{what}: DII does not fit one 4096-byte section ({} modules): {e}",
            modules.len()
        ))
    })?;
    let mut ddbs = Vec::new();
    for m in &modules {
        for s in module_blocks(did, m.id, module_versions[&m.id], bml.block_size, &m.bytes)
            .map_err(|e| MuxError::Config(format!("{what}: module {:#06x}: {e}", m.id)))?
        {
            ddbs.push(s.encode()?);
        }
    }
    // DIIs between the DDBs: one per dii_interval_ms at the configured rate
    let per_dii = (bytes_per_s * f64::from(bml.dii_interval_ms) / 1000.0).max(1.0) as usize;
    let mut sections = vec![dii_bytes.clone()];
    let mut acc = 0usize;
    for d in ddbs {
        if acc >= per_dii {
            sections.push(dii_bytes.clone());
            acc = 0;
        }
        acc += d.len();
        sections.push(d);
    }
    if prev.is_some() {
        report.push(format!(
            "{what}: carousel rebuilt, transaction {transaction_number}: modules changed {}{}",
            changed
                .iter()
                .map(|id| format!("{id:#06x} (version {})", module_versions[id]))
                .collect::<Vec<_>>()
                .join(", "),
            if removed { ", modules removed" } else { "" }
        ));
    }
    report.insert(
        0,
        format!(
            "{what}: component_tag {component_tag:#04x}, {} module(s), {component_size} bytes, {} DDB(s) of {} bytes, cycle ≈ {:.2} s at {} kbit/s: {}",
            modules.len(),
            sections.len() - sections.iter().filter(|s| s[0] == 0x3B).count(),
            bml.block_size,
            cycle_s,
            bml.bitrate_kbps,
            modules
                .iter()
                .map(|m| format!(
                    "{:#06x}{}[{}]",
                    m.id,
                    if m.multipart { " multipart" } else { "" },
                    m.resources.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ")
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    );
    Ok(Carousel {
        component_tag,
        download_id: did,
        transaction_id: tid,
        transaction_number,
        modules,
        module_versions,
        dii: dii_bytes,
        sections,
        component_size: component_size as u32,
        report,
        source: src.clone(),
        snapshot: snap,
    })
}

/// Transcode a UTF-8 text resource to EUC-JP (`bml.source_encoding =
/// "utf-8"`); an XML declaration naming UTF-8 is rewritten to EUC-JP.
/// Characters EUC-JP cannot code are an error naming the first one.
pub fn transcode_utf8_to_euc_jp(data: &[u8], what: &str) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(data)
        .map_err(|e| MuxError::Config(format!("{what}: not valid UTF-8 ({e})")))?;
    let mut text = text.to_string();
    for from in [
        "encoding=\"UTF-8\"",
        "encoding=\"utf-8\"",
        "encoding='UTF-8'",
        "encoding='utf-8'",
    ] {
        if text.contains(from) {
            text = text.replacen(from, "encoding=\"EUC-JP\"", 1);
            break;
        }
    }
    let (out, _, had_errors) = encoding_rs::EUC_JP.encode(&text);
    if had_errors {
        let bad = text
            .chars()
            .find(|c| {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                encoding_rs::EUC_JP.encode(s).2
            })
            .map(|c| format!("{c:?} (U+{:04X})", c as u32))
            .unwrap_or_default();
        return Err(MuxError::Config(format!(
            "{what}: {bad} cannot be coded in EUC-JP (JIS X 0208); replace it"
        )));
    }
    Ok(out.into_owned())
}

/// `bml_version = "major.minor"` → the versions sent with
/// default_version_flag 0.
pub fn bml_versions(bml: &BmlCfg, what: &str) -> Result<Option<BmlVersions>> {
    let Some(v) = bml.bml_version.as_deref().filter(|v| !v.trim().is_empty()) else {
        return Ok(None);
    };
    let bad = || MuxError::Config(format!("{what}: bml_version {v:?}: give \"major.minor\""));
    let (a, b) = v.split_once('.').ok_or_else(bad)?;
    Ok(Some(BmlVersions {
        bml_major: a.trim().parse().map_err(|_| bad())?,
        bml_minor: b.trim().parse().map_err(|_| bad())?,
        bxml: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types() {
        let o = Default::default();
        assert_eq!(
            media_type("startup.bml", &o),
            Some("text/X-arib-bml;charset=\"euc-jp\"")
        );
        assert_eq!(media_type("A.PNG", &o), Some("image/X-arib-png"));
        assert_eq!(media_type("x.bin", &o), None);
        let mut o = std::collections::BTreeMap::new();
        o.insert("bin".to_string(), "application/octet-stream".to_string());
        assert_eq!(media_type("x.bin", &o), Some("application/octet-stream"));
        assert_eq!(
            resource_type_value("text/X-arib-bml;charset=\"euc-jp\""),
            Some(0x1401)
        );
        assert_eq!(resource_type_value("image/jpeg"), Some(0x2000));
        assert_eq!(resource_type_value(AIT_MEDIA_TYPE), None);
    }

    #[test]
    fn multipart_offsets_match_the_resource_list() {
        let res = vec![
            Resource {
                name: "startup.bml".into(),
                media_type: "text/X-arib-bml;charset=\"euc-jp\"".into(),
                data: b"<bml/>".to_vec(),
            },
            Resource {
                name: "a.png".into(),
                media_type: "image/X-arib-png".into(),
                data: vec![0x89, b'P', b'N', b'G', 0, 1, 2],
            },
        ];
        let (e, _) = multipart_entity(&res, true).unwrap();
        assert!(e.starts_with(
            b"Content-Type: multipart/mixed; boundary=\"IETF_RFC1945_Multipart_Mixed\"\r\n\r\n--IETF_RFC1945_Multipart_Mixed\r\n"
        ));
        assert_eq!(parse_multipart(&e).unwrap(), res);
        // resource list: first body-part
        let all = {
            // parse again keeping the list
            let hdr_end = find(&e, b"\r\n\r\n", 0).unwrap();
            let he = find(&e, b"\r\n\r\n", hdr_end + 4).unwrap();
            let h = std::str::from_utf8(&e[hdr_end + 4..he]).unwrap();
            assert!(h.contains(RESOURCE_LIST_MEDIA_TYPE));
            let len: usize = h
                .lines()
                .find_map(|l| l.strip_prefix("Content-Length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            e[he + 4..he + 4 + len].to_vec()
        };
        assert_eq!(all[0], 2);
        let mut p = 1;
        for r in &res {
            let n = all[p] as usize;
            let off = u32::from_be_bytes(all[p + 1..p + 5].try_into().unwrap()) as usize;
            let hl = u16::from_be_bytes(all[p + 5..p + 7].try_into().unwrap()) as usize;
            let rl = u32::from_be_bytes(all[p + 7..p + 11].try_into().unwrap()) as usize;
            let tv = u16::from_be_bytes(all[p + 11..p + 13].try_into().unwrap());
            let nl = (all[p + 13] & 0x7F) as usize;
            assert_eq!(all[p + 13] & 0x80, 0x80);
            assert_eq!(&all[p + 14..p + 14 + nl], r.name.as_bytes());
            assert_eq!(n, 13 + nl);
            assert_eq!(rl, r.data.len());
            assert_eq!(tv, resource_type_value(&r.media_type).unwrap());
            // the body-part header starts at off and is hl bytes, then CRLF, then the data
            let h = std::str::from_utf8(&e[off..off + hl]).unwrap();
            assert!(h.starts_with("Content-Type: "), "{h}");
            assert!(h.contains(&format!("Content-Location: {}", r.name)));
            assert_eq!(&e[off + hl..off + hl + 2], b"\r\n");
            assert_eq!(&e[off + hl + 2..off + hl + 2 + rl], &r.data[..]);
            p += 1 + n;
        }
        assert_eq!(p, all.len());
        // without a list: parses to the same resources
        assert_eq!(
            parse_multipart(&multipart_entity(&res, false).unwrap().0).unwrap(),
            res
        );
    }

    #[test]
    fn boundary_avoids_resource_contents() {
        let res = vec![Resource {
            name: "x.css".into(),
            media_type: "text/css".into(),
            data: format!("--{BOUNDARY}").into_bytes(),
        }];
        let (e, b) = multipart_entity(&res, true).unwrap();
        assert_eq!(b, format!("{BOUNDARY}-1"));
        assert!(contains(
            &e,
            format!("boundary=\"{BOUNDARY}-1\"").as_bytes()
        ));
        assert_eq!(parse_multipart(&e).unwrap(), res);
    }

    #[test]
    fn utf8_sources_are_transcoded() {
        let src = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<p>データ放送</p>";
        let out = transcode_utf8_to_euc_jp(src.as_bytes(), "t").unwrap();
        assert!(out.starts_with(b"<?xml version=\"1.0\" encoding=\"EUC-JP\"?>"));
        let (back, _, _) = encoding_rs::EUC_JP.decode(&out);
        assert!(back.contains("データ放送"));
        assert!(!out.windows(3).any(|w| w == "デ".as_bytes()));
        let e = transcode_utf8_to_euc_jp("a\u{1F600}b".as_bytes(), "t")
            .unwrap_err()
            .to_string();
        assert!(e.contains("U+1F600"), "{e}");
        assert!(transcode_utf8_to_euc_jp(&[0xFF, 0xFE], "t").is_err());
    }

    #[test]
    fn resource_names() {
        assert!(check_resource_name("startup.bml").is_ok());
        assert!(check_resource_name("_a-b.c").is_ok());
        assert!(check_resource_name("-a").is_err());
        assert!(check_resource_name("a b").is_err());
        assert!(check_resource_name("").is_err());
    }
}
