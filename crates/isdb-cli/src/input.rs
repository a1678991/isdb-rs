//! Frame assembly from the configured inputs: per-layer TS files, one TS
//! split into layers by PID, or a synthetic test pattern.  Every call
//! yields exactly the packet count each layer carries per OFDM frame
//! (Table 3-3 × N); missing packets are H.222.0 null packets.

use crate::keys::Command;
use crate::phyargs::parse_pids;
use crate::source::{
    Feed, Packet, PacketStream, check_feed, is_null, null_packet, path_opener, pid, queue_depth,
    split_packets,
};
use anyhow::{Result, bail};
use clap::Args;
use isdbt_core::{Layer, Params};
use isdbt_mux::capacity;
use isdbt_mux::pcrclock::PcrClock;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

/// Packets of one OFDM frame.
#[derive(Debug, Clone)]
pub struct FramePackets {
    /// Exactly `counts[l]` packets per layer.
    pub layers: [Vec<Packet>; 3],
    /// How many of them came from the input (the rest are null padding).
    pub real: [usize; 3],
}

/// A source of per-frame packet sets.
pub trait FrameInput: Send {
    /// Packets for one frame; `None` once every input has ended and no
    /// input packet is left (or when stopped while blocked).
    fn take_frame(&mut self, counts: [usize; 3], stop: &AtomicBool) -> Option<FramePackets>;
    /// One-line description.
    fn describe(&self) -> String;
    /// Extra counters for status lines (may be empty).
    fn status(&self) -> String {
        String::new()
    }
    /// True if the input never ends by itself (only a frame bound or a
    /// stop request ends the transmission).
    fn endless(&self) -> bool {
        false
    }
    /// Enable playback control ([`Self::control`]) before the first frame;
    /// false when the input has none.
    fn enable_control(&mut self) -> Result<bool> {
        Ok(false)
    }
    /// Apply a playback command; the line to show (the new state).
    fn control(&mut self, _cmd: Command) -> String {
        "this input has no playback control".into()
    }
}

fn pad(layers: &mut [Vec<Packet>; 3], counts: [usize; 3]) -> [usize; 3] {
    std::array::from_fn(|l| {
        let real = layers[l].len();
        layers[l].resize(counts[l], null_packet());
        real
    })
}

/// Independent per-layer inputs.
pub struct LayerInputs {
    sources: [Option<PacketStream>; 3],
    feed: Feed,
}

impl LayerInputs {
    /// From per-layer streams (`None` = that layer carries only nulls).
    pub fn new(sources: [Option<PacketStream>; 3], feed: Feed) -> Self {
        Self { sources, feed }
    }
}

impl FrameInput for LayerInputs {
    fn take_frame(&mut self, counts: [usize; 3], stop: &AtomicBool) -> Option<FramePackets> {
        let mut layers: [Vec<Packet>; 3] = std::array::from_fn(|l| Vec::with_capacity(counts[l]));
        for (l, src) in self.sources.iter_mut().enumerate() {
            let Some(src) = src else { continue };
            while layers[l].len() < counts[l] {
                match src.next(stop) {
                    Some(p) => layers[l].push(p),
                    None => break,
                }
            }
        }
        let real = pad(&mut layers, counts);
        let any_source = self.sources.iter().any(Option::is_some);
        let all_ended = self.sources.iter().flatten().all(PacketStream::ended);
        if (any_source && all_ended && real.iter().sum::<usize>() == 0)
            || stop.load(Ordering::Relaxed)
        {
            return None;
        }
        Some(FramePackets { layers, real })
    }

    fn describe(&self) -> String {
        let v: Vec<String> = self
            .sources
            .iter()
            .zip(Layer::ALL)
            .map(|(s, l)| {
                format!(
                    "{l} <- {}",
                    s.as_ref().map_or("nulls".to_string(), |s| s.name.clone())
                )
            })
            .collect();
        format!("{} (feed {:?})", v.join(", "), self.feed)
    }

    fn status(&self) -> String {
        let skipped: u64 = self
            .sources
            .iter()
            .flatten()
            .map(|s| s.stats.skipped_bytes.load(Ordering::Relaxed))
            .sum();
        let rewinds: u64 = self
            .sources
            .iter()
            .flatten()
            .map(|s| s.stats.rewinds.load(Ordering::Relaxed))
            .sum();
        let mut s = String::new();
        if skipped > 0 {
            s += &format!(" resync-skipped {skipped} B");
        }
        if rewinds > 0 {
            s += &format!(" rewinds {rewinds}");
        }
        s
    }
}

/// One TS split into layers by PID (relay of a station mux).  PIDs listed
/// for layers A/C go there, every other PID to layer B, null packets are
/// dropped.  Input order is preserved: when the next packet's layer is full
/// for this frame, the frame is closed (other layers padded) and the packet
/// opens the next frame, so no layer ever runs ahead of the input.
///
/// With a file (or loop) feed the input is paced by its own PCR
/// ([`isdbt_mux::pcrclock::PcrClock`], H.222.0 §2.4.2.2): every packet gets
/// its arrival time interpolated from the PCR, and OFDM frame *k* carries
/// the packets due before the end of the frame, `t < (k+1)·T_frame`, so the
/// stream goes on air at its own rate and the rest of each frame is null
/// padding.  When a layer cannot carry the packets due in a frame they wait
/// for the next one and the lag (on-air time − due time) is reported.  A
/// live feed is paced by its writer instead.
pub struct MuxInput {
    stream: PacketStream,
    route: Box<[u8; 8192]>,
    carry: Option<(usize, Packet)>,
    /// PCR pacing (file/loop feeds).
    pacer: Option<Pacer>,
    /// Null packets dropped from the input.
    pub dropped_nulls: u64,
    /// Frames closed early because a layer was full.
    pub saturated: [u64; 3],
    /// Layer capacities (bit/s), for the status line.
    capacity: [f64; 3],
}

