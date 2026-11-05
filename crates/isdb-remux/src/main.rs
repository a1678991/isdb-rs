//! `isdb-remux`: a clear ISDB-S3 TLV stream (a file or stdin) to a timed
//! MPEG-TS or a Matroska file of one service, every track with the MPT's
//! clock, its captions as SubRip / WebVTT.
//!
//! ```notcode
//! isdb-remux -i dec.tlv --mkv -o out.mkv            # every track, one file
//! isdb-remux -i dec.tlv --mkv --split-events         # one file per event, by the input
//! isdb-remux -i dec.tlv --ts -o out.ts              # + out.ja.srt / .vtt
//! DESCRAMBLER < rec.tlv | isdb-remux --ts > out.ts  # straight from a descrambler
//! ```
//!
//! The TS goes to stdout when no `-o` is given, but not onto a terminal
//! (refused, as curl does); `-o -` writes it to stdout anyway.

use clap::Parser;
use isdb_remux::split::{DEFAULT_TEMPLATE, Template};
use isdb_remux::{Remux, mkv::Mkv, output, ts::Session};
use std::path::{Path, PathBuf};

/// A `0x…` or decimal u16.
fn parse_u16(s: &str) -> Result<u16, String> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u16::from_str_radix(h, 16).map_err(|e| e.to_string()),
        None => s.parse::<u16>().map_err(|e| e.to_string()),
    }
}

/// The `isdb-remux` arguments.
#[derive(Parser, Debug)]
#[command(name = "isdb-remux", version, about)]
#[command(group(clap::ArgGroup::new("form").required(true).args(["ts", "mkv"])))]
struct Args {
    /// The clear TLV stream (stdin when none): never scrambled, or the
    /// output of a cardholder's descrambler.
    #[arg(short = 'i', long)]
    input: Option<PathBuf>,
    /// The output file. `--ts` without it goes to stdout, unless stdout
    /// is a terminal (refused; `-o -` writes there anyway). With
    /// `--split-events`, the directory (made if missing; default: the input's).
    #[arg(short = 'o', long)]
    output: Option<PathBuf>,
    /// An MPEG-TS: the video, every audio, the data assets as private
    /// data, with the PTS/DTS of the MPTs' timestamp descriptors; the
    /// captions as `.srt` / `.vtt` next to `-o`.
    #[arg(long)]
    ts: bool,
    /// A Matroska file of every track: the HEVC video, every AAC audio
    /// (raw AAC, not re-encoded), the captions as text tracks, a chapter per
    /// MH-EIT event, the data broadcasting packages as attachments. Needs `-o` (the file is
    /// finished by seeking back) and room for a scratch copy of the media
    /// next to it (`OUT.part`, removed at the end).
    #[arg(long)]
    mkv: bool,
    /// With `--mkv`: one file per MH-EIT event instead of one with chapters,
    /// each from the stream's real switch, with its own tracks and timeline; a
    /// stretch no event covers gets a file too. The files appear at the end of the
    /// run, and an existing file is not overwritten (the name takes a `-N`).
    #[arg(long, requires = "mkv", conflicts_with = "ts")]
    split_events: bool,
    /// With `--split-events`: the file name. `${START_TIME}` is the first
    /// picture's presentation time in JST, `2026-11-05_02-24-02.698`;
    /// `${EVENT_NAME}` the MH-EIT's; `${CHANNEL_NAME}` the MH-SDT's service name.
    #[arg(long, requires = "split_events", value_name = "TEMPLATE", default_value = DEFAULT_TEMPLATE)]
    split_name: Template,
    /// With `--ts`: where the captions' `.srt` / `.vtt` go (default: next
    /// to `-o`, as `OUT.LANG.srt`; none on stdout without this).
    #[arg(long, requires = "ts")]
    captions: Option<PathBuf>,
    /// The service to record (`0x0097` or decimal; default: the first
    /// whose MPT arrives).
    #[arg(long, value_parser = parse_u16)]
    service: Option<u16>,
    /// Log verbosely.
    #[arg(short = 'v')]
    verbose: bool,
    /// No report but errors (and no progress).
    #[arg(short = 'q')]
    quiet: bool,
    /// No progress line while the input is read (it goes to stderr: on a
    /// terminal one line redrawn in place, otherwise a line every 10 s).
    #[arg(long)]
    no_progress: bool,
}

fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    tracing_subscriber::fmt()
        .with_max_level(if a.verbose {
            tracing::Level::DEBUG
        } else {
            tracing::Level::WARN
        })
        .without_time()
        .compact()
        .with_writer(std::io::stderr)
        .init();
    if a.mkv && !a.split_events && a.output.is_none() {
        anyhow::bail!("--mkv needs -o FILE (or --split-events)");
    }
    let to_stdout = output::is_stdout(a.output.as_deref());
    let out_name = match &a.output {
        Some(p) if !to_stdout => p.display().to_string(),
        _ => "stdout".to_owned(),
    };
    let app = concat!("isdb-remux ", env!("CARGO_PKG_VERSION"));
    let session = if a.split_events {
        if a.output.as_deref() == Some(Path::new("-")) {
            anyhow::bail!("--split-events writes files: give -o DIR, not -o -");
        }
        let dir = split_dir(&a)?;
        let mkv = Mkv::create_split(&dir, a.split_name.clone(), app)
            .map_err(|e| anyhow::anyhow!("cannot write in {}: {e}", dir.display()))?;
        Session::new(Box::new(std::io::sink()), a.service)?.with_mkv(mkv)
    } else if a.mkv {
        let path = a.output.as_ref().expect("clap: --mkv requires -o");
        if to_stdout {
            anyhow::bail!("--mkv writes a file it seeks back into: give -o FILE, not -o -");
        }
        let mkv = Mkv::create(path, app)
            .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", path.display()))?;
        Session::new(Box::new(std::io::sink()), a.service)?.with_mkv(mkv)
    } else {
        let out = output::open(a.output.as_deref(), "the MPEG-TS")?;
        Session::new(out, a.service)?
    };
    let (raw, total): (Box<dyn std::io::Read>, Option<u64>) = match &a.input {
        Some(p) => {
            let f = std::fs::File::open(p)
                .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", p.display()))?;
            let total = f
                .metadata()
                .ok()
                .filter(std::fs::Metadata::is_file)
                .map(|m| m.len());
            (Box::new(f), total)
        }
        None => (Box::new(std::io::stdin().lock()), None),
    };
    let mut progress = isdb_remux::progress::Progress::new(raw, total, !a.quiet && !a.no_progress);
    let mut remux = Remux::new(session);
    remux.run(std::io::BufReader::new(&mut progress))?;
    progress.finish();
    if a.mkv && !a.quiet && !a.no_progress {
        // The Matroska form's second pass (the file in decode order).
        if a.split_events {
            eprintln!("writing the files …");
        } else {
            eprintln!("writing {out_name} …");
        }
    }
    let isdb_remux::Report {
        counts: c,
        captions,
        stats,
        assets,
        written,
    } = remux.finish()?;
    let caption_lines = if a.ts {
        write_captions(&a, &captions)?
    } else {
        Vec::new()
    };
    if a.quiet {
        return Ok(());
    }
    if a.split_events {
        eprintln!(
            "mkv: {} file(s) ({} at the scheduled time, no switch seen); {} picture(s), {} audio frame(s), {} data item(s); {} AU(s) before an MPU start or after a loss, {} untimed; {}",
            written.len(),
            c.chapters_scheduled,
            c.video,
            c.audio,
            c.data,
            c.unsynced,
            c.untimed,
            loss_line(&c)
        );
        for w in &written {
            eprintln!(
                "  {}: {} track(s), {} attachment(s), {} frame(s)",
                w.path.display(),
                w.tracks,
                w.attachments,
                w.frames
            );
        }
    } else if a.mkv {
        eprintln!(
            "mkv {out_name}: {} track(s) ({} caption track(s), {} cue(s)), {} chapter(s) ({} at the scheduled time, no switch seen), {} attachment(s); {} picture(s), {} audio frame(s), {} data item(s); {} AU(s) before an MPU start or after a loss, {} untimed; {}",
            c.streams,
            captions.iter().filter(|t| !t.cues.is_empty()).count(),
            captions.iter().map(|t| t.cues.len()).sum::<usize>(),
            c.chapters,
            c.chapters_scheduled,
            c.attachments,
            c.video,
            c.audio,
            c.data,
            c.unsynced,
            c.untimed,
            loss_line(&c)
        );
    } else {
        eprintln!(
            "ts {out_name}: {} stream(s); {} picture(s), {} audio frame(s), {} data item(s); {} AU(s) before an MPU start or after a loss, {} untimed; {}",
            c.streams,
            c.video,
            c.audio,
            c.data,
            c.unsynced,
            c.untimed,
            loss_line(&c)
        );
    }
    for l in &caption_lines {
        eprintln!("{l}");
    }
    eprintln!(
        "TLV packets {}; recorded assets' MMTP packets {}{}",
        stats.packets,
        stats.media_packets,
        if stats.scrambled > 0 {
            format!(
                " ({} still scrambled, skipped: descramble the stream first)",
                stats.scrambled
            )
        } else {
            String::new()
        }
    );
    for (pid, s) in assets {
        if s.packets_lost > 0 || s.mfus_dropped > 0 {
            eprintln!(
                "asset {pid:#06x}: {} MMTP packet(s), {} lost, {} MFU(s), {} dropped",
                s.packets, s.packets_lost, s.mfus, s.mfus_dropped
            );
        }
    }
    Ok(())
}

