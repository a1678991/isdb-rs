//! `isdb_frontend::control::chain` against the chain's one register map,
//! `fpga/isdb_fpga/chain/registers.toml` (issue #30 item 5; the HDL's
//! `top.py` is checked against the same file by `fpga/tests/test_registers.py`).
//!
//! Every register constant of [`reg`] is a map register at the same address
//! and every map register has one (the constants are read from the source,
//! so a constant added without the map fails, and compared with the
//! compiled values); every flag of [`features`], [`capture`], [`init`] and
//! [`status`] is the map's field; the multi-bit fields' accessors
//! ([`features::ldpc_limit`], [`features::ldpc_step`], [`capture::seq`],
//! [`DecoderHealth::from_words`]) read exactly the map's bits; the array
//! extents, the counter names, the reset constants ([`MAX_ITER_RESET`],
//! [`N0_DEFAULT`], [`NONE`], [`SLOTS_VERSION`]), the ID word's chain bits,
//! the front end's CTRL bits, STATUS and the command codes are the map's;
//! and each register's documentation says "Read only" exactly when the map
//! makes it read only on every image, names the FEATURES flag that gates it
//! and says when the image ignores a write the host makes.

use isdb_frontend::control::chain::{
    self as cc, Counters, DecoderHealth, MAX_ITER_RESET, N0_DEFAULT, NONE, SLOTS_VERSION, capture,
    features, init, reg, status,
};
use isdb_frontend::control::{self, Identity, Op};
use std::collections::BTreeMap;
use toml::{Table, Value};

const MAP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fpga/isdb_fpga/chain/registers.toml"
);
const SOURCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/control/chain.rs");

fn map() -> Table {
    let text = std::fs::read_to_string(MAP).unwrap_or_else(|e| panic!("{MAP}: {e}"));
    text.parse().expect("registers.toml parses")
}

fn int(v: &Value) -> u32 {
    u32::try_from(v.as_integer().expect("an integer")).expect("32 bits")
}

fn str_of<'a>(t: &'a Table, k: &str) -> &'a str {
    t.get(k).and_then(Value::as_str).unwrap_or("")
}

fn registers(m: &Table) -> Vec<&Table> {
    m["register"]
        .as_array()
        .expect("[[register]]")
        .iter()
        .map(|r| r.as_table().expect("a table"))
        .collect()
}

fn register<'a>(m: &'a Table, name: &str) -> &'a Table {
    registers(m)
        .into_iter()
        .find(|r| str_of(r, "name") == name)
        .unwrap_or_else(|| panic!("no register {name} in the map"))
}

fn address(r: &Table) -> u16 {
    u16::try_from(int(&r["address"])).expect("16 bits")
}

fn count(r: &Table) -> usize {
    r.get("count").map_or(1, |c| int(c) as usize)
}

fn reset(r: &Table) -> u32 {
    int(&r["reset"])
}

/// (lsb, width) of field `name`.
fn field(r: &Table, name: &str) -> (u32, u32) {
    let f = r["field"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|f| f.as_table().unwrap())
        .find(|f| str_of(f, "name") == name)
        .unwrap_or_else(|| panic!("no field {name} in {}", str_of(r, "name")));
    (int(&f["lsb"]), f.get("width").map_or(1, int))
}

fn ones(lsb: u32, width: u32) -> u32 {
    u32::try_from(((1u64 << width) - 1) << lsb).expect("32 bits")
}

/// The flags (one-bit fields) of a register: name → bit value.
fn flags(r: &Table) -> BTreeMap<String, u32> {
    r["field"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|f| f.as_table().unwrap())
        .filter(|f| f.get("width").is_none_or(|w| int(w) == 1))
        .map(|f| (str_of(f, "name").to_owned(), 1 << int(&f["lsb"])))
        .collect()
}