/// PCR pacing state of [`MuxInput`].
struct Pacer {
    clock: PcrClock<Packet>,
    frame_ticks: u64,
    frame: u64,
    /// Input bytes per layer and the time span they cover.
    bytes: [u64; 3],
    first_t: Option<u64>,
    last_t: u64,
    /// Lag of the last packet sent (ticks), and the largest seen.
    lag: u64,
    max_lag: u64,
    ended: bool,
    /// Next packet (layer, packet, due time) held back for a later frame.
    carry: Option<(usize, Packet, u64)>,
}

impl MuxInput {
    /// Split `stream`; `a_pids`/`c_pids` select layers A/C, the rest B.
    pub fn new(stream: PacketStream, a_pids: &[u16], c_pids: &[u16]) -> Self {
        let mut route = Box::new([1u8; 8192]);
        for &p in a_pids {
            route[p as usize] = 0;
        }
        for &p in c_pids {
            route[p as usize] = 2;
        }
        Self {
            stream,
            route,
            carry: None,
            pacer: None,
            dropped_nulls: 0,
            saturated: [0; 3],
            capacity: [0.0; 3],
        }
    }

    /// Pace by the input's PCR (frames of `params`); see the type docs.
    pub fn paced(mut self, params: &Params) -> Self {
        self.pacer = Some(Pacer {
            clock: PcrClock::new(None),
            frame_ticks: params.frame_ticks_27mhz(),
            frame: 0,
            bytes: [0; 3],
            first_t: None,
            last_t: 0,
            lag: 0,
            max_lag: 0,
            ended: false,
            carry: None,
        });
        self.capacity = std::array::from_fn(|l| capacity::layer_bps(params, Layer::ALL[l]));
        self
    }

    fn take_unpaced(&mut self, counts: [usize; 3], stop: &AtomicBool) -> [Vec<Packet>; 3] {
        let mut layers: [Vec<Packet>; 3] = std::array::from_fn(|l| Vec::with_capacity(counts[l]));
        loop {
            if layers.iter().zip(counts).all(|(v, n)| v.len() >= n) {
                break;
            }
            let (l, p) = match self.carry.take() {
                Some(c) => c,
                None => match self.stream.next(stop) {
                    Some(p) if is_null(&p) => {
                        self.dropped_nulls += 1;
                        continue;
                    }
                    Some(p) => (self.route[pid(&p) as usize] as usize, p),
                    None => break,
                },
            };
            if layers[l].len() >= counts[l] {
                self.carry = Some((l, p));
                self.saturated[l] += 1;
                break;
            }
            layers[l].push(p);
        }
        layers
    }
}

impl Pacer {
    /// The next frame's packets of [`MuxInput`] paced by the PCR: the
    /// input's `stream`, `route` and counters are passed in (the pacer is a
    /// field of the same input).
    fn take(
        &mut self,
        stream: &mut PacketStream,
        route: &[u8; 8192],
        dropped_nulls: &mut u64,
        saturated: &mut [u64; 3],
        counts: [usize; 3],
        stop: &AtomicBool,
    ) -> [Vec<Packet>; 3] {
        let mut layers: [Vec<Packet>; 3] = std::array::from_fn(|l| Vec::with_capacity(counts[l]));
        let frame_start = self.frame * self.frame_ticks;
        let limit = frame_start + self.frame_ticks;
        self.frame += 1;
        loop {
            // Next packet in input order: the one held back by a full layer,
            // else the clock's next timed packet (reading more input until
            // the clock can time one).
            let (l, p, t) = match self.carry.take() {
                Some(c) => c,
                None => {
                    while self.clock.ready() == 0 && !self.ended {
                        match stream.next(stop) {
                            Some(p) => self.clock.push(&p, p),
                            None if stream.ended() => {
                                self.clock.finish();
                                self.ended = true;
                            }
                            None => return layers, // stopped
                        }
                    }
                    let Some(x) = self.clock.pop() else { break };
                    if is_null(&x.item) {
                        *dropped_nulls += 1;
                        continue;
                    }
                    (route[pid(&x.item) as usize] as usize, x.item, x.t)
                }
            };
            if t >= limit {
                self.carry = Some((l, p, t)); // due in a later frame
                break;
            }
            if layers[l].len() >= counts[l] {
                // Layer full: the packet waits for the next frame (order kept).
                self.carry = Some((l, p, t));
                saturated[l] += 1;
                break;
            }
            self.lag = frame_start.saturating_sub(t);
            self.max_lag = self.max_lag.max(self.lag);
            self.bytes[l] += 188;
            self.first_t.get_or_insert(t);
            self.last_t = t;
            layers[l].push(p);
        }
        layers
    }
}

