//! A `ts-program` relay configuration without a TOML file: the values of
//! `examples/relay-ts.toml` as a template filled from a few options
//! (`isdb tx|mux|loopback --relay FILE`, [`RelayOptions`]).
//!
//! The configuration is produced as TOML text and parsed with
//! [`Config::from_toml`], so what `--print-config` prints is exactly what
//! runs: saving it and passing it with `--mux-config` / `-c` gives the
//! same multiplex.

use std::path::PathBuf;

use crate::config::{AtEof, CaMode, Config, LayerName};
use crate::error::Result;

/// Network / service name used when neither `name` nor the source SDT
/// gives one (the name of `examples/relay-ts.toml`).
pub const DEFAULT_NAME: &str = "ベンチ";

/// Options of a relay configuration; everything else is the value of
/// `examples/relay-ts.toml` (network_id / transport_stream_id 0x79EE, a
/// placeholder, service 0x0400, PMT PID 0x01F0, dedicated PCR PID 0x01FF,
/// roles → PIDs by the default plan, rate_check "error", the fallback
/// event, TOT, BIT, a CAT with the source's CA_descriptors only).
#[derive(Debug, Clone, PartialEq)]
pub struct RelayOptions {
    /// The recorded TS (written as given; pass an absolute path for a
    /// configuration that is used elsewhere).
    pub source: PathBuf,
    /// Source program_number (= service_id); `None`: [`crate::relay::probe`]'s default.
    pub program: Option<u16>,
    /// Source SDT service name selecting the program.
    pub service_name: Option<String>,
    /// UHF channel of the NIT (must be the transmit channel).
    pub channel: u32,
    /// remote_control_key_id (1..=12).
    pub remote_control_key_id: u8,
    /// area_code of the terrestrial delivery system descriptor (12 bits).
    pub area_code: u16,
    /// Network name, TS name, service name, provider and broadcaster name.
    pub name: String,
    /// digital_recording_control_data by name: "source" (default: the
    /// relayed program's own value, see
    /// [`crate::signalling::RecordingControlCfg`]) or a B10 Table 6-39
    /// name ("copy-free", "copy-one-generation", "copy-never", …), used as
    /// given.
    pub recording_control: String,
    /// The source's conditional-access signalling ([`CaMode`]).
    pub ca: CaMode,
    /// End-of-file behaviour.
    pub at_eof: AtEof,
    /// EIT p/f from the source.
    pub eit_from_source: bool,
    /// Layer of the service (B with partial reception; A for a single
    /// 13-segment layer).
    pub layer: LayerName,
    /// Fixed JST start `YYYY-MM-DDTHH:MM:SS` (reproducible output);
    /// `None`: the system clock.
    pub clock_start: Option<String>,
    /// Comment lines put at the top (without `#`).
    pub header: Vec<String>,
}

