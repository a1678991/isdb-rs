//! Real captures (opt-in; no capture is committed). Each test reads the
//! file named by an environment variable and prints a `SKIP` line and
//! passes when the variable is unset or the file is missing:
//! * `ISDB_RS_CAPTURE_TS` — a descrambled live ISDB-T terrestrial capture
//!   (null packets may be stripped);
//! * `ISDB_RS_OUR_TS` — a transport stream written by this project's
//!   multiplexer (for the NIT transplant and the ours-vs-capture diff).
//!
//! ```text
//! ISDB_RS_CAPTURE_TS=/path/to/capture.ts ISDB_RS_OUR_TS=/path/to/ours.ts \
//!     cargo test -p isdb-analyze --test real_data -- --nocapture
//! ```
//!
//! Run with `--nocapture` to see the reports.

mod common;
use common::*;
use isdb_analyze::edit::{self, EditOp, EditOptions, NullSlots, Selector};
use isdb_analyze::mutate::Expr;
use isdb_analyze::validate::Severity;
use isdb_si::AnyTable;
use isdb_si::descriptor::Descriptor;
use isdb_ts::psi::Pat;
use isdb_ts::section::Long;
use std::path::PathBuf;

/// Descrambled live terrestrial capture.
const CAPTURE: &str = "ISDB_RS_CAPTURE_TS";
/// Transport stream written by this project's multiplexer.
const OURS: &str = "ISDB_RS_OUR_TS";

fn path(var: &str) -> Option<PathBuf> {
    let Some(p) = std::env::var_os(var).map(PathBuf::from) else {
        eprintln!("SKIP: {var} not set");
        return None;
    };
    if !p.is_file() {
        eprintln!("SKIP: {var}={} missing", p.display());
        return None;
    }
    Some(p)
}

fn load(var: &str) -> Option<(PathBuf, Vec<Pkt>)> {
    let p = path(var)?;
    let (pk, _) = isdb_analyze::scan::read_packets(&p).unwrap();
    Some((p, pk))
}

/// The PAT of `p` and the PMT PIDs it lists.
fn pat(p: &[Pkt]) -> (Long<Pat>, Vec<u16>) {
    let sec = edit::assemble_pid(p, 0x0000)
        .into_iter()
        .find(|o| o.bytes.first() == Some(&0x00))
        .expect("a PAT in the capture");
    let pat = Long::<Pat>::decode(&sec.bytes).unwrap();
    let pmts = pat.body.programs().map(|(_, pid)| pid).collect();
    (pat, pmts)
}

#[test]
fn capture_inspect_decodes_everything() {
    let Some((path, _)) = load(CAPTURE) else {
        return;
    };
    let cap = isdb_analyze::scan_path(&path, &Default::default()).unwrap();
    let r = isdb_analyze::inspect::inspect(
        &cap,
        &isdb_analyze::inspect::InspectOptions {
            decode_sections: true,
            nominal_bps: None,
        },
    );
    println!("{}", r.to_text());
    assert_eq!(r.section_error_count, 0);
    assert!(r.sections.iter().all(|s| s.decode_error.is_none()));
    // every SI section is typed (no RawSection); every descriptor typed
    // except those B10 leaves to other standards (0xC9/0xCB/0xCC, see
    // isdb-si SPEC-NOTES "descriptors and tables defined outside B10").
    fn walk(n: &isdb_analyze::tree::Node, bad: &mut Vec<String>) {
        use isdb_analyze::tree::{Node, get};
        match n {
            Node::Struct(name, f) => {
                let tag = get(n, "tag")
                    .map(|t| isdb_analyze::tree::scalar(t, "tag"))
                    .unwrap_or_default();
                if name == "RawSection"
                    || name == "Undecodable"
                    || (name == "Unknown" && !matches!(tag.as_str(), "0xc9" | "0xcb" | "0xcc"))
                {
                    bad.push(format!("{name} {tag}"));
                }
                f.iter().for_each(|(_, v)| walk(v, bad));
            }
            Node::List(v) => v.iter().for_each(|x| walk(x, bad)),
            _ => {}
        }
    }
    for s in &r.sections {
        let mut bad = Vec::new();
        walk(s.decoded.as_ref().unwrap(), &mut bad);
        assert!(bad.is_empty(), "{}: {bad:?}", s.key);
    }
    // A terrestrial NIT names the network and carries the TS information
    // and terrestrial delivery system descriptors (B10 Part 2 Table 6-1,
    // TR-B14); SDT names the services; EIT p/f events have titles.
    assert!(r.si.transport_stream_id.is_some());
    assert!(
        r.si.networks[0]
            .name
            .as_deref()
            .is_some_and(|n| !n.is_empty())
    );
    let ts = &r.si.networks[0].transport_streams[0];
    let ts_name = &ts.ts_info.as_ref().unwrap().ts_name;
    assert!(!ts_name.is_empty());
    assert!(!ts.delivery.as_ref().unwrap().guard_interval_name.is_empty());
    assert!(!r.si.services.is_empty());
    assert!(r.si.services.iter().any(|s| s.name.is_some()));
    assert!(r.si.present_following.iter().all(|e| e.title.is_some()));
    assert!(r.si.time.as_ref().unwrap().tot.is_some());
    // JSON serialises (with the decoded strings)
    assert!(
        serde_json::to_string(&r)
            .unwrap()
            .contains(ts_name.as_str())
    );
}