impl FrameInput for MuxInput {
    fn take_frame(&mut self, counts: [usize; 3], stop: &AtomicBool) -> Option<FramePackets> {
        let mut layers = match &mut self.pacer {
            Some(pc) => pc.take(
                &mut self.stream,
                &self.route,
                &mut self.dropped_nulls,
                &mut self.saturated,
                counts,
                stop,
            ),
            None => self.take_unpaced(counts, stop),
        };
        let real = pad(&mut layers, counts);
        let drained = match &self.pacer {
            Some(pc) => pc.ended && pc.clock.ready() == 0 && pc.carry.is_none(),
            None => self.stream.ended() && self.carry.is_none(),
        };
        if (drained && real.iter().sum::<usize>() == 0) || stop.load(Ordering::Relaxed) {
            return None;
        }
        Some(FramePackets { layers, real })
    }

    fn describe(&self) -> String {
        let a: Vec<String> = (0..8192)
            .filter(|&p| self.route[p] == 0)
            .map(|p| format!("0x{p:04x}"))
            .collect();
        let c: Vec<String> = (0..8192)
            .filter(|&p| self.route[p] == 2)
            .map(|p| format!("0x{p:04x}"))
            .collect();
        format!(
            "{} split by PID: A <- [{}], C <- [{}], B <- other PIDs; nulls dropped (feed {:?}{})",
            self.stream.name,
            a.join(","),
            c.join(","),
            self.stream.feed(),
            if self.pacer.is_some() {
                ", paced by the input's PCR"
            } else {
                ""
            }
        )
    }

    fn status(&self) -> String {
        let mut s = format!(
            " in-nulls-dropped {} layer-full A/B/C {}/{}/{}",
            self.dropped_nulls, self.saturated[0], self.saturated[1], self.saturated[2]
        );
        if let Some(pc) = &self.pacer {
            let span = pc.last_t.saturating_sub(pc.first_t.unwrap_or(0));
            if span > 27_000_000 / 2 {
                let mut over = Vec::new();
                for (l, id) in Layer::ALL.iter().enumerate() {
                    if pc.bytes[l] == 0 {
                        continue;
                    }
                    let bps = pc.bytes[l] as f64 * 8.0 * 27e6 / span as f64;
                    s += &format!(
                        " | in {id} {:.2} Mbit/s (cap {:.2})",
                        bps / 1e6,
                        self.capacity[l] / 1e6
                    );
                    if bps > self.capacity[l] * 1.001 {
                        over.push(id.to_string());
                    }
                }
                if !over.is_empty() {
                    s += &format!(" OVER CAPACITY on {}", over.join("/"));
                }
            }
            s += &format!(
                " | PCR lag {:.2} s (max {:.2})",
                pc.lag as f64 / 27e6,
                pc.max_lag as f64 / 27e6
            );
            let st = pc.clock.stats();
            if st.discontinuities > 0 {
                s += &format!(" pcr-discontinuities {}", st.discontinuities);
            }
        }
        s
    }
}

/// Rates of one input TS per layer as routed by the PID split, measured
/// over its first `probe_s` seconds of PCR time (nulls excluded).
#[derive(Debug, Clone, Default)]
pub struct InputRates {
    /// PCR PID used as the time base (None: the input has no PCR).
    pub pcr_pid: Option<isdb_ts::Pid>,
    /// Seconds of PCR time measured.
    pub span_s: f64,
    /// Mean rate per layer (bit/s).
    pub mean: [f64; 3],
    /// Largest rate over any 1 s window per layer (bit/s).
    pub peak_1s: [f64; 3],
}

/// Measure the per-layer rates of `path` (a regular file) from its PCR:
/// at most `probe_s` seconds of PCR time or `max_bytes` bytes. `route`
/// gives a PID's layer index (0 = A … 2 = C).
pub fn probe_rates(
    path: &str,
    route: &dyn Fn(u16) -> u8,
    probe_s: u64,
    max_bytes: usize,
) -> Result<InputRates> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; max_bytes];
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    buf.truncate(n);
    let mut clock = PcrClock::<u8>::new(None);
    let mut skipped = 0;
    let limit = probe_s * 27_000_000;
    // (time, layer) of every non-null packet
    let mut timed: Vec<(u64, u8)> = Vec::new();
    let mut done = false;
    split_packets(&buf, &mut skipped, |p| {
        let l = if is_null(&p) { 255 } else { route(pid(&p)) };
        clock.push(&p, l);
        while let Some(x) = clock.pop() {
            if x.t > limit {
                done = true;
            }
            timed.push((x.t, x.item));
        }
        !done
    });
    let mut r = InputRates {
        pcr_pid: clock.pcr_pid(),
        ..Default::default()
    };
    if r.pcr_pid.is_none() || timed.len() < 2 {
        return Ok(r);
    }
    let t0 = timed[0].0;
    let t1 = timed.last().map_or(t0, |x| x.0);
    r.span_s = (t1 - t0) as f64 / 27e6;
    if r.span_s < 0.5 {
        return Ok(r);
    }
    let mut total = [0u64; 3];
    for &(_, l) in &timed {
        if l < 3 {
            total[l as usize] += 188 * 8;
        }
    }
    r.mean = std::array::from_fn(|l| total[l] as f64 / r.span_s);
    // sliding 1 s windows (two-pointer)
    let win = 27_000_000u64;
    let mut acc = [0u64; 3];
    let mut j = 0;
    for i in 0..timed.len() {
        let (t, l) = timed[i];
        if l < 3 {
            acc[l as usize] += 188 * 8;
        }
        while timed[j].0 + win <= t {
            if timed[j].1 < 3 {
                acc[timed[j].1 as usize] -= 188 * 8;
            }
            j += 1;
        }
        if t >= t0 + win {
            for (peak, &a) in r.peak_1s.iter_mut().zip(&acc) {
                *peak = peak.max(a as f64);
            }
        }
    }
    if r.span_s < 1.0 {
        r.peak_1s = r.mean;
    }
    Ok(r)
}

