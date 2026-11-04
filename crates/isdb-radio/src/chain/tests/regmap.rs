//! The host's register use against the chain's one register map,
//! `fpga/isdb_fpga/chain/registers.toml` (issue #30 item 5), on the mock
//! register backend ([`Model`], its [`Model::log`]):
//!
//! - the open sequence, on images with and without each FEATURES flag it
//!   depends on and with and without the host's start values, reads and
//!   writes exactly the map's `[[host.open]]` steps, in order, with their
//!   values (the front end's configuration a step of its own);
//! - a tick of the slow loop's register path (arming a capture, waiting,
//!   reading and solving; with and without FEATURES SUMS_CAPTURE), the
//!   stream path's solve, the error report and release are the map's
//!   `tick`, `arm`, `solve`, `stream_solve`, `state` and `release` steps;
//! - over all of these, the host writes no register the map calls read
//!   only, unless the image has the register's `write_feature` or the map
//!   records the write as one the image ignores with that value;
//! - the registers the host reads and writes are exactly the map's `host`
//!   column; the table, with the registers the host never touches, is
//!   printed.
//!
//! Skips (with a message) without the map: a packaged crate has no
//! `fpga/`.

use super::*;
use std::collections::BTreeSet;
use toml::{Table, Value};

const MAP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fpga/isdb_fpga/chain/registers.toml"
);

struct Reg {
    name: String,
    address: u16,
    count: u16,
    access: String,
    host: String,
    write_feature: String,
    ignored_write: Option<u32>,
    resets: Vec<u32>,
}

struct Map {
    window: (u16, u16),
    regs: Vec<Reg>,
    /// FEATURES flags: name → bit value.
    features: BTreeMap<String, u32>,
    /// INIT bits: name → bit value.
    init: BTreeMap<String, u32>,
    ctrl: BTreeMap<String, u32>,
    frontend: BTreeMap<String, u16>,
    host: Table,
}

fn int(v: &Value) -> u32 {
    u32::try_from(v.as_integer().expect("an integer")).expect("32 bits")
}

fn s<'a>(t: &'a Table, k: &str) -> &'a str {
    t.get(k).and_then(Value::as_str).unwrap_or("")
}

fn tables(v: &Value) -> impl Iterator<Item = &Table> {
    v.as_array()
        .expect("an array of tables")
        .iter()
        .map(|t| t.as_table().expect("a table"))
}

fn flags(r: &Table) -> BTreeMap<String, u32> {
    tables(&r["field"])
        .filter(|f| f.get("width").is_none_or(|w| int(w) == 1))
        .map(|f| (s(f, "name").to_owned(), 1 << int(&f["lsb"])))
        .collect()
}

fn load() -> Option<Map> {
    let Ok(text) = std::fs::read_to_string(MAP) else {
        println!("SKIP: {MAP} not found (a packaged crate)");
        return None;
    };
    let m: Table = text.parse().expect("registers.toml parses");
    let raw: Vec<&Table> = tables(&m["register"]).collect();
    let find = |n: &str| *raw.iter().find(|r| s(r, "name") == n).unwrap();
    let regs = raw
        .iter()
        .map(|r| {
            let count = r.get("count").map_or(1, int);
            Reg {
                name: s(r, "name").to_owned(),
                address: u16::try_from(int(&r["address"])).unwrap(),
                count: u16::try_from(count).unwrap(),
                access: s(r, "access").to_owned(),
                host: s(r, "host").to_owned(),
                write_feature: s(r, "write_feature").to_owned(),
                ignored_write: r.get("ignored_write").map(int),
                resets: match &r["reset"] {
                    Value::Array(a) => a.iter().map(int).collect(),
                    v => vec![int(v); count as usize],
                },
            }
        })
        .collect();
    let w: Vec<u32> = m["window"].as_array().unwrap().iter().map(int).collect();
    Some(Map {
        window: (u16::try_from(w[0]).unwrap(), u16::try_from(w[1]).unwrap()),
        regs,
        features: flags(find("FEATURES")),
        init: flags(find("INIT")),
        ctrl: tables(&m["ctrl"])
            .map(|c| (s(c, "name").to_owned(), 1 << int(&c["bit"])))
            .collect(),
        frontend: tables(&m["frontend"])
            .map(|f| {
                (
                    s(f, "name").to_owned(),
                    u16::try_from(int(&f["address"])).unwrap(),
                )
            })
            .collect(),
        host: m["host"].as_table().unwrap().clone(),
    })
}