#[test]
fn capture_validate_findings() {
    let Some((path, _)) = load(CAPTURE) else {
        return;
    };
    let cap = isdb_analyze::scan_path(&path, &Default::default()).unwrap();
    let r = isdb_analyze::validate::validate(&cap, &Default::default());
    println!("{}", r.to_text(Severity::Info));
    // Live operational practice (e.g. PCR intervals) is classified and
    // printed, not asserted. Sections and continuity must be clean.
    assert!(
        !r.findings
            .iter()
            .any(|f| f.check.starts_with("section.") && f.severity == Severity::Error)
    );
    assert!(
        !r.findings
            .iter()
            .any(|f| f.check == "transport.cc" && f.severity == Severity::Error)
    );
    // Live AAC (B32 Part 2 §5.2) violates no rule.
    assert!(
        !r.findings
            .iter()
            .any(|f| f.check.starts_with("aac.") && f.severity > Severity::Info),
        "{}",
        r.to_text(Severity::Warning)
    );
}

#[test]
fn capture_identity_rewrite_keeps_sections() {
    let Some((_, p)) = load(CAPTURE) else { return };
    let (_, pmts) = pat(&p);
    for pid in [
        0x0000u16, 0x0001, 0x0010, 0x0011, 0x0012, 0x0014, 0x0024, 0x0027,
    ]
    .into_iter()
    .chain(pmts)
    {
        let mut q = p.clone();
        let r =
            edit::rewrite_pid(&mut q, pid, |_| Ok(None), vec![], NullSlots::WhenNeeded).unwrap();
        let a: Vec<Vec<u8>> = edit::assemble_pid(&p, pid)
            .into_iter()
            .map(|o| o.bytes)
            .collect();
        let b: Vec<Vec<u8>> = edit::assemble_pid(&q, pid)
            .into_iter()
            .map(|o| o.bytes)
            .collect();
        assert_eq!(a, b, "pid {pid:#06x}");
        let same = q.iter().zip(&p).filter(|(x, y)| x == y).count();
        println!(
            "pid {pid:#06x}: {} sections, packets byte-identical {same}/{} {r:?}",
            a.len(),
            p.len()
        );
    }
}

