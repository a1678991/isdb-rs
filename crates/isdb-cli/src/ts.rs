//! `isdb ts` — transport stream analysis and experiment editing, on top of
//! `isdb-analyze` (decoders only: H.222.0 via isdb-ts, ARIB STD-B10 via
//! isdb-si, STD-B24 strings via isdb-text).
//!
//! * `inspect FILE` — packets, PIDs, continuity, PCR timing, programs/PES,
//!   every section with repetition statistics, decoded SI (text or JSON).
//! * `validate FILE` — conformance findings with clause citations; exits
//!   non-zero when a finding at `--fail-on` severity (default error) exists.
//! * `diff A B` — table-level/field-level comparison and PID inventory.
//! * `edit IN OUT` — null PIDs, remove/replace/insert sections, typed field
//!   mutation; output keeps packet positions, CRCs and continuity.
//! * `extract FILE` — sections to a hex fixture (`pid tid hex` per line).

use anyhow::{Context, Result, anyhow, bail};
use isdb_analyze::edit::{self, EditOp, EditOptions, NullSlots, Selector};
use isdb_analyze::mutate::Expr;
use isdb_analyze::validate::{Expectations, Severity, ValidateOptions};
use isdb_analyze::{ScanOptions, inspect, scan_path};
use isdb_ts::Pid;
use std::path::PathBuf;
use tracing::{info, warn};

fn int(s: &str) -> Result<u64, isdb_analyze::mutate::MutateError> {
    isdb_analyze::mutate::int(s)
}

fn parse_u16(s: &str) -> Result<u16, String> {
    int(s)
        .map_err(|e| e.to_string())
        .and_then(|v| u16::try_from(v).map_err(|_| format!("{v} does not fit 16 bits")))
}

fn parse_u8(s: &str) -> Result<u8, String> {
    int(s)
        .map_err(|e| e.to_string())
        .and_then(|v| u8::try_from(v).map_err(|_| format!("{v} does not fit 8 bits")))
}

/// A 13-bit PID (H.222.0 Table 2-3), decimal or 0x hex.
fn parse_pid(s: &str) -> Result<Pid, String> {
    isdb_analyze::mutate::pid(s).map_err(|e| e.to_string())
}

fn parse_pid_list(s: &str) -> Result<Vec<Pid>, String> {
    s.split(',')
        .filter(|x| !x.trim().is_empty())
        .map(|x| parse_pid(x.trim()))
        .collect()
}

/// Severity threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Level {
    /// Everything.
    Info,
    /// Warnings and errors.
    Warning,
    /// Errors only.
    Error,
    /// Never (validate always exits 0).
    Never,
}

impl Level {
    fn severity(self) -> Option<Severity> {
        match self {
            Level::Info => Some(Severity::Info),
            Level::Warning => Some(Severity::Warning),
            Level::Error => Some(Severity::Error),
            Level::Never => None,
        }
    }
}

/// Null-slot policy for section rewriting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Slots {
    /// Only the PID's original packet slots.
    Never,
    /// Null packets once behind the original schedule, and for insertions.
    Needed,
    /// Any null packet as soon as data is pending.
    Always,
}

/// Common reading options.
#[derive(clap::Args, Debug, Clone)]
pub struct ReadOpts {
    /// Stop after this many packets.
    #[arg(long)]
    pub max_packets: Option<u64>,
    /// Nominal bit rate (bit/s) for timing when the stream has no PCR.
    #[arg(long)]
    pub bitrate: Option<f64>,
}

/// `isdb ts inspect`.
#[derive(clap::Args, Debug)]
pub struct InspectArgs {
    /// Transport stream (188/192/204-byte packets, autodetected).
    pub file: PathBuf,
    /// JSON output.
    #[arg(long)]
    pub json: bool,
    /// Include the decoded field tree of every section.
    #[arg(long)]
    pub decode: bool,
    #[command(flatten)]
    pub read: ReadOpts,
}