impl Map {
    fn reg(&self, name: &str) -> &Reg {
        self.regs
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no register {name}"))
    }

    fn at(&self, addr: u16) -> Option<&Reg> {
        self.regs
            .iter()
            .find(|r| (r.address..r.address + r.count).contains(&addr))
    }

    fn in_window(&self, addr: u16) -> bool {
        (self.window.0..self.window.1).contains(&addr)
    }

    /// The addresses a step names: a chain register's elements, or a front
    /// end register (`fe.NAME`).
    fn addresses(&self, name: &str) -> Vec<u16> {
        if let Some(fe) = name.strip_prefix("fe.") {
            return vec![self.frontend[fe]];
        }
        let r = self.reg(name);
        (r.address..r.address + r.count).collect()
    }
}

/// An expected access, or the front end's configuration (a run of writes
/// outside the chain's window).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Want {
    Log(Logged),
    FrontEnd,
}

impl std::fmt::Debug for Want {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Log(Logged::Read(a)) => write!(f, "read {a:#05x}"),
            Self::Log(Logged::Write(a, v)) => write!(f, "write {a:#05x} = {v:#x}"),
            Self::Log(Logged::Ctrl(c)) => write!(f, "ctrl {c:#x}"),
            Self::FrontEnd => write!(f, "front end configured"),
        }
    }
}

/// What a sequence's values resolve against.
struct Values<'a> {
    features: u32,
    init: &'a ChainInit,
    /// The solved taps and their frame.
    solved: Option<(Taps, u32)>,
}

/// The map's sequence `seq` for these values.
fn expected(map: &Map, seq: &str, v: &Values) -> Vec<Want> {
    let mut out = Vec::new();
    let mut loaded = 0u32;
    for step in tables(&map.host[seq]) {
        if let Some(f) = step.get("if").and_then(Value::as_str)
            && v.features & map.features[f] == 0
        {
            continue;
        }
        match s(step, "if_init") {
            "" => {}
            "constellation" if v.init.constellation.iter().any(Option::is_some) => {}
            "n0" if v.init.n0.is_some() => {}
            "constellation" | "n0" => continue,
            other => panic!("if_init {other}"),
        }
        if let Some(c) = step.get("ctrl") {
            let bits = c
                .as_array()
                .unwrap()
                .iter()
                .map(|b| map.ctrl[b.as_str().unwrap()])
                .sum();
            out.push(Want::Log(Logged::Ctrl(bits)));
        } else if step.contains_key("frontend") {
            out.push(Want::FrontEnd);
        } else if let Some(r) = step.get("read").and_then(Value::as_str) {
            out.extend(
                map.addresses(r)
                    .into_iter()
                    .map(|a| Want::Log(Logged::Read(a))),
            );
        } else if let Some(r) = step.get("write").and_then(Value::as_str) {
            let base = map.reg(r).address;
            let writes: Vec<(u16, u32)> = match &step["value"] {
                Value::Integer(_) => vec![(base, int(&step["value"]))],
                Value::String(name) => match name.as_str() {
                    "init.carrier" => vec![(base, v.init.carrier.cast_unsigned())],
                    "init.layout_lag" => vec![(base, v.init.layout_lag)],
                    "init.taps" => taps_words(base, &v.init.taps.unwrap_or_else(identity)),
                    "init.n0" => vec![(base, v.init.n0.unwrap())],
                    "init.constellation" => v
                        .init
                        .constellation
                        .iter()
                        .enumerate()
                        .filter_map(|(m, p)| p.as_ref().map(|p| (m, p)))
                        .flat_map(|(m, p)| {
                            p.iter().enumerate().map(move |(t, &q)| {
                                let a = base + u16::try_from(cc::MAX_POINTS * m + t).unwrap();
                                (a, pack_point(q))
                            })
                        })
                        .collect(),
                    "init.const_mask" => {
                        let mask = v
                            .init
                            .constellation
                            .iter()
                            .enumerate()
                            .filter(|(_, p)| p.is_some())
                            .map(|(m, _)| 1 << m)
                            .sum();
                        vec![(base, mask)]
                    }
                    "loaded" => vec![(base, loaded)],
                    "taps" => taps_words(base, &v.solved.expect("a solve").0),
                    "frame" => vec![(base, v.solved.expect("a solve").1)],
                    other => panic!("value {other}"),
                },
                other => panic!("value {other:?}"),
            };
            out.extend(
                writes
                    .into_iter()
                    .map(|(a, x)| Want::Log(Logged::Write(a, x))),
            );
            if let Some(b) = step.get("init_bit").and_then(Value::as_str) {
                loaded |= map.init[b];
            }
        } else {
            panic!("a step without read, write, ctrl or frontend: {step:?}");
        }
    }
    out
}