/// Deterministic test packets: layer `l` uses PID 0x0100·(l+1), a running
/// continuity counter, the packet number in bytes 4..12 and a xorshift
/// payload, so every packet of a run is distinct.
pub struct SyntheticInput {
    next: [u64; 3],
}

impl SyntheticInput {
    /// New pattern starting at packet 0.
    pub fn new() -> Self {
        Self { next: [0; 3] }
    }

    /// Packet `n` of layer `l` (0 = A … 2 = C).
    pub fn packet(l: u8, n: u64) -> Packet {
        let mut p = [0u8; 188];
        let pid = 0x100 * (u16::from(l) + 1);
        p[0] = 0x47;
        p[1..3].copy_from_slice(&pid.to_be_bytes());
        p[3] = 0x10 | (n & 15) as u8;
        p[4..12].copy_from_slice(&n.to_be_bytes());
        let mut x = n.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ ((u64::from(l) + 1) << 56) | 1;
        for b in &mut p[12..] {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x.to_le_bytes()[3]; // bits 31..24
        }
        p
    }
}

impl FrameInput for SyntheticInput {
    fn take_frame(&mut self, counts: [usize; 3], stop: &AtomicBool) -> Option<FramePackets> {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let layers = [0u8, 1, 2].map(|id| {
            let l = usize::from(id);
            let v = (0..counts[l] as u64)
                .map(|k| Self::packet(id, self.next[l] + k))
                .collect();
            self.next[l] += counts[l] as u64;
            v
        });
        Some(FramePackets {
            layers,
            real: counts,
        })
    }

    fn describe(&self) -> String {
        "synthetic test packets (PID 0x0100/0x0200/0x0300 for A/B/C)".into()
    }

    fn endless(&self) -> bool {
        true
    }
}

/// In-process service multiplexer (isdbt-mux): every frame comes from
/// `Multiplexer::next_frame`, which already yields exactly the per-layer
/// packet counts of the transmission parameters (its own null packets are
/// counted as null fill).
pub struct MuxConfigInput {
    mux: isdbt_mux::Multiplexer,
    name: String,
    endless: bool,
}

/// True if a multiplexer built from `cfg` ends by itself: it relays at
/// least one `ts-program` and every one of them has `at_eof = "end"`
/// ([`isdbt_mux::Multiplexer::relays_finished`]); otherwise it runs forever.
pub fn mux_config_ends(cfg: &isdbt_mux::Config) -> bool {
    use isdbt_mux::config::{AtEof, StreamKind};
    let mut relays = cfg
        .services
        .iter()
        .flat_map(|s| &s.streams)
        .filter(|st| st.kind == StreamKind::TsProgram)
        .peekable();
    relays.peek().is_some() && relays.all(|st| st.at_eof == AtEof::End)
}