/// `isdb ts validate`.
#[derive(clap::Args, Debug)]
pub struct ValidateArgs {
    /// Transport stream.
    pub file: PathBuf,
    /// JSON output.
    #[arg(long)]
    pub json: bool,
    /// Lowest severity to print.
    #[arg(long, value_enum, default_value_t = Level::Warning)]
    pub show: Level,
    /// Exit non-zero if a finding of at least this severity exists.
    #[arg(long, value_enum, default_value_t = Level::Error)]
    pub fail_on: Level,
    #[command(flatten)]
    pub read: ReadOpts,
    /// Expected transport_stream_id (PAT, SDT/EIT actual).
    #[arg(long, value_parser = parse_u16)]
    pub expect_tsid: Option<u16>,
    /// Expected original_network_id (SDT, NIT TS loop, EIT, BIT).
    #[arg(long, value_parser = parse_u16)]
    pub expect_onid: Option<u16>,
    /// Expected NIT actual network_id.
    #[arg(long, value_parser = parse_u16)]
    pub expect_nid: Option<u16>,
    /// Expected service ids (PAT programs and SDT services), comma separated.
    #[arg(long, value_parser = parse_u16, value_delimiter = ',', num_args = 1..)]
    pub expect_services: Option<Vec<u16>>,
    /// Frequency (MHz) the NIT terrestrial delivery descriptor must list.
    #[arg(long)]
    pub expect_freq_mhz: Option<f64>,
    /// Expected guard_interval code (0=1/32, 1=1/16, 2=1/8, 3=1/4; B10 Table 6-62).
    #[arg(long, value_parser = parse_u8)]
    pub expect_guard: Option<u8>,
    /// Expected transmission_mode code (0..2 = Mode 1..3; B10 Table 6-63).
    #[arg(long, value_parser = parse_u8)]
    pub expect_mode: Option<u8>,
    /// Expected area_code.
    #[arg(long, value_parser = parse_u16)]
    pub expect_area: Option<u16>,
    /// Expected remote_control_key_id.
    #[arg(long, value_parser = parse_u8)]
    pub expect_remote_key: Option<u8>,
    /// Expected ts_name (TS_information_descriptor).
    #[arg(long)]
    pub expect_ts_name: Option<String>,
    /// Expected network name (NIT network_name_descriptor).
    #[arg(long)]
    pub expect_network_name: Option<String>,
}

/// `isdb ts diff`.
#[derive(clap::Args, Debug)]
pub struct DiffArgs {
    /// First transport stream (e.g. ours).
    pub a: PathBuf,
    /// Second transport stream (e.g. a live station capture).
    pub b: PathBuf,
    /// JSON output.
    #[arg(long)]
    pub json: bool,
    /// List every EIT schedule section instead of a per-sub_table summary.
    #[arg(long)]
    pub all: bool,
    #[command(flatten)]
    pub read: ReadOpts,
}

/// `isdb ts edit`.  Operations run in this order: --null-pid, --remove,
/// --replace, --set, --insert.
#[derive(clap::Args, Debug)]
// The `[...]` below are optional-argument syntax that clap prints verbatim in
// `--help`; escaping them for rustdoc would change the help text.
#[allow(rustdoc::broken_intra_doc_links)]
pub struct EditArgs {
    /// Input transport stream.
    pub input: PathBuf,
    /// Output transport stream (188-byte packets).
    pub output: PathBuf,
    /// Replace every packet of these PIDs with null packets (comma list, repeatable).
    #[arg(long, value_parser = parse_pid_list)]
    pub null_pid: Vec<Vec<Pid>>,
    /// Remove sections: PID:TID[:EXT[:SN]].
    #[arg(long)]
    pub remove: Vec<String>,
    /// Replace sections: PID:TID[:EXT[:SN]]=FILE (FILE: .ts capture or hex fixture;
    /// sections matching the selector are taken from it, matched by section_number).
    #[arg(long)]
    pub replace: Vec<String>,
    /// Typed field mutation, e.g. `nit.ts[0].desc[0xFA].guard_interval=2`,
    /// `nit.version=24`, `pat.add=0x0e28:0x1fff` (see isdb_analyze::mutate).
    #[arg(long = "set")]
    pub set: Vec<String>,
    /// Insert sections into null-packet slots: PID:TID[:EXT[:SN]]=FILE@INTERVAL_MS
    /// (sections matching the selector are read from FILE and sent together every
    /// INTERVAL_MS, timed by PCR).
    #[arg(long)]
    pub insert: Vec<String>,
    /// First insertion time (ms from the start).
    #[arg(long, default_value_t = 0.0)]
    pub insert_offset_ms: f64,
    /// Use of null packets when rewritten sections need more room.
    #[arg(long, value_enum, default_value_t = Slots::Needed)]
    pub null_slots: Slots,
    /// Nominal bit rate (bit/s) for timing insertions when there is no PCR.
    #[arg(long)]
    pub bitrate: Option<f64>,
    /// Print the edit report as JSON.
    #[arg(long)]
    pub json: bool,
}