fn taps_words(base: u16, t: &Taps) -> Vec<(u16, u32)> {
    t.iter()
        .enumerate()
        .map(|(k, &p)| (base + u16::try_from(k).unwrap(), pack_point(p)))
        .collect()
}

/// The model's log with each run of front-end writes as one `FrontEnd`.
fn actual(map: &Map, log: &[Logged]) -> Vec<Want> {
    let mut out = Vec::new();
    for &l in log {
        let w = match l {
            Logged::Write(a, _) if !map.in_window(a) => Want::FrontEnd,
            l => Want::Log(l),
        };
        if !(w == Want::FrontEnd && out.last() == Some(&Want::FrontEnd)) {
            out.push(w);
        }
    }
    out
}

fn concat(map: &Map, seqs: &[&str], v: &Values) -> Vec<Want> {
    seqs.iter().flat_map(|q| expected(map, q, v)).collect()
}

/// One flow's accesses and the FEATURES of its image.
struct Flow {
    what: String,
    features: u32,
    log: Vec<Logged>,
}

fn take_log(m: &Mock) -> Vec<Logged> {
    std::mem::take(&mut m.get().log)
}

fn image(version: u16, features: u32) -> Mock {
    let m = Mock::new(Model::new(id_word(), version, features));
    m.get().gpio_out = 1;
    m
}

fn start_values() -> ChainInit {
    let mut taps = Taps::default();
    taps[5] = [16_384, 0];
    taps[6] = [-300, 77];
    let mut init = ChainInit {
        carrier: 5_678,
        taps: Some(taps),
        n0: Some(40_000),
        ..ChainInit::default()
    };
    init.constellation[3] = Some((0..16).map(|k| [k * 60 - 480, 5 - k]).collect());
    init.constellation[1] = Some((0..4).map(|k| [k * 100, -k]).collect());
    init
}

/// The open sequence on images with and without the flags it reads, with
/// and without the host's start values.
fn open_flows(map: &Map) -> Vec<Flow> {
    let mut flows = Vec::new();
    let nothing = ChainInit {
        carrier: -1_234,
        ..ChainInit::default()
    };
    let all_values = start_values();
    let cases: [(&str, u16, u32, &ChainInit); 5] = [
        (
            "every feature, nothing from the host",
            2,
            features::ALL,
            &nothing,
        ),
        (
            "every feature, the host's values",
            2,
            features::ALL,
            &all_values,
        ),
        (
            "no DECODER_HEALTH, CONSTELLATION or N0, the host's values",
            2,
            features::ALL & !(features::DECODER_HEALTH | features::CONSTELLATION | features::N0),
            &all_values,
        ),
        (
            "format 1, SUMS only, the host's values",
            1,
            features::SUMS,
            &all_values,
        ),
        (
            "the first image's flags (top.FEATURES), nothing from the host",
            2,
            map.features
                .iter()
                .filter(|(n, _)| !matches!(n.as_str(), "CONSTELLATION" | "N0"))
                .map(|(_, b)| b)
                .sum(),
            &nothing,
        ),
    ];
    for (what, version, feats, init) in cases {
        let m = image(version, feats);
        let mut c = ChainControl::attach("mock", &m).unwrap();
        c.open(&m, &params(), init).unwrap();
        let log = take_log(&m);
        let v = Values {
            features: feats,
            init,
            solved: None,
        };
        assert_eq!(
            actual(map, &log),
            expected(map, "open", &v),
            "open, {what} (features {feats:#x}): the accesses against registers.toml [[host.open]]"
        );
        flows.push(Flow {
            what: format!("open, {what}"),
            features: feats,
            log,
        });
    }
    flows
}

fn loop_opts() -> SlowLoopOptions {
    SlowLoopOptions {
        gate: None,
        check: None,
        ..SlowLoopOptions::new(fake_solve, 33.7561e6)
    }
}

