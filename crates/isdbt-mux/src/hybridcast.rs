//! Hybridcast application signalling: the AIT that names the HTML5
//! application a receiver starts (ARIB STD-B24 Vol. 4 Ch. 5; IPTVFJ
//! STD-0010 §7, STD-0013 §5.2), its PMT identification when it is carried
//! as its own component (B24 Vol. 4 §7.3) and the BML launcher document
//! that starts the application from the data carousel (STD-0013 §6.1.5,
//! §8.1.3.3).
//!
//! One AIT, one application, the IPTVFJ STD-0013 values: application_type
//! 0x0010, application_profile 0x0002 version 1.1.1, control code
//! AUTOSTART (or PRESENT / KILL), a common-loop transport protocol
//! descriptor (protocol_id 0x0003 HTTP/HTTPS, label 0) with the URL base,
//! an application descriptor, a simple application location descriptor
//! with the initial path and an application boundary and permission
//! descriptor (B24 Vol. 4 Annex A Fig. A-2 shows this minimal set).

use isdb_si::ait::{
    AitApplication, AitBody, AitDescriptor, ApplicationDescriptor, ApplicationProfile,
    BoundaryAndPermission, BoundaryEntry, HttpSelector, HttpUrl, SimpleApplicationLocation,
    TransportProtocolDescriptor, protocol_id,
};
use isdb_si::bxml::{AitIdentifier, AitIdentifierInfo};
use isdb_si::values::CodedValue;

use crate::config::HybridcastCfg;
use crate::error::{MuxError, Result};

/// The URL split the AIT carries: `base` (URL_base_byte, up to and
/// including the last `/`), `initial_path` (initial_path_bytes) and
/// `origin` (scheme and host, the default boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitUrl {
    /// URL_base.
    pub base: String,
    /// initial_path.
    pub initial_path: String,
    /// `scheme://host[:port]`.
    pub origin: String,
}

/// Split a start URL (B24 Vol. 4 §5.3.2 URL_base + §5.3.3 initial_path;
/// STD-0010 §7.4.1 "the URL is URL_base + URL_extension + initial_path").
pub fn split_url(url: &str) -> Result<SplitUrl> {
    let bad = |m: &str| MuxError::Config(format!("hybridcast url {url:?}: {m}"));
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| bad("needs a scheme (http:// or https://)"))?;
    if !matches!(scheme, "http" | "https") {
        return Err(bad(
            "the scheme must be http or https (B24 Vol. 4 §5.3.2 protocol_id 0x0003)",
        ));
    }
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if host.is_empty() {
        return Err(bad("no host"));
    }
    if url.bytes().any(|b| b <= b' ' || b >= 0x7F) {
        return Err(bad("must be ASCII without spaces or control characters"));
    }
    let origin = format!("{scheme}://{host}");
    let cut = path.rfind('/').unwrap_or(0) + 1;
    Ok(SplitUrl {
        base: format!("{origin}{}", &path[..cut]),
        initial_path: path[cut..].to_string(),
        origin,
    })
}

/// Check the fixed-size fields of a configuration (called at load).
pub fn check(cfg: &HybridcastCfg, what: &str) -> Result<()> {
    if cfg.organization_id == 0 {
        return Err(MuxError::Config(format!(
            "{what}: organization_id 0 is reserved (IPTVFJ STD-0013 §5.2.1: \"value '0' cannot be entered in an AIT\")"
        )));
    }
    if cfg.control_code.name() == "reserved"
        || cfg.control_code == isdb_si::values::ApplicationControlCode::Prefetch
    {
        return Err(MuxError::Config(format!(
            "{what}: control_code must be autostart, present or kill (STD-0013 §5.2.3: PREFETCH shall not be used)"
        )));
    }
    if cfg.visibility.name() == "reserved" {
        return Err(MuxError::Config(format!(
            "{what}: visibility '10' is reserved"
        )));
    }
    if cfg.permission_bitmap >> 13 != 0 {
        return Err(MuxError::Config(format!(
            "{what}: permission_bitmap {:#06x}: the upper 3 bits select the bitmap layout; STD-0013 §8.2.4.2.17.1 defines '000' (0x1FFF = all, 0 = none) and '001' (per-function bits)",
            cfg.permission_bitmap
        )));
    }
    if cfg.application_priority > 3 {
        return Err(MuxError::Config(format!(
            "{what}: application_priority is 2 bits (0..3)"
        )));
    }
    if cfg.ait_version > 31 {
        return Err(MuxError::Config(format!(
            "{what}: ait_version is 5 bits (0..31)"
        )));
    }
    cfg.profile_version()?;
    split_url(&cfg.url)?;
    for b in &cfg.boundaries {
        if !(b.starts_with("http://") || b.starts_with("https://")) {
            return Err(MuxError::Config(format!(
                "{what}: boundary {b:?} must start with http:// or https:// (STD-0013 §6.1.3.1)"
            )));
        }
    }
    if cfg.resource.is_empty()
        || cfg.resource.len() > 127
        || !cfg
            .resource
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.!~'();/@=+$,".contains(&b))
        || !(cfg.resource.as_bytes()[0].is_ascii_alphanumeric() || cfg.resource.starts_with('_'))
    {
        return Err(MuxError::Config(format!(
            "{what}: resource {:?} is not a B24 Vol. 2 §9.2.1.2 resource name (alphanumeric or '_' first, then also -.!~'();/@=+$, up to 127 bytes)",
            cfg.resource
        )));
    }
    Ok(())
}