/// `isdb ts extract`.
#[derive(clap::Args, Debug)]
pub struct ExtractArgs {
    /// Transport stream.
    pub file: PathBuf,
    /// Only this PID.
    #[arg(long, value_parser = parse_pid)]
    pub pid: Option<Pid>,
    /// Only this table_id.
    #[arg(long, value_parser = parse_u8)]
    pub tid: Option<u8>,
    /// Every distinct body of every version (default: latest per section).
    #[arg(long)]
    pub all_versions: bool,
    /// Output file (default: stdout).
    #[arg(short, long)]
    pub output: Option<PathBuf>,
}

/// TS tool subcommands.
#[derive(clap::Subcommand, Debug)]
pub enum TsCmd {
    /// Full report: packets, PIDs, CC, PCR, programs/PES, sections, decoded SI.
    Inspect(InspectArgs),
    /// Spec conformance findings with clause citations (non-zero exit on errors).
    Validate(ValidateArgs),
    /// Compare two captures at table/field level, with PID inventory.
    Diff(DiffArgs),
    /// Experiment transforms producing a valid TS.
    Edit(EditArgs),
    /// Extract sections to a hex fixture (`pid tid hex` per line).
    Extract(ExtractArgs),
}

/// `isdb ts` arguments.
#[derive(clap::Args, Debug)]
pub struct TsArgs {
    #[command(subcommand)]
    pub cmd: TsCmd,
}

