//! Convert TTML caption documents (the `.ttml` files `isdb tlv extract` /
//! extracted elementary streams hold, in arrival order) to SRT or WebVTT on the
//! document clock (development tool).
//!
//! cargo run -p isdb-caption --example ttml2srt -- [--vtt] FILE.ttml...

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let vtt = args.first().is_some_and(|a| a == "--vtt");
    if vtt {
        args.remove(0);
    }
    let mut t = isdb_caption::Timeline::new();
    for path in &args {
        let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        match isdb_caption::parse(&bytes) {
            Ok(doc) => t.push(&doc, Some, None),
            Err(e) => eprintln!("{path}: {e}"),
        }
    }
    let cues = t.finish(None);
    print!(
        "{}",
        if vtt {
            isdb_caption::to_vtt(&cues)
        } else {
            isdb_caption::to_srt(&cues)
        }
    );
}