/// `pub const NAME: T = VALUE;` of `pub mod <name>` in the source, with
/// each constant's documentation.
fn source_consts(module: &str) -> BTreeMap<String, (u32, String)> {
    let src = std::fs::read_to_string(SOURCE).unwrap_or_else(|e| panic!("{SOURCE}: {e}"));
    let start = src
        .find(&format!("pub mod {module} {{"))
        .unwrap_or_else(|| panic!("no `pub mod {module}`"));
    let mut out = BTreeMap::new();
    let mut doc = String::new();
    for line in src[start..].lines().skip(1) {
        let l = line.trim();
        if l == "}" && line.starts_with('}') {
            break;
        }
        if let Some(d) = l.strip_prefix("///") {
            doc.push_str(d.trim());
            doc.push(' ');
        } else if let Some(rest) = l.strip_prefix("pub const ") {
            let (name, rest) = rest.split_once(':').expect("pub const NAME: T = V;");
            let value = rest
                .split_once('=')
                .map(|(_, v)| v.trim().trim_end_matches(';').replace('_', ""));
            let v = value.and_then(|v| {
                v.strip_prefix("0x")
                    .map_or_else(|| v.parse().ok(), |h| u32::from_str_radix(h, 16).ok())
            });
            // an expression (features::ALL) is checked by value
            if let Some(v) = v {
                out.insert(name.trim().to_owned(), (v, std::mem::take(&mut doc)));
            }
            doc.clear();
        } else if !l.starts_with("#[") {
            doc.clear();
        }
    }
    out
}

#[test]
fn every_register_constant_is_the_maps() {
    let m = map();
    let consts = source_consts("reg");
    // the source as read is the compiled crate
    let compiled = [
        ("VERSION", reg::VERSION),
        ("FEATURES", reg::FEATURES),
        ("CARRIER", reg::CARRIER),
        ("LAYOUT_LAG", reg::LAYOUT_LAG),
        ("INIT", reg::INIT),
        ("STATUS", reg::STATUS),
        ("FRAME", reg::FRAME),
        ("SEQ", reg::SEQ),
        ("N0", reg::N0),
        ("LLR_SCALE", reg::LLR_SCALE),
        ("LEARNED_FROM", reg::LEARNED_FROM),
        ("CONST_MASK", reg::CONST_MASK),
        ("MAX_ITER", reg::MAX_ITER),
        ("AGC", reg::AGC),
        ("TAPS", reg::TAPS),
        ("TAPS_FROM", reg::TAPS_FROM),
        ("SUMS_FRAME", reg::SUMS_FRAME),
        ("SUMS_COUNT", reg::SUMS_COUNT),
        ("SUMS_CAPTURE", reg::SUMS_CAPTURE),
        ("COUNTERS", reg::COUNTERS),
        ("TMCC_OVERRUNS", reg::TMCC_OVERRUNS),
        ("DECODER", reg::DECODER),
        ("RECLAIMED", reg::RECLAIMED),
        ("SUMS_R", reg::SUMS_R),
        ("SUMS_P", reg::SUMS_P),
        ("CONST", reg::CONST),
    ];
    assert_eq!(
        consts.len(),
        compiled.len(),
        "a reg:: constant not listed here"
    );
    for (name, v) in compiled {
        assert_eq!(consts.get(name).map(|c| c.0), Some(u32::from(v)), "{name}");
    }
    let from_map: BTreeMap<String, u32> = registers(&m)
        .iter()
        .map(|r| (str_of(r, "name").to_owned(), u32::from(address(r))))
        .collect();
    let from_src: BTreeMap<String, u32> = consts.iter().map(|(k, v)| (k.clone(), v.0)).collect();
    assert_eq!(from_src, from_map, "reg:: against registers.toml");
    let window: Vec<u32> = m["window"].as_array().unwrap().iter().map(int).collect();
    assert_eq!(window, [0x400, 0x700]);
}

#[test]
fn every_register_document_says_what_the_map_says() {
    let m = map();
    let consts = source_consts("reg");
    for r in registers(&m) {
        let name = str_of(r, "name");
        let doc = &consts[name].1;
        let read_only = str_of(r, "access") == "ro"
            && r.get("write_feature").is_none()
            && r.get("ignored_write").is_none();
        assert_eq!(
            doc.starts_with("Read only"),
            read_only,
            "reg::{name}: `{doc}` against access {} (registers.toml)",
            str_of(r, "access")
        );
        for key in ["feature", "write_feature"] {
            let f = str_of(r, key);
            if !f.is_empty() {
                assert!(
                    doc.contains(&format!("features::{f}`")),
                    "reg::{name} does not name features::{f} ({key} in registers.toml): `{doc}`"
                );
            }
        }
        if r.get("ignored_write").is_some() {
            assert!(
                doc.contains("ignores the write"),
                "reg::{name}: the image ignores the host's write (registers.toml), not said: `{doc}`"
            );
        }
    }
}