fn scan(file: &std::path::Path, read: &ReadOpts) -> Result<isdb_analyze::Capture> {
    scan_path(
        file,
        &ScanOptions {
            max_packets: read.max_packets,
            ..Default::default()
        },
    )
    .with_context(|| format!("reading {}", file.display()))
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

fn load_for(spec: &str) -> Result<(Selector, Vec<Vec<u8>>, &str)> {
    let (sel, rest) = spec
        .split_once('=')
        .ok_or_else(|| anyhow!("expected SELECTOR=FILE…: {spec:?}"))?;
    let sel = Selector::parse(sel)?;
    let file = rest.split('@').next().unwrap_or(rest);
    let sections = edit::load_sections(std::path::Path::new(file), &sel)?;
    Ok((sel, sections, rest))
}

/// Run `isdb ts`.
pub fn run(args: TsArgs) -> Result<()> {
    match args.cmd {
        TsCmd::Inspect(a) => {
            let cap = scan(&a.file, &a.read)?;
            let r = inspect::inspect(
                &cap,
                &inspect::InspectOptions {
                    decode_sections: a.decode || a.json,
                    nominal_bps: a.read.bitrate,
                },
            );
            if a.json {
                print_json(&r)
            } else {
                print!("{}", r.to_text());
                Ok(())
            }
        }
        TsCmd::Validate(a) => {
            let cap = scan(&a.file, &a.read)?;
            let expect = Expectations {
                transport_stream_id: a.expect_tsid,
                original_network_id: a.expect_onid,
                network_id: a.expect_nid,
                service_ids: a.expect_services,
                frequency_mhz: a.expect_freq_mhz,
                guard_interval: a.expect_guard,
                transmission_mode: a.expect_mode,
                area_code: a.expect_area,
                remote_control_key_id: a.expect_remote_key,
                ts_name: a.expect_ts_name,
                network_name: a.expect_network_name,
            };
            let r = isdb_analyze::validate::validate(
                &cap,
                &ValidateOptions {
                    expect,
                    nominal_bps: a.read.bitrate,
                },
            );
            if a.json {
                print_json(&r)?;
            } else {
                print!(
                    "{}",
                    r.to_text(a.show.severity().unwrap_or(Severity::Error))
                );
            }
            if let Some(t) = a.fail_on.severity() {
                let n = r.findings.iter().filter(|f| f.severity >= t).count();
                if n > 0 {
                    bail!("{n} finding(s) at or above {t}");
                }
            }
            Ok(())
        }
        TsCmd::Diff(a) => {
            let ca = scan(&a.a, &a.read)?;
            let cb = scan(&a.b, &a.read)?;
            let r = isdb_analyze::diff::diff(
                &ca,
                &cb,
                &isdb_analyze::diff::DiffOptions {
                    nominal_bps: a.read.bitrate,
                },
            );
            if a.json {
                print_json(&r)
            } else {
                print!("{}", r.to_text(a.all));
                Ok(())
            }
        }
        TsCmd::Edit(a) => {
            let (mut pkts, fmt) = isdb_analyze::scan::read_packets(&a.input)
                .with_context(|| format!("reading {}", a.input.display()))?;
            if pkts.is_empty() {
                bail!("{}: no transport packets", a.input.display());
            }
            if let Some(unit) = fmt.map(|f| f.size()).filter(|&n| n != 188) {
                info!(
                    unit_bytes = unit,
                    "note: input has {unit}-byte units; output is 188-byte packets"
                );
            }
            let mut ops = Vec::new();
            let nulls: Vec<Pid> = a.null_pid.into_iter().flatten().collect();
            if !nulls.is_empty() {
                ops.push(EditOp::NullPids(nulls));
            }
            for s in &a.remove {
                ops.push(EditOp::Remove(
                    Selector::parse(s).with_context(|| format!("--remove {s}"))?,
                ));
            }
            for s in &a.replace {
                let (sel, sections, _) = load_for(s).with_context(|| format!("--replace {s}"))?;
                ops.push(EditOp::Replace { sel, sections });
            }
            for s in &a.set {
                ops.push(EditOp::Mutate(
                    Expr::parse(s).with_context(|| format!("--set {s}"))?,
                ));
            }
            for s in &a.insert {
                let (sel, sections, rest) = load_for(s).with_context(|| format!("--insert {s}"))?;
                let ms = rest
                    .rsplit_once('@')
                    .ok_or_else(|| anyhow!("--insert needs @INTERVAL_MS: {s:?}"))?
                    .1;
                let interval: f64 = ms.parse().with_context(|| format!("interval {ms:?}"))?;
                ops.push(EditOp::Insert {
                    pid: sel.pid,
                    sections,
                    interval_s: interval / 1000.0,
                    offset_s: a.insert_offset_ms / 1000.0,
                });
            }
            if ops.is_empty() {
                bail!("no edit operation given");
            }
            let policy = match a.null_slots {
                Slots::Never => NullSlots::Never,
                Slots::Needed => NullSlots::WhenNeeded,
                Slots::Always => NullSlots::Always,
            };
            let rep = edit::apply(
                &mut pkts,
                &ops,
                &EditOptions {
                    null_slots: policy,
                    nominal_bps: a.bitrate,
                },
            )
            .with_context(|| format!("editing {}", a.input.display()))?;
            edit::write_packets(&a.output, &pkts)?;
            if a.json {
                print_json(&rep)?;
            } else {
                for r in &rep {
                    info!(op = %r.op, "{}", r.op);
                    if r.nulled_packets > 0 {
                        info!(
                            packets = r.nulled_packets,
                            "  nulled {} packets", r.nulled_packets
                        );
                    }
                    for w in &r.rewrites {
                        info!(
                            pid = %w.pid,
                            sections_in = w.sections_in,
                            changed = w.changed,
                            removed = w.removed,
                            inserted = w.inserted,
                            sections_out = w.sections_out,
                            dropped = w.dropped,
                            original_slots = w.original_slots,
                            slots_nulled = w.slots_nulled,
                            null_slots_used = w.null_slots_used,
                            max_delay_packets = w.max_delay_packets,
                            "  pid {:#06x}: {} sections in, {} changed, {} removed, {} inserted, {} out, {} DROPPED; slots {} original ({} now null), {} null used, max delay {} packets",
                            w.pid,
                            w.sections_in,
                            w.changed,
                            w.removed,
                            w.inserted,
                            w.sections_out,
                            w.dropped,
                            w.original_slots,
                            w.slots_nulled,
                            w.null_slots_used,
                            w.max_delay_packets
                        );
                    }
                }
            }
            let dropped: usize = rep
                .iter()
                .flat_map(|r| r.rewrites.iter())
                .map(|w| w.dropped)
                .sum();
            if dropped > 0 {
                warn!(
                    dropped,
                    "{dropped} section(s) did not fit before the end of the stream (try --null-slots always)"
                );
            }
            info!(
                packets = pkts.len(),
                path = %a.output.display(),
                "wrote {} packets to {}",
                pkts.len(),
                a.output.display()
            );
            Ok(())
        }
        TsCmd::Extract(a) => {
            let cap = scan(
                &a.file,
                &ReadOpts {
                    max_packets: None,
                    bitrate: None,
                },
            )?;
            let text = edit::extract(&cap, a.pid, a.tid, a.all_versions);
            match a.output {
                Some(p) => {
                    std::fs::write(&p, text).with_context(|| format!("write {}", p.display()))
                }
                None => {
                    print!("{text}");
                    Ok(())
                }
            }
        }
    }
}