/// The register path with the capture: arm, wait, read and solve; the
/// error report; release.
fn capture_flow(map: &Map) -> Vec<Flow> {
    let feats = features::ALL;
    let m = image(2, feats);
    let mut c = ChainControl::attach("mock", &m).unwrap();
    c.open(&m, &params(), &ChainInit::default()).unwrap();
    take_log(&m);
    let init = ChainInit::default();
    let v = |solved| Values {
        features: feats,
        init: &init,
        solved,
    };
    let mut l = SlowLoop::new(loop_opts());
    let mut log = Vec::new();
    // the first tick: FEATURES, the state, the capture armed
    m.get().frame_end(sums(40, 6_720));
    assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Capturing);
    let got = take_log(&m);
    let mut want = vec![Want::Log(Logged::Read(map.reg("FEATURES").address))];
    want.extend(concat(map, &["tick", "arm"], &v(None)));
    assert_eq!(actual(map, &got), want, "the first tick (capture armed)");
    log.extend(got);
    // waiting: the state only
    assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Capturing);
    let got = take_log(&m);
    assert_eq!(
        actual(map, &got),
        expected(map, "tick", &v(None)),
        "a waiting tick"
    );
    log.extend(got);
    // held: the sums read, solved and committed
    m.get().frame_end(sums(50, 6_720));
    m.get().frame_end(sums(51, 6_720));
    assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 50 });
    let got = take_log(&m);
    let solved = m.get().taps;
    assert_eq!(solved.unwrap().1, 50);
    assert_eq!(
        actual(map, &got),
        concat(map, &["tick", "solve"], &v(solved)),
        "a solving tick"
    );
    log.extend(got);
    let mut flows = vec![Flow {
        what: "slow loop, register path with the capture".into(),
        features: feats,
        log,
    }];
    // the error report, then release
    c.state(&m).unwrap();
    let got = take_log(&m);
    assert_eq!(
        actual(map, &got),
        expected(map, "state", &v(None)),
        "the error report"
    );
    flows.push(Flow {
        what: "error report".into(),
        features: feats,
        log: got,
    });
    c.release(&m).unwrap();
    let got = take_log(&m);
    assert_eq!(
        actual(map, &got),
        expected(map, "release", &v(None)),
        "release"
    );
    flows.push(Flow {
        what: "release".into(),
        features: feats,
        log: got,
    });
    flows
}

/// The register path of an image without the capture (the latch moves
/// every frame).
fn no_capture_flow(map: &Map) -> Flow {
    let feats = features::ALL & !features::SUMS_CAPTURE;
    let m = image(2, feats);
    let mut c = ChainControl::attach("mock", &m).unwrap();
    c.open(&m, &params(), &ChainInit::default()).unwrap();
    take_log(&m);
    let init = ChainInit::default();
    let mut l = SlowLoop::new(loop_opts());
    m.get().sums = Some(sums(42, 6_720));
    assert_eq!(l.tick(&mut c, &m).unwrap(), Tick::Solved { frame: 42 });
    let got = take_log(&m);
    let solved = m.get().taps;
    let v = Values {
        features: feats,
        init: &init,
        solved,
    };
    let mut want = vec![Want::Log(Logged::Read(map.reg("FEATURES").address))];
    want.extend(concat(map, &["tick", "solve"], &v));
    assert_eq!(
        actual(map, &got),
        want,
        "a solving tick without the capture"
    );
    Flow {
        what: "slow loop, register path without the capture".into(),
        features: feats,
        log: got,
    }
}

/// The stream path: the sums from a sums block, no register read.
fn stream_flow(map: &Map) -> Flow {
    let feats = features::ALL;
    let m = image(2, feats);
    let mut c = ChainControl::attach("mock", &m).unwrap();
    c.open(&m, &params(), &ChainInit::default()).unwrap();
    take_log(&m);
    let h = Arc::new(ChainHealth::new());
    let mut l = SlowLoop::new(SlowLoopOptions {
        health: Some(Arc::clone(&h)),
        sums_source: SumsSource::Stream,
        ..loop_opts()
    });
    let t0 = Instant::now();
    h.frame_at(
        FrameHealth {
            frame: 2,
            decoded: true,
            n0: 40_000,
            slots: 96,
            locked: true,
            ..FrameHealth::default()
        },
        t0,
    );
    let (f, n, w) = sums(2, 6_720);
    h.sums(cc::sums_from_words(f, n, &w));
    assert_eq!(
        l.tick_at(&mut c, &m, t0).unwrap(),
        Tick::Solved { frame: 2 }
    );
    let got = take_log(&m);
    let init = ChainInit::default();
    let v = Values {
        features: feats,
        init: &init,
        solved: m.get().taps,
    };
    assert_eq!(
        actual(map, &got),
        expected(map, "stream_solve", &v),
        "the stream path's solve"
    );
    Flow {
        what: "slow loop, stream path".into(),
        features: feats,
        log: got,
    }
}