impl MuxConfigInput {
    /// The playback state of the relays (empty without playback control).
    fn playback(&self) -> String {
        self.mux
            .playback_status()
            .iter()
            .map(isdbt_mux::playback::PlaybackStatus::describe)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Build the multiplexer from a TOML file (relative media paths are
    /// resolved against the file's directory).
    pub fn open(path: &std::path::Path, params: &Params) -> Result<Self> {
        use anyhow::Context;
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg = isdbt_mux::Config::from_toml(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        let base = path
            .parent()
            .map(std::path::PathBuf::from)
            .unwrap_or_default();
        let mux =
            isdbt_mux::Multiplexer::new(&cfg, params, &base).context("building the multiplexer")?;
        Ok(Self::from_mux(mux, path.display().to_string(), &cfg))
    }

    /// Wrap a built multiplexer, printing its start-up notes (ts-program
    /// probe reports: program, PID → role → output PID, dropped PIDs,
    /// rates, layer usage) and warnings.  `cfg` is the configuration it was
    /// built from (for [`FrameInput::endless`]).
    pub fn from_mux(mux: isdbt_mux::Multiplexer, name: String, cfg: &isdbt_mux::Config) -> Self {
        for n in mux.notes() {
            let text = n.replace('\n', "\n  ");
            match copy_control_note(n) {
                Some((source, output)) => {
                    info!(target: "isdb::tx", source, output, "  {text}");
                }
                None => info!(target: "isdb::tx", "  {text}"),
            }
        }
        for w in mux.warnings() {
            warn!(target: "isdb::tx", "mux warning: {w}");
        }
        Self {
            mux,
            name,
            endless: !mux_config_ends(cfg),
        }
    }
}

/// The relay policy's copy-control note among a multiplexer's start-up
/// notes (CONTRIBUTING.md "Relay policy": the source's digital copy control
/// is reported whenever a `ts-program` service is relayed, so a relaxed
/// value is visible): `(source, output)` as the note states them, `None`
/// for any other note. The note's shape is `isdbt_mux`'s
/// `service 0x….: source copy control: <source>; relayed as: <output>[; …]`.
pub fn copy_control_note(note: &str) -> Option<(&str, &str)> {
    let (_, rest) = note.split_once("source copy control: ")?;
    let (source, rest) = rest.split_once("; relayed as: ")?;
    let output = rest.split_once("; ").map_or(rest, |(o, _)| o);
    Some((source, output))
}

impl FrameInput for MuxConfigInput {
    fn take_frame(&mut self, counts: [usize; 3], stop: &AtomicBool) -> Option<FramePackets> {
        if stop.load(Ordering::Relaxed) || self.mux.relays_finished() {
            return None;
        }
        let layers = self.mux.next_frame();
        for w in self.mux.take_alerts() {
            warn!(target: "isdb::tx", "mux warning: {w}");
        }
        debug_assert!(
            (0..3).all(|l| layers[l].len() == counts[l]),
            "multiplexer frame size"
        );
        let real = std::array::from_fn(|l| layers[l].iter().filter(|p| !is_null(p)).count());
        Some(FramePackets { layers, real })
    }

    fn describe(&self) -> String {
        format!("isdbt-mux service multiplexer from {}", self.name)
    }

    fn endless(&self) -> bool {
        self.endless
    }

    fn enable_control(&mut self) -> Result<bool> {
        self.mux.enable_playback_control()?;
        Ok(!self.mux.playback_status().is_empty())
    }

    fn control(&mut self, cmd: Command) -> String {
        let rate = self.mux.playback_status().first().map_or(1.0, |p| p.rate);
        let note = match cmd {
            Command::TogglePause => {
                self.mux.toggle_pause();
                String::new()
            }
            Command::Seek(d) => match self.mux.seek(d).first() {
                Some(t) => format!(" (seek {d:+} s: reading from {t:.1} s)"),
                None => " (no seek point)".into(),
            },
            Command::Faster | Command::Slower => {
                match isdbt_mux::playback::next_rate(rate, cmd == Command::Faster) {
                    Some(r) => {
                        self.mux.set_rate(r);
                        String::new()
                    }
                    None => " (no further rate)".into(),
                }
            }
        };
        format!("{}{note}", self.playback())
    }

    fn status(&self) -> String {
        let st = self.mux.stats();
        let mut s = format!(" | mux late PES {} errors {}", st.late_pes, st.errors.len());
        let pb = self.playback();
        if !pb.is_empty() {
            s = format!(" | {pb}{s}");
        }
        for r in self.mux.relay_status() {
            s += &format!(" | {r}");
        }
        s
    }
}

/// PCR time measured by the start-up rate probe of a `--mux-ts` file.
pub const PROBE_SECONDS: u64 = 30;
/// Byte limit of the probe (≈ 30 s at 34 Mbit/s).
pub const PROBE_BYTES: usize = 128 << 20;

fn is_regular_file(path: &str) -> bool {
    path != "-" && std::fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// Report the probed rates against the layer capacities.  Returns whether
/// to pace by PCR (false when the input has no PCR); refuses (error with
/// layouts that fit) a mean rate above a layer's capacity unless
/// `allow_overrate`.
pub fn check_input_rates(r: &InputRates, params: &Params, allow_overrate: bool) -> Result<bool> {
    let Some(pcr) = r.pcr_pid else {
        info!(
            target: "isdb::tx",
            probe_mb = PROBE_BYTES >> 20,
            "  input rate: no PCR found in the first {} MB — not paced (packets go out as fast as the layers take them)",
            PROBE_BYTES >> 20
        );
        return Ok(false);
    };
    let mut over = Vec::new();
    for (id, _) in params.active_layers() {
        let l = id.index();
        let cap = capacity::layer_bps(params, id);
        if r.mean[l] == 0.0 {
            continue;
        }
        info!(
            target: "isdb::tx",
            layer = %id,
            pcr_pid = format_args!("{pcr:#06x}"),
            span_s = r.span_s,
            mean_bps = r.mean[l],
            peak_1s_bps = r.peak_1s[l],
            capacity_bps = cap,
            "  input rate (PCR 0x{pcr:04x}, first {:.1} s): layer {id} mean {:.2} Mbit/s, peak {:.2} Mbit/s (1 s) — capacity {:.2} Mbit/s",
            r.span_s,
            r.mean[l] / 1e6,
            r.peak_1s[l] / 1e6,
            cap / 1e6
        );
        if r.mean[l] > cap {
            over.push(capacity::over_capacity_message(
                params,
                id,
                r.mean[l],
                &format!("the input's layer {id} share (mean over {:.1} s)", r.span_s),
            ));
        } else if r.peak_1s[l] > cap {
            warn!(
                target: "isdb::tx",
                layer = %id,
                peak_1s_bps = r.peak_1s[l],
                capacity_bps = cap,
                "  layer {id}: 1-s peaks ({:.2} Mbit/s) exceed the capacity; the stream falls behind its PCR schedule during peaks (lag shown in the status lines)",
                r.peak_1s[l] / 1e6
            );
        }
    }
    if !over.is_empty() {
        let msg = over.join("\n");
        if !allow_overrate {
            bail!(
                "{msg}\n  the file would go on air slower than real time; choose a layout above, or pass --allow-overrate to send it anyway"
            );
        }
        warn!(target: "isdb::tx", "(--allow-overrate): {msg}");
    }
    Ok(true)
}

/// Input selection shared by `tx` and `loopback`.
#[derive(Debug, Clone, Args)]
pub struct InputArgs {
    /// Layer A transport stream ("-" = stdin; FIFOs fine).
    #[arg(long, value_name = "PATH", conflicts_with = "relay")]
    pub a_ts: Option<String>,
    /// Layer B transport stream.
    #[arg(long, value_name = "PATH", conflicts_with = "relay")]
    pub b_ts: Option<String>,
    /// Layer C transport stream.
    #[arg(long, value_name = "PATH", conflicts_with = "relay")]
    pub c_ts: Option<String>,
    /// One TS to split into layers by PID (relay of a station mux):
    /// --a-pids/--c-pids go to A/C, all other PIDs to B, nulls dropped.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["a_ts", "b_ts", "c_ts", "synthetic", "relay"])]
    pub mux_ts: Option<String>,
    /// PIDs carried in layer A with --mux-ts (e.g. 0x0000,0x0010,0x1fc8).
    #[arg(long, value_name = "LIST", requires = "mux_ts", conflicts_with_all = ["a_ts", "b_ts", "c_ts", "synthetic"])]
    pub a_pids: Option<String>,
    /// PIDs carried in layer C with --mux-ts.
    #[arg(long, value_name = "LIST", requires = "mux_ts", conflicts_with_all = ["a_ts", "b_ts", "c_ts", "synthetic"])]
    pub c_pids: Option<String>,
    /// Feed policy: file (end at EOF), loop (rewind at EOF), live (never
    /// block; pad with null packets when the input lags).
    #[arg(long, value_enum, default_value_t = Feed::File)]
    pub feed: Feed,
    /// Transmit deterministic synthetic packets in every layer.
    #[arg(long, conflicts_with_all = ["a_ts", "b_ts", "c_ts", "relay"])]
    pub synthetic: bool,
    /// Generate the TS in-process with the isdbt-mux service multiplexer
    /// from this TOML description (see `isdb mux --print-config-example`);
    /// the multiplexer uses the same transmission parameters.
    #[arg(long, value_name = "TOML", conflicts_with_all = ["a_ts", "b_ts", "c_ts", "mux_ts", "synthetic", "relay"])]
    pub mux_config: Option<std::path::PathBuf>,
    /// `--relay FILE` and its options (rebroadcast a recording without a TOML).
    #[command(flatten)]
    pub relay: crate::relay::RelayArgs,
    /// Transmit a --mux-ts file even though its rate (measured from its
    /// PCR) exceeds a layer's capacity: it then falls behind its own
    /// schedule (slow motion; the lag is reported).  Without this flag such
    /// an input is refused with layer layouts that fit.
    #[arg(long)]
    pub allow_overrate: bool,
}

impl InputArgs {
    /// True if no input was selected.
    pub fn is_empty(&self) -> bool {
        self.a_ts.is_none()
            && self.b_ts.is_none()
            && self.c_ts.is_none()
            && self.mux_ts.is_none()
            && !self.synthetic
            && self.mux_config.is_none()
            && self.relay.relay.is_none()
    }

    /// True for inputs generated in-process (synthetic, mux config,
    /// relay); they never end by themselves unless every relay has
    /// `at_eof = "end"`.
    pub fn is_generated(&self) -> bool {
        self.synthetic || self.mux_config.is_some() || self.relay.relay.is_some()
    }

    /// Open the inputs for `params`.
    pub fn open(&self, params: &Params) -> Result<Box<dyn FrameInput>> {
        let counts: [usize; 3] = std::array::from_fn(|l| {
            params.layers()[l].map_or(0, |c| c.tsp_per_frame(params.mode()))
        });
        let frame_s = params.frame_seconds();
        if self.synthetic {
            return Ok(Box::new(SyntheticInput::new()));
        }
        if self.relay.relay.is_some() {
            bail!("--relay is resolved by relay::prepare before the inputs are opened");
        }
        if let Some(path) = &self.mux_config {
            return Ok(Box::new(MuxConfigInput::open(path, params)?));
        }
        if let Some(path) = &self.mux_ts {
            check_feed(path, self.feed)?;
            let a = parse_pids(self.a_pids.as_deref().unwrap_or(""))?;
            let c = parse_pids(self.c_pids.as_deref().unwrap_or(""))?;
            if !a.is_empty() && counts[0] == 0 {
                bail!("--a-pids given but layer A is not configured");
            }
            if !c.is_empty() && counts[2] == 0 {
                bail!("--c-pids given but layer C is not configured");
            }
            if counts[1] == 0 {
                bail!("--mux-ts sends unlisted PIDs to layer B, which is not configured");
            }
            if let Some(dup) = a.iter().find(|p| c.contains(p)) {
                bail!("PID 0x{dup:04x} listed for both layers A and C");
            }
            let mut paced = self.feed != Feed::Live;
            if paced && is_regular_file(path) {
                let route = |p: u16| {
                    if a.contains(&p) {
                        0
                    } else if c.contains(&p) {
                        2
                    } else {
                        1
                    }
                };
                let r = probe_rates(path, &route, PROBE_SECONDS, PROBE_BYTES)?;
                paced = check_input_rates(&r, params, self.allow_overrate)?;
            }
            let depth = queue_depth(counts.iter().sum(), frame_s);
            let s = PacketStream::spawn(path, path_opener(path), self.feed, depth)?;
            let m = MuxInput::new(s, &a, &c);
            return Ok(Box::new(if paced { m.paced(params) } else { m }));
        }
        if self.is_empty() {
            bail!("no input: give --a-ts/--b-ts/--c-ts, --mux-ts, --mux-config or --synthetic");
        }
        let paths = [&self.a_ts, &self.b_ts, &self.c_ts];
        if paths.iter().filter(|p| p.as_deref() == Some("-")).count() > 1 {
            bail!("only one input can be stdin");
        }
        let mut sources: [Option<PacketStream>; 3] = Default::default();
        for (l, p) in paths.iter().enumerate() {
            let Some(p) = p else { continue };
            if counts[l] == 0 {
                bail!(
                    "input given for layer {} which is not configured",
                    Layer::ALL[l]
                );
            }
            check_feed(p, self.feed)?;
            sources[l] = Some(PacketStream::spawn(
                p,
                path_opener(p),
                self.feed,
                queue_depth(counts[l], frame_s),
            )?);
        }
        Ok(Box::new(LayerInputs::new(sources, self.feed)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mux_config_ends_only_when_every_relay_ends() {
        use isdbt_mux::config::{AtEof, StreamKind};
        use isdbt_mux::relay_template::RelayOptions;
        let mut o = RelayOptions::new("x.ts");
        assert!(
            !mux_config_ends(&o.config().unwrap()),
            "at_eof loop (default)"
        );
        o.at_eof = AtEof::End;
        let mut cfg = o.config().unwrap();
        assert!(mux_config_ends(&cfg));
        // A second relay that loops keeps the multiplex running.
        let mut looping = cfg.services[0].streams[0].clone();
        looping.at_eof = AtEof::Loop;
        cfg.services[0].streams.push(looping);
        assert!(!mux_config_ends(&cfg));
        // No ts-program at all (media loops, PSI/SI only): never ends.
        for st in &mut cfg.services[0].streams {
            st.kind = StreamKind::Declared;
        }
        assert!(!mux_config_ends(&cfg));
    }
    use crate::source::tests::pkt;
    use std::sync::mpsc::sync_channel;

    fn live(name: &str) -> (std::sync::mpsc::SyncSender<Packet>, PacketStream) {
        let (tx, rx) = sync_channel(1 << 12);
        (tx, PacketStream::from_receiver(name, rx, Feed::Live))
    }

    #[test]
    fn live_pads_with_nulls_and_keeps_cbr() {
        let (tx, s) = live("b");
        let mut inp = LayerInputs::new([None, Some(s), None], Feed::Live);
        let stop = AtomicBool::new(false);
        for n in 0..5 {
            tx.send(pkt(0x200, n)).unwrap();
        }
        let f = inp.take_frame([4, 10, 0], &stop).unwrap();
        assert_eq!(f.real, [0, 5, 0]);
        assert_eq!(f.layers[0].len(), 4);
        assert_eq!(f.layers[1].len(), 10);
        assert!(f.layers[0].iter().all(is_null));
        assert_eq!(
            &f.layers[1][..5],
            &(0..5).map(|n| pkt(0x200, n)).collect::<Vec<_>>()[..]
        );
        assert!(f.layers[1][5..].iter().all(is_null));
        // Stalled input: an all-null frame, immediately.
        let t = std::time::Instant::now();
        let f = inp.take_frame([4, 10, 0], &stop).unwrap();
        assert_eq!(f.real, [0, 0, 0]);
        assert!(t.elapsed() < std::time::Duration::from_millis(20));
        // More than a frame queued: the excess waits for the next frame.
        for n in 5..20 {
            tx.send(pkt(0x200, n)).unwrap();
        }
        assert_eq!(inp.take_frame([4, 10, 0], &stop).unwrap().real, [0, 10, 0]);
        drop(tx);
        let f = inp.take_frame([4, 10, 0], &stop).unwrap();
        assert_eq!(f.real, [0, 5, 0]);
        assert_eq!(f.layers[1][0], pkt(0x200, 15));
        assert!(inp.take_frame([4, 10, 0], &stop).is_none());
    }

    #[test]
    fn layer_inputs_end_when_all_inputs_end() {
        let (ta, a) = live("a");
        let (tb, b) = live("b");
        let mut inp = LayerInputs::new([Some(a), Some(b), None], Feed::Live);
        let stop = AtomicBool::new(false);
        drop(ta);
        tb.send(pkt(0x200, 0)).unwrap();
        assert_eq!(inp.take_frame([2, 2, 0], &stop).unwrap().real, [0, 1, 0]);
        assert!(inp.take_frame([2, 2, 0], &stop).is_some(), "B still open");
        drop(tb);
        assert!(inp.take_frame([2, 2, 0], &stop).is_none());
    }

    #[test]
    fn mux_split_routes_by_pid_and_preserves_order() {
        let (tx, s) = live("mux");
        let mut inp = MuxInput::new(s, &[0x1fc8, 0x0000], &[]);
        let stop = AtomicBool::new(false);
        let seq = [
            pkt(0x0000, 0),
            pkt(0x0100, 1),
            pkt(0x1fff, 2),
            pkt(0x1fc8, 3),
            pkt(0x0101, 4),
            pkt(0x1fc8, 5),
        ];
        for p in seq {
            tx.send(p).unwrap();
        }
        // Layer A holds 2 per frame: the third A packet closes the frame.
        let f = inp.take_frame([2, 5, 0], &stop).unwrap();
        assert_eq!(f.real, [2, 2, 0]);
        assert_eq!(f.layers[0][..2], [seq[0], seq[3]]);
        assert_eq!(f.layers[1][..2], [seq[1], seq[4]]);
        assert!(f.layers[1][2..].iter().all(is_null));
        assert_eq!(inp.dropped_nulls, 1);
        assert_eq!(inp.saturated, [1, 0, 0]);
        let f = inp.take_frame([2, 5, 0], &stop).unwrap();
        assert_eq!(f.real, [1, 0, 0]);
        assert_eq!(f.layers[0][0], seq[5]);
        drop(tx);
        assert!(inp.take_frame([2, 5, 0], &stop).is_none());
    }

    /// AF-only PCR packet (H.222.0 Table 2-6; base·300 + ext = `ticks`).
    pub(crate) fn pcr_packet(pid: u16, ticks: u64) -> Packet {
        let (base, ext) = (ticks / 300, ticks % 300);
        let mut p = [0xFFu8; 188];
        let [hi, lo] = pid.to_be_bytes();
        p[..6].copy_from_slice(&[0x47, hi, lo, 0x20, 183, 0x10]);
        // base bits 32..0 in bytes 6..=10, then 6 reserved bits and ext
        p[6..11].copy_from_slice(&(base << 7).to_be_bytes()[3..]);
        let [.., e_hi, e_lo] = ext.to_be_bytes();
        p[10] |= 0x7E | (e_hi & 1);
        p[11] = e_lo;
        p
    }

    /// `frames` OFDM frames' worth of input at `per_frame` data packets per
    /// frame (PID 0x0111) with a PCR packet (PID 0x01FF) every 10 data
    /// packets, PCR advancing so that the rate is exactly that.
    fn timed_input(params: &Params, per_frame: u64, frames: u64) -> Vec<Packet> {
        let ft = params.frame_ticks_27mhz();
        // 11 packets (10 data + 1 PCR) per PCR interval
        let per_pkt = ft / (per_frame + per_frame / 10);
        let mut v = Vec::new();
        let mut n = 0u32;
        let total = per_frame * frames;
        let mut i = 0u64;
        while u64::from(n) < total {
            if i.is_multiple_of(11) {
                v.push(pcr_packet(0x01FF, 1_000_000 + i * per_pkt));
            } else {
                v.push(pkt(0x0111, n));
                n += 1;
            }
            i += 1;
        }
        v
    }

    fn mode3() -> Params {
        crate::phyargs::PhyArgs {
            mode: 3,
            gi: 8,
            partial: true,
            layer_a: "1,qpsk,2/3,2".into(),
            layer_b: "12,64qam,3/4,2".into(),
            layer_c: "none".into(),
        }
        .params()
        .unwrap()
    }

    #[test]
    fn mux_split_is_paced_by_pcr() {
        let p = mode3();
        let input = timed_input(&p, 50, 6);
        let (tx, rx) = sync_channel(1 << 14);
        let s = PacketStream::from_receiver("f", rx, Feed::File);
        let mut inp = MuxInput::new(s, &[0x01FF], &[]).paced(&p);
        for x in &input {
            tx.send(*x).unwrap();
        }
        drop(tx);
        let stop = AtomicBool::new(false);
        let mut got = Vec::new();
        let mut reals = Vec::new();
        while let Some(f) = inp.take_frame([20, 100, 0], &stop) {
            reals.push(f.real);
            got.extend(f.layers[1][..f.real[1]].iter().copied());
            assert_eq!(f.layers[1].len(), 100);
        }
        // 50 data packets per frame (±1 at frame edges), the rest is null
        // padding: the 100-packet layer is not filled ahead of the PCR.
        assert!(reals.len() >= 6, "{reals:?}");
        for r in &reals[..6] {
            assert!((49..=51).contains(&r[1]), "{reals:?}");
        }
        let want: Vec<Packet> = input.iter().filter(|x| pid(x) == 0x0111).copied().collect();
        assert_eq!(got, want, "order and content kept");
        assert!(inp.status().contains("PCR lag 0.00 s"), "{}", inp.status());
    }

    #[test]
    fn paced_split_over_capacity_lags() {
        let p = mode3();
        let input = timed_input(&p, 150, 8);
        let (tx, rx) = sync_channel(1 << 14);
        let s = PacketStream::from_receiver("f", rx, Feed::File);
        let mut inp = MuxInput::new(s, &[0x01FF], &[]).paced(&p);
        for x in &input {
            tx.send(*x).unwrap();
        }
        drop(tx);
        let stop = AtomicBool::new(false);
        let mut frames = 0;
        let mut total = 0;
        while let Some(f) = inp.take_frame([20, 100, 0], &stop) {
            frames += 1;
            total += f.real[1];
        }
        assert_eq!(total, 1200, "every packet sent");
        assert_eq!(frames, 12, "1200 packets / 100 per frame");
        assert!(inp.saturated[1] >= 10);
        // 1200 packets due over 8 frames go out over 12: the last ones are
        // ≈ 4 frames (0.9 s) late.
        let max_lag = inp.pacer.as_ref().unwrap().max_lag as f64 / 27e6;
        assert!((0.7..1.0).contains(&max_lag), "{max_lag} {}", inp.status());
    }

    #[test]
    fn synthetic_packets_are_distinct_and_valid() {
        let mut s = SyntheticInput::new();
        let stop = AtomicBool::new(false);
        let f1 = s.take_frame([3, 4, 0], &stop).unwrap();
        let f2 = s.take_frame([3, 4, 0], &stop).unwrap();
        assert_eq!(f1.layers[1].len(), 4);
        assert_ne!(f1.layers[1][0], f2.layers[1][0]);
        assert_eq!(pid(&f1.layers[0][0]), 0x100);
        assert_eq!(pid(&f2.layers[1][3]), 0x200);
        assert_eq!(f2.layers[1][3], SyntheticInput::packet(1, 7));
    }
}