/// Encode the AIT section (table_id 0x74) of `cfg`.
pub fn ait_section(cfg: &HybridcastCfg) -> Result<Vec<u8>> {
    let url = split_url(&cfg.url)?;
    let (major, minor, micro) = cfg.profile_version()?;
    let mut body = AitBody::new();
    body.common_descriptors
        .push(AitDescriptor::TransportProtocol(
            TransportProtocolDescriptor {
                protocol_id: protocol_id::HTTP,
                label: 0,
                selector: HttpSelector {
                    urls: vec![HttpUrl {
                        base: url.base.into_bytes(),
                        extensions: Vec::new(),
                    }],
                }
                .encode()?,
            },
        ));
    let boundaries: Vec<Vec<u8>> = if cfg.boundaries.is_empty() {
        vec![url.origin.into_bytes()]
    } else {
        cfg.boundaries
            .iter()
            .map(|b| b.clone().into_bytes())
            .collect()
    };
    let mut descriptors = vec![
        AitDescriptor::Application(ApplicationDescriptor {
            profiles: vec![ApplicationProfile {
                profile: cfg.profile,
                major,
                minor,
                micro,
            }],
            service_bound: cfg.service_bound,
            visibility: cfg.visibility.code(),
            reserved: 0x1F,
            priority: cfg.priority,
            transport_protocol_labels: vec![0],
        }),
        AitDescriptor::SimpleApplicationLocation(SimpleApplicationLocation {
            initial_path: url.initial_path.into_bytes(),
        }),
        AitDescriptor::BoundaryAndPermission(BoundaryAndPermission {
            entries: vec![BoundaryEntry {
                permission_bitmaps: vec![cfg.permission_bitmap],
                managed_urls: boundaries,
            }],
        }),
    ];
    if let Some(p) = cfg.autostart_priority {
        descriptors.push(AitDescriptor::AutostartPriority { priority: p });
    }
    body.applications.push(AitApplication {
        organization_id: cfg.organization_id,
        application_id: cfg.application_id,
        control_code: cfg.control_code.code(),
        reserved: 0xF,
        descriptors,
    });
    Ok(body
        .section(cfg.application_type, cfg.ait_version)
        .encode()?)
}

/// ait_identifier_info() of the PMT data component descriptor of a
/// dedicated AIT component (B24 Vol. 4 Table 7-1): transport_type 1 "AIT
/// section transmission".
pub fn ait_identifier_info(cfg: &HybridcastCfg) -> Result<Vec<u8>> {
    Ok(AitIdentifierInfo {
        entries: vec![AitIdentifier {
            application_type: cfg.application_type,
            section_transport: true,
            application_priority: cfg.application_priority & 3,
            ait_version: cfg.ait_version & 0x1F,
        }],
    }
    .encode()?)
}

/// The default launcher prologue: the XML declaration, DOCTYPE and
/// bml-version processing instruction of terrestrial BML documents as a
/// live broadcast sends them (B24 Vol. 2 §5.2 fixes the form; the public
/// identifier and the version 3.0 are the TR-B14 operation, read from a
/// terrestrial recording).
pub const LAUNCHER_PROLOGUE: &str = "<?xml version=\"1.0\" encoding=\"EUC-JP\"?>\n<!DOCTYPE bml PUBLIC \"+//ARIB STD-B24:1999//DTD BML Document//JA\" \"http://www.arib.or.jp/B24/DTD/bml_1_1.dtd\">\n<?bml bml-version=\"3.0\"?>\n";