fn all_flows(map: &Map) -> Vec<Flow> {
    let mut flows = open_flows(map);
    flows.extend(capture_flow(map));
    flows.push(no_capture_flow(map));
    flows.push(stream_flow(map));
    flows
}

/// The open sequence, the slow loop's ticks (both paths), the error report
/// and release: the map's steps, in order, with their values.
#[test]
fn the_host_sequences_are_the_maps() {
    let Some(map) = load() else { return };
    let flows = all_flows(&map);
    assert_eq!(flows.len(), 10);
}

/// No flow writes a register the map calls read only, except one the
/// image's FEATURES make writable (`write_feature`) or a write the map
/// records as ignored with that value (`ignored_write`); nothing is
/// written in the window outside the map's registers.
#[test]
fn the_host_writes_only_what_the_map_lets_it() {
    let Some(map) = load() else { return };
    let mut bad = Vec::new();
    for f in all_flows(&map) {
        for &l in &f.log {
            let Logged::Write(a, v) = l else { continue };
            if !map.in_window(a) {
                continue;
            }
            let Some(r) = map.at(a) else {
                bad.push(format!("{}: wrote {v:#x} to unmapped {a:#05x}", f.what));
                continue;
            };
            let allowed = r.access != "ro"
                || (!r.write_feature.is_empty()
                    && f.features & map.features[&r.write_feature] != 0)
                || r.ignored_write == Some(v);
            if !allowed {
                bad.push(format!(
                    "{}: wrote {v:#x} to {} ({a:#05x}, read only{})",
                    f.what,
                    r.name,
                    if r.write_feature.is_empty() {
                        String::new()
                    } else {
                        format!(" without FEATURES {}", r.write_feature)
                    }
                ));
            }
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}

/// The registers the host reads and writes over every flow are the map's
/// `host` column; the coverage table (with the registers the host never
/// touches) is printed.
#[test]
fn the_host_touches_the_maps_registers() {
    let Some(map) = load() else { return };
    let mut read = BTreeSet::new();
    let mut written = BTreeSet::new();
    for f in all_flows(&map) {
        for &l in &f.log {
            let (a, set) = match l {
                Logged::Read(a) => (a, &mut read),
                Logged::Write(a, _) => (a, &mut written),
                Logged::Ctrl(_) => continue,
            };
            if let Some(r) = map.at(a) {
                set.insert(r.name.clone());
            } else {
                assert!(
                    !map.in_window(a),
                    "{}: {a:#05x} is no register of the map",
                    f.what
                );
            }
        }
    }
    println!("the host's chain registers (registers.toml `host`; isdb_radio::chain):");
    let mut wrong = Vec::new();
    let mut never = Vec::new();
    for r in &map.regs {
        let seen = format!(
            "{}{}",
            if read.contains(&r.name) { "r" } else { "" },
            if written.contains(&r.name) { "w" } else { "" }
        );
        println!(
            "  {:#05x} {:<14} {:<6} host {:<2}  {}",
            r.address,
            r.name,
            r.access,
            seen,
            if seen.is_empty() { "never touched" } else { "" }
        );
        if seen.is_empty() {
            never.push(r.name.as_str());
        }
        if seen != r.host {
            wrong.push(format!(
                "{}: the host {} it, registers.toml says {:?}",
                r.name,
                match seen.as_str() {
                    "" => "never touches",
                    "r" => "reads",
                    "w" => "writes",
                    _ => "reads and writes",
                },
                r.host
            ));
        }
    }
    println!("  never touched by the host: {}", never.join(", "));
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The host's identity taps are the map's TAPS reset (what START without
/// INIT bit 0 would keep after configuration).
#[test]
fn the_identity_is_the_maps_taps_reset() {
    let Some(map) = load() else { return };
    let words: Vec<u32> = identity().iter().map(|&p| pack_point(p)).collect();
    assert_eq!(words, map.reg("TAPS").resets);
}