#[test]
fn capture_edit_round_trips() {
    let Some((_, p)) = load(CAPTURE) else { return };
    let base = scan(&p);
    let (pat, pmts) = pat(&p);
    // null an elementary PID of the first PMT: gone, positions kept
    let pmt = edit::assemble_pid(&p, pmts[0])
        .into_iter()
        .find(|o| o.bytes.first() == Some(&0x02))
        .unwrap();
    let es = Long::<isdb_ts::psi::Pmt>::decode(&pmt.bytes)
        .unwrap()
        .body
        .streams[0]
        .elementary_pid;
    let mut q = p.clone();
    edit::apply(
        &mut q,
        &[EditOp::NullPids(vec![es])],
        &EditOptions::default(),
    )
    .unwrap();
    let c = scan(&q);
    assert!(!c.stats.pids.contains_key(&es));
    assert_eq!(c.packets, base.packets);
    // NIT field mutation (guard_interval to a different value): the diff
    // shows exactly that field
    let tsid = pat.header.table_id_extension;
    let nit = edit::assemble_pid(&p, 0x0010)
        .into_iter()
        .find(|o| o.bytes.first() == Some(&0x40))
        .unwrap();
    let AnyTable::Nit(nit) = AnyTable::decode(0x0010, &nit.bytes).unwrap() else {
        panic!()
    };
    let gi = nit
        .body
        .0
        .transport_streams
        .iter()
        .find(|t| t.transport_stream_id == tsid)
        .unwrap()
        .descriptors
        .iter()
        .find_map(|d| match d {
            Descriptor::TerrestrialDeliverySystem(t) => Some(t.guard_interval),
            _ => None,
        })
        .unwrap();
    let expr = format!("nit.ts[{tsid:#06x}].desc[0xFA].guard_interval={}", gi ^ 1);
    let mut q = p.clone();
    edit::apply(
        &mut q,
        &[EditOp::Mutate(Expr::parse(&expr).unwrap())],
        &EditOptions::default(),
    )
    .unwrap();
    let c = scan(&q);
    let d = isdb_analyze::diff::diff(&base, &c, &Default::default());
    let changed: Vec<_> = d.sections.iter().filter(|s| !s.identical).collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].changes.len(), 1, "{:?}", changed[0].changes);
    assert!(changed[0].changes[0].path.ends_with("guard_interval"));
    let r = isdb_analyze::validate::validate(&c, &Default::default());
    assert!(
        !r.findings.iter().any(
            |f| (f.check.starts_with("section.") || f.check == "transport.cc")
                && f.severity == Severity::Error
        ),
        "{}",
        r.to_text(Severity::Error)
    );
}

#[test]
fn transplant_nit_between_captures_is_crc_and_cc_clean() {
    let (Some((pa, _)), Some((_, pb))) = (load(OURS), load(CAPTURE)) else {
        return;
    };
    let sel = Selector::parse("0x10:0x40").unwrap();
    let nit_a = edit::load_sections(&pa, &sel).unwrap();
    let mut q = pb.clone();
    let rep = edit::apply(
        &mut q,
        &[EditOp::Replace {
            sel,
            sections: nit_a.clone(),
        }],
        &EditOptions::default(),
    )
    .unwrap();
    println!("{:?}", rep);
    let c = scan(&q);
    assert_eq!(
        c.slots
            .iter()
            .find(|(k, _)| k.table_id == 0x40)
            .unwrap()
            .1
            .latest,
        nit_a[0]
    );
    let r = isdb_analyze::validate::validate(&c, &Default::default());
    let bad: Vec<_> = r
        .findings
        .iter()
        .filter(|f| {
            f.check.starts_with("section.crc")
                || f.check == "section.assembly"
                || (f.check == "transport.cc" && f.severity == Severity::Error)
        })
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}

#[test]
fn ours_vs_capture() {
    let (Some(pa), Some(pb)) = (path(OURS), path(CAPTURE)) else {
        return;
    };
    let a = isdb_analyze::scan_path(&pa, &Default::default()).unwrap();
    let b = isdb_analyze::scan_path(&pb, &Default::default()).unwrap();
    let ra = isdb_analyze::inspect::inspect(&a, &Default::default());
    println!("{}", ra.to_text());
    let va = isdb_analyze::validate::validate(&a, &Default::default());
    println!("{}", va.to_text(Severity::Info));
    let d = isdb_analyze::diff::diff(&a, &b, &Default::default());
    println!("{}", d.to_text(false));
    assert!(a.section_error_count == 0);
}