/// The `startup.bml` that starts the application: on load it checks the
/// AIT-controlled application API (`getBrowserSupport('ARIB', 'APIGroup',
/// 'AITControlledApp.Start')`, STD-0013 §6.1.5) and calls
/// `browser.startAITControlledApp(organization_id, application_id, uri)`
/// with the carousel URI of the section-format AIT
/// (`arib-dc://-1.-1.-1/<component_tag>/<moduleId>/<resource>`, STD-0013
/// §8.1.3.3). The body keeps the broadcast video and audio on screen
/// (`video/X-arib-mpeg2` / `audio/X-arib-mpeg2-aac` objects with
/// `data="/-1"`, as live startup documents do) and shows one status line:
/// whether the receiver supports AIT-controlled applications (a BML-only
/// receiver loads the page and says so). ASCII only, so the EUC-JP
/// declaration holds.
pub fn launcher_bml(cfg: &HybridcastCfg, component_tag: u8, resolution: &str) -> String {
    let uri = format!(
        "arib-dc://-1.-1.-1/{component_tag:02X}/{:04X}/{}",
        cfg.module_id, cfg.resource
    );
    let prologue = cfg
        .launcher_prologue
        .as_deref()
        .unwrap_or(LAUNCHER_PROLOGUE);
    let (w, h) = resolution.split_once('x').unwrap_or(("960", "540"));
    format!(
        "{prologue}<bml>\n<head>\n<title>Hybridcast launcher</title>\n<style>\n<![CDATA[\nbody {{\n\tresolution:{resolution};\n\tdisplay-aspect-ratio:16v9;\n\tused-key-list:basic data-button\n}}\n]]>\n</style>\n<script>\n<![CDATA[\nfunction init() {{\n\tvar msg = document.getElementById('msg');\n\tif (browser.getBrowserSupport('ARIB', 'APIGroup', 'AITControlledApp.Start') == 1) {{\n\t\tmsg.firstChild.data = 'Starting the Hybridcast application {org}/{app}';\n\t\tbrowser.startAITControlledApp({org}, {app}, '{uri}');\n\t}} else {{\n\t\tmsg.firstChild.data = 'This receiver has no Hybridcast engine (AITControlledApp.Start unsupported)';\n\t}}\n}}\n]]>\n</script>\n</head>\n<body id=\"body\" onload=\"init();\">\n\t<div id=\"main\" style=\"left:0px; top:0px; width:{w}px; height:{h}px\">\n\t\t<object id=\"Vstream\" type=\"video/X-arib-mpeg2\" data=\"/-1\" remain=\"remain\" style=\"left:0px; top:0px; width:{w}px; height:{h}px;\"/>\n\t\t<object id=\"Astream\" type=\"audio/X-arib-mpeg2-aac\" data=\"/-1\" remain=\"remain\" streamstatus=\"play\"/>\n\t</div>\n\t<p id=\"msg\" style=\"top:{py}px;left:40px;width:{pw}px;height:60px;font-size:24px;line-height:30px;color-index:7;background-color-index:0;\">Hybridcast launcher: checking the receiver</p>\n</body>\n</bml>\n",
        org = cfg.organization_id,
        app = cfg.application_id,
        py = h.parse::<u32>().unwrap_or(540).saturating_sub(80),
        pw = w.parse::<u32>().unwrap_or(960).saturating_sub(80),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_split() {
        let u = split_url("https://example.com/hybrid/appsA/app1/index.html").unwrap();
        assert_eq!(u.base, "https://example.com/hybrid/appsA/app1/");
        assert_eq!(u.initial_path, "index.html");
        assert_eq!(u.origin, "https://example.com");
        let u = split_url("http://host:8080").unwrap();
        assert_eq!(u.base, "http://host:8080/");
        assert_eq!(u.initial_path, "");
        assert!(split_url("ftp://x/y").is_err());
        assert!(split_url("example.com/x").is_err());
        assert!(split_url("http://ex ample.com/x").is_err());
    }
}