/// Where `--split-events` writes: `-o`'s directory (made if missing), else the
/// input's. Standard input has none, and must say.
fn split_dir(a: &Args) -> anyhow::Result<PathBuf> {
    if let Some(dir) = &a.output {
        if dir.exists() && !dir.is_dir() {
            anyhow::bail!("--split-events: -o {} is not a directory", dir.display());
        }
        std::fs::create_dir_all(dir)
            .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", dir.display()))?;
        return Ok(dir.clone());
    }
    let Some(input) = &a.input else {
        anyhow::bail!("--split-events from standard input: give -o DIR");
    };
    Ok(match input.parent() {
        Some(p) if p != Path::new("") => p.to_owned(),
        _ => PathBuf::from("."),
    })
}

/// The recorder's loss handling, for the report.
fn loss_line(c: &isdb_remux::ts::Counts) -> String {
    format!(
        "after lost packets: {} picture(s) placed by POC, {} not written (no reference), {} audio frame(s) stepped over, {} POC mismatch(es)",
        c.placed, c.skipped, c.audio_lost, c.misplaced
    )
}

/// The caption sidecars of `--ts`: per caption asset, `STEM.TAG.srt` and
/// `STEM.TAG.vtt` (TAG the documents' language, and the packet id when
/// two assets share one) in `--captions` or next to `-o`. The report
/// lines.
fn write_captions(
    a: &Args,
    tracks: &[isdb_remux::ts::CaptionTrack],
) -> anyhow::Result<Vec<String>> {
    let base = match (&a.captions, &a.output) {
        (Some(dir), out) => {
            std::fs::create_dir_all(dir)
                .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", dir.display()))?;
            let stem = out
                .as_ref()
                .and_then(|o| o.file_stem())
                .map_or_else(|| "captions".into(), |s| s.to_os_string());
            dir.join(stem)
        }
        (None, Some(out)) if !output::is_stdout(Some(out)) => out.with_extension(""),
        (None, _) => {
            return Ok(if tracks.iter().any(|t| !t.cues.is_empty()) {
                vec!["captions: not written (the TS goes to stdout; give --captions DIR)".into()]
            } else {
                Vec::new()
            });
        }
    };
    let mut lines = Vec::new();
    for t in tracks {
        let lang = t.lang.clone().unwrap_or_else(|| "und".into());
        let shared = tracks.iter().filter(|o| o.lang == t.lang).count() > 1;
        let tag = if shared {
            format!("{lang}.{:04x}", t.pid)
        } else {
            lang
        };
        let mut report = format!("captions {:#06x}: {} cue(s)", t.pid, t.cues.len());
        if t.rejected > 0 {
            report.push_str(&format!(", {} document(s) rejected", t.rejected));
        }
        if !t.cues.is_empty() {
            for (ext, text) in [
                ("srt", isdb_caption::to_srt(&t.cues)),
                ("vtt", isdb_caption::to_vtt(&t.cues)),
            ] {
                let mut path = base.clone().into_os_string();
                path.push(format!(".{tag}.{ext}"));
                let path = PathBuf::from(path);
                std::fs::write(&path, text)
                    .map_err(|e| anyhow::anyhow!("cannot write {}: {e}", path.display()))?;
                report.push_str(&format!(" → {}", path.display()));
            }
        }
        lines.push(report);
    }
    Ok(lines)
}
