//! TX link-rate probe for the Pluto backend: streams a low-level tone of
//! cs12 samples at a given rate into the AD9361's digital loopback (TX LO
//! powered down, maximum attenuation: nothing is radiated) and reports the
//! stream's underruns, to find the rate the link sustains in the transmit
//! direction (docs/plans/isdbs-fpga-tx.md §5).
//!
//! ```sh
//! cargo run --release -p isdb-radio --example txrate -- ip:10.10.0.30 8e6 20
//! ```
//!
//! Arguments: the libiio URI, the sample rate in Hz, the seconds to stream.
//!
//! The DAC core's sticky underflow flag (ADI `up_dac_common` REG 0x22 bit 0,
//! debug address 0x80000088 on `cf-ad9361-dds-core-lpc`) is cleared 2 s
//! into the stream and read again just before the stream ends, through a
//! second libiio context: it sets whenever the unpacker finds no DMA word,
//! which is also the idle state between streams, so only a read taken
//! while streaming says whether the link kept the DAC fed.

use isdb_radio::{C32, DeviceSpec, RadioConfig, TxOptions};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

const DAC_DEV: &str = "cf-ad9361-dds-core-lpc";
const DAC_STATUS_REG: u32 = 0x8000_0088;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, uri, rate, seconds] = &args[..] else {
        return Err("usage: txrate URI RATE_HZ SECONDS".into());
    };
    let rate: f64 = rate.parse()?;
    let seconds: f64 = seconds.parse()?;
    let spec: DeviceSpec = format!("pluto:uri={uri},loopback=on").parse()?;
    let backend = isdb_radio::backend(&spec.backend)?;
    // A 1 Hz granularity is enough for a rate probe.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rate_num = rate.round() as u32;
    let cfg = RadioConfig {
        center_hz: 1_049_480_000,
        nominal_hz: 1_049_480_000,
        rate_num,
        rate_den: 1,
        bandwidth_hz: 20_000_000,
        args: spec.args.clone(),
        gains: vec![],
    };
    // Frames of 0.1 s; the queue holds 2 of them before streaming starts.
    let frame_s = 0.1;
    let opts = TxOptions {
        prefill_frames: 2,
        frame_seconds: frame_s,
        backoff_db: 10.0,
    };
    println!("{}", backend.describe_tx(&cfg, &opts)?);
    let (mut sink, info) = backend.open_tx(&cfg, &opts)?;
    println!("opened: {info}");
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (rate * frame_s) as usize;
    // A tone at rate/64, unit RMS before the backoff.
    let frame: Vec<C32> = (0..n)
        .map(|i| {
            let t = i as f32 / 64.0 * std::f32::consts::TAU;
            C32::new(t.cos(), t.sin())
        })
        .collect();
    let stop = AtomicBool::new(false);
    let ctx = isdb_iio::Context::open(uri)?;
    let dac = ctx.device(DAC_DEV)?;
    let t0 = Instant::now();
    let mut frames = 0u64;
    let mut cleared = false;
    while t0.elapsed().as_secs_f64() < seconds {
        sink.write(&frame, &stop)?;
        frames += 1;
        if !cleared && t0.elapsed().as_secs_f64() >= 2.0 {
            dac.reg_write(DAC_STATUS_REG, 1)?;
            let v = dac.reg_read(DAC_STATUS_REG)?;
            println!(
                "DAC status 0x88 cleared at {:.1} s, reads {v:#x} right after",
                t0.elapsed().as_secs_f64()
            );
            cleared = true;
        }
        if frames.is_multiple_of(50) {
            println!("{:6.1} s{}", frames as f64 * frame_s, sink.status());
        }
    }
    let unf = dac.reg_read(DAC_STATUS_REG)?;
    println!("DAC status 0x88 while still streaming: {unf:#x} (bit 0 = underflow since the clear)");
    let fin = sink.finish(false)?;
    println!(
        "{fin}\n{frames} frames = {:.1} s on air at {:.3} Msps ({:.1} MB/s of cs12)",
        frames as f64 * frame_s,
        rate / 1e6,
        rate * 4.0 / 1e6
    );
    Ok(())
}