impl RelayOptions {
    /// The values of `examples/relay-ts.toml` for `source`: channel 44,
    /// remote key 9, area code 0x5A5, name "ベンチ", copy control from the
    /// source, CA kept, loop, EIT from the source, layer B, system clock.
    pub fn new(source: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            program: None,
            service_name: None,
            channel: 44,
            remote_control_key_id: 9,
            area_code: 0x5A5,
            name: DEFAULT_NAME.to_string(),
            recording_control: crate::signalling::RecordingControlCfg::SOURCE.to_string(),
            ca: CaMode::Keep,
            at_eof: AtEof::Loop,
            eit_from_source: true,
            layer: LayerName::B,
            clock_start: None,
            header: Vec::new(),
        }
    }

    /// The configuration as TOML text (commented like the example).
    pub fn to_toml(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let name = q(&self.name);
        let mut t = String::new();
        for h in &self.header {
            t += &format!("# {h}\n");
        }
        t += "# isdbt-mux relay configuration (the values of examples/relay-ts.toml).\n";
        t += "# Integers may be written in hex (0x...).\n\n";
        t += &format!(
            "[network]\n\
             network_id = 0x79EE              # placeholder in the B10 Annex N terrestrial range 0x7880–0x7FE8: use an ID not used where the receiver is\n\
             name = {name}\n\
             area_code = {:#05X}                # B10 Annex D Table D-2: the receiver's area\n\
             remote_control_key_id = {}        # one-touch key\n\
             channels = [{}]                  # MUST equal the channel `isdb tx` transmits on\n\n",
            self.area_code, self.remote_control_key_id, self.channel
        );
        t += "[transport]\ntransport_stream_id = 0x79EE\n\n";
        match &self.clock_start {
            Some(s) => t += &format!("[clock]\nsource = \"fixed\"\nstart = {}\n\n", q(s)),
            None => {
                t += "[clock]\nsource = \"system\"                # TOT and EIT \"now\" = the wall clock at start\n\n"
            }
        }
        t += &format!(
            "[[service]]\n\
             service_id = 0x0400\n\
             service_type = 0x01              # digital TV service (B10 Table 6-25)\n\
             provider = {name}\n\
             name = {name}\n\
             layer = \"{:?}\"\n\
             pmt_pid = 0x01F0\n\
             pcr_pid = 0x01FF                 # dedicated PCR PID: relayed packets keep their payload\n\
             # recording_control = \"source\": the relayed program's digital_recording_control_data\n\
             # (B10 Table 6-39; PMT, else ES loops, else SDT, else EIT), ES-loop copy control kept;\n\
             # a Table 6-39 name is used as given. copy_control_type '01' as receivers require\n\
             copy_control = {{ recording_control = {}, in_pmt = true, in_sdt = true }}\n\n",
            self.layer,
            q(&self.recording_control)
        );
        t += "  [[service.stream]]\n  kind = \"ts-program\"\n";
        t += &format!("  source = {}\n", q(&self.source.to_string_lossy()));
        match (self.program, &self.service_name) {
            (Some(p), _) => {
                t += &format!(
                    "  program = {p:#06X}               # program_number (= service_id) in the recording's PAT\n"
                )
            }
            (None, Some(n)) => {
                t += &format!(
                    "  service_name = {}         # source SDT service name\n",
                    q(n)
                )
            }
            (None, None) => {
                t += "  # program = 0x0101 / service_name = \"...\": default = the recording's only program\n"
            }
        }
        t += "  # Output PIDs by role (pid_map = \"auto\"); override the plan with\n";
        t += "  # pids = { video = 0x0111, audio = [0x0112, 0x0113], captions = 0x0130, superimpose = 0x0138, data = 0x0140 }\n";
        t += "  # or list them: pid_map = [[0x0140, 0x0111], [0x0141, 0x0112]] (\"keep\": source PIDs).\n";
        if self.ca == CaMode::Drop {
            t += "  ca = \"drop\"                   # source CA descriptors, ECM and EMM not relayed\n";
        } else {
            t += "  # ca = \"keep\" (default): source CA descriptors, ECM and EMM relayed (payload unscrambled); \"drop\": none\n";
        }
        t += &format!(
            "  at_eof = \"{}\"                # \"loop\" or \"end\"\n",
            match self.at_eof {
                AtEof::Loop => "loop",
                AtEof::End => "end",
            }
        );
        t += &format!(
            "  eit_from_source = {}         # EIT p/f event name / text / genre / duration from the recording\n",
            self.eit_from_source
        );
        t += "  rate_check = \"error\"           # refuse a layer that cannot carry the rate the whole file needs\n";
        t += "  max_early_ms = 0\n\n";
        t += "  # Used when the recording has no EIT p/f for the program (or eit_from_source = false).\n";
        t += "  [[service.event]]\n  event_id = 0x0001\n  start_offset_s = -60\n  duration = \"01:00:00\"\n  name = \"録画の再送信\"\n\n";
        t += "[eit]\npresent_running_status = 4\nfollowing_running_status = 1\n\n";
        t += "[tot]\nenabled = true\n\n";
        t += &format!(
            "[bit]\nenabled = true\nbroadcast_view_propriety = false\nbroadcasters = [\n  {{ broadcaster_id = 0x01, name = {name} }},\n]\n\n"
        );
        t += "[cat]\ndescriptors = []                 # none of its own; with ca = \"keep\" the source CAT's CA_descriptors are added\n";
        t
    }

    /// The parsed configuration ([`Config::from_toml`] of [`Self::to_toml`]).
    pub fn config(&self) -> Result<Config> {
        Config::from_toml(&self.to_toml())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_round_trip_through_toml() {
        let mut o = RelayOptions::new("/r/a \"b\" \\ c.ts");
        o.program = Some(0x0101);
        o.channel = 27;
        o.remote_control_key_id = 3;
        o.area_code = 0x5A5;
        o.name = "名前\"x\"".into();
        o.recording_control = "copy-one-generation".into();
        o.ca = CaMode::Drop;
        o.at_eof = AtEof::End;
        o.eit_from_source = false;
        o.layer = LayerName::A;
        o.clock_start = Some("2026-01-01T09:00:00".into());
        o.header = vec!["hello".into()];
        let text = o.to_toml();
        assert!(text.starts_with("# hello\n"), "{text}");
        let c = o.config().unwrap();
        assert_eq!(c.network.channels, vec![27]);
        assert_eq!(c.network.remote_control_key_id.get(), 3);
        assert_eq!(c.network.area_code, 0x5A5);
        assert_eq!(c.network.name, "名前\"x\"");
        let s = &c.services[0];
        assert_eq!(
            (s.service_id, s.pmt_pid.get(), s.pcr_pid.get()),
            (0x0400, 0x01F0, 0x01FF)
        );
        assert_eq!(s.layer, LayerName::A);
        assert_eq!(s.name, "名前\"x\"");
        let st = &s.streams[0];
        assert_eq!(st.source.as_deref(), Some("/r/a \"b\" \\ c.ts"));
        assert_eq!(st.program, Some(0x0101));
        assert_eq!(st.ca, CaMode::Drop);
        assert_eq!(st.at_eof, AtEof::End);
        assert!(!st.eit_from_source);
        assert!(st.pid_map.is_auto());
        assert_eq!(c.clock.start.as_deref(), Some("2026-01-01T09:00:00"));
        let mut by_name = RelayOptions::new("x.ts");
        by_name.service_name = Some("ＴＶ１２".into());
        assert_eq!(
            by_name.config().unwrap().services[0].streams[0]
                .service_name
                .as_deref(),
            Some("ＴＶ１２")
        );
        // the default keeps the source's copy restriction and CA signalling
        let d = RelayOptions::new("x.ts").config().unwrap();
        assert_eq!(d.services[0].streams[0].ca, CaMode::Keep);
        assert_eq!(d.network.area_code, 0x5A5);
        assert_eq!(
            d.services[0]
                .copy_control
                .as_ref()
                .unwrap()
                .recording_control,
            crate::signalling::RecordingControlCfg::Source
        );
        // an unknown copy-control name is refused by the typed config
        let mut bad = RelayOptions::new("x.ts");
        bad.recording_control = "copy-sometimes".into();
        assert!(bad.config().is_err());
    }
}