#[test]
fn every_flag_is_the_maps() {
    let m = map();
    let check = |module: &str, reg_name: &str| {
        let src: BTreeMap<String, u32> = source_consts(module)
            .into_iter()
            .map(|(k, v)| (k, v.0))
            .collect();
        assert_eq!(
            src,
            flags(register(&m, reg_name)),
            "{module}:: against {reg_name}'s flags"
        );
    };
    check("features", "FEATURES");
    check("capture", "SUMS_CAPTURE");
    check("init", "INIT");
    check("status", "STATUS");
    // the compiled values (the source as read)
    let f = register(&m, "FEATURES");
    for (name, v) in [
        ("TAPS", features::TAPS),
        ("SUMS", features::SUMS),
        ("CONSTELLATION", features::CONSTELLATION),
        ("N0", features::N0),
        ("SUMS_CAPTURE", features::SUMS_CAPTURE),
        ("DECODER_HEALTH", features::DECODER_HEALTH),
        ("SUMS_SEQ", features::SUMS_SEQ),
        ("SUMS_STREAM", features::SUMS_STREAM),
    ] {
        assert_eq!(1 << field(f, name).0, v, "features::{name}");
    }
    assert_eq!(
        features::ALL,
        flags(f).values().sum::<u32>(),
        "features::ALL"
    );
    let c = register(&m, "SUMS_CAPTURE");
    for (name, v) in [
        ("VALID", capture::VALID),
        ("ARMED", capture::ARMED),
        ("NO_LAYOUT", capture::NO_LAYOUT),
    ] {
        assert_eq!(1 << field(c, name).0, v, "capture::{name}");
    }
    let i = register(&m, "INIT");
    for (name, v) in [
        ("TAPS", init::TAPS),
        ("CONSTELLATION", init::CONSTELLATION),
        ("N0", init::N0),
    ] {
        assert_eq!(1 << field(i, name).0, v, "init::{name}");
    }
    let s = register(&m, "STATUS");
    for (name, v) in [
        ("LOCKED", status::LOCKED),
        ("FRAMED", status::FRAMED),
        ("DECODING", status::DECODING),
    ] {
        assert_eq!(1 << field(s, name).0, v, "status::{name}");
    }
}

/// A member of [`DecoderHealth`] as a number.
type Member = fn(&DecoderHealth) -> u32;

#[test]
fn every_multi_bit_field_reads_the_maps_bits() {
    let m = map();
    let f = register(&m, "FEATURES");
    let (lsb, w) = field(f, "MAX_ITER");
    assert_eq!(features::ldpc_limit(ones(lsb, w)), ones(0, w));
    assert_eq!(features::ldpc_limit(!ones(lsb, w)), 0);
    let (lsb, w) = field(f, "STEP");
    assert_eq!(features::ldpc_step(ones(lsb, w)), ones(0, w));
    assert_eq!(features::ldpc_step(!ones(lsb, w)), 0);
    let (lsb, w) = field(register(&m, "SUMS_CAPTURE"), "SEQ");
    assert_eq!(u32::from(capture::seq(ones(lsb, w))), ones(0, w));
    assert_eq!(capture::seq(!ones(lsb, w)), 0);
    // DECODER and RECLAIMED: each field all ones moves its member only
    let d = register(&m, "DECODER");
    let r = register(&m, "RECLAIMED");
    let max = |w: u32| ones(0, w);
    let cases: [(&Table, &str, Member); 6] = [
        (d, "OVERRUNS", |h| u32::from(h.overruns)),
        (d, "QUEUE_OVERFLOW", |h| u32::from(h.queue_overflow)),
        (d, "HELD", |h| u32::from(h.held)),
        (d, "BANKS", |h| u32::from(h.banks)),
        (r, "ABORTED", |h| u32::from(h.aborted)),
        (r, "REUSED", |h| u32::from(h.reused)),
    ];
    for (t, name, get) in cases {
        let (lsb, w) = field(t, name);
        let word = ones(lsb, w);
        let words = if std::ptr::eq(t, d) {
            [word, 0]
        } else {
            [0, word]
        };
        let h = DecoderHealth::from_words(words);
        assert_eq!(get(&h), max(w), "{name}");
        let others: u32 = cases
            .iter()
            .filter(|c| c.1 != name)
            .map(|c| (c.2)(&h))
            .sum();
        assert_eq!(others, 0, "{name} spills into another member");
    }
    // the map's reset of DECODER: no bank held, K banks
    let h = DecoderHealth::from_words([reset(d), reset(r)]);
    assert_eq!((h.held, h.banks), (0, 3));
}

#[test]
fn arrays_resets_and_the_id_word_are_the_maps() {
    let m = map();
    assert_eq!(count(register(&m, "TAPS")), cc::TAPS);
    assert_eq!(count(register(&m, "SUMS_R")), 4 * cc::R_ENTRIES);
    assert_eq!(count(register(&m, "SUMS_P")), 4 * cc::TAPS);
    assert_eq!(
        count(register(&m, "CONST")),
        cc::MODULATIONS * cc::MAX_POINTS
    );
    let sums: Vec<u16> = ["SUMS_R", "SUMS_P"]
        .iter()
        .flat_map(|n| {
            let r = register(&m, n);
            (0..count(r)).map(move |k| address(r) + u16::try_from(k).unwrap())
        })
        .collect();
    assert_eq!(cc::all_sum_registers(), sums);
    let c = register(&m, "COUNTERS");
    let names: Vec<&str> = c["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(names, Counters::NAMES);
    let addrs: Vec<u16> = (0..count(c))
        .map(|k| address(c) + u16::try_from(k).unwrap())
        .collect();
    assert_eq!(Counters::registers().to_vec(), addrs);
    // resets
    assert_eq!(reset(register(&m, "MAX_ITER")), MAX_ITER_RESET);
    assert_eq!(reset(register(&m, "N0")), N0_DEFAULT);
    for name in ["FRAME", "LEARNED_FROM", "TAPS_FROM", "SUMS_FRAME"] {
        assert_eq!(reset(register(&m, name)), NONE, "{name}");
    }
    assert_eq!(reset(register(&m, "VERSION")), u32::from(SLOTS_VERSION));
    // the ID word's chain bits
    let id = m["id_word"].as_table().unwrap();
    let (lsb, w) = (int(&id["chain_lsb"]), int(&id["chain_width"]));
    let fe = Identity {
        version: 4,
        taps: 32,
    };
    let word = cc::id_word(&fe, 0xF);
    assert_eq!(word & !fe.word(), ones(lsb, w));
    assert_eq!(cc::slots_version(fe.word() | 2 << lsb), Some(2));
}

#[test]
fn the_front_ends_bits_and_the_protocol_are_the_maps() {
    let m = map();
    let ctrl: BTreeMap<&str, u32> = m["ctrl"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            let c = c.as_table().unwrap();
            (str_of(c, "name"), 1 << int(&c["bit"]))
        })
        .collect();
    assert_eq!(
        ctrl,
        BTreeMap::from([
            ("RUN", control::ctrl::RUN),
            ("START", control::ctrl::START),
            ("SYMBOLS", control::ctrl::SYMBOLS),
            ("SLOTS", control::ctrl::SLOTS),
        ])
    );
    let fe: BTreeMap<&str, &Table> = m["frontend"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let f = f.as_table().unwrap();
            (str_of(f, "name"), f)
        })
        .collect();
    assert_eq!(address(fe["STATUS"]), control::reg::STATUS);
    assert_eq!(address(fe["KP"]), control::reg::KP);
    let st = fe["STATUS"]["fields"].as_table().unwrap();
    assert_eq!(1 << int(&st["SLOTS"]), cc::STATUS_SLOTS);
    let ops = m["protocol"]["ops"].as_table().unwrap();
    for (name, op) in [
        ("NOP", Op::Nop),
        ("ADDR", Op::Addr),
        ("HI", Op::Hi),
        ("WRITE", Op::Write),
        ("READ", Op::Read),
        ("CTRL", Op::Ctrl),
        ("WRITE_S", Op::WriteS),
    ] {
        assert_eq!(int(&ops[name]), u32::from(op as u8), "{name}");
    }
    assert_eq!(ops.len(), 7);
}
