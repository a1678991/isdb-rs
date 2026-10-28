//! The block pipeline of the satellite receivers (`isdb rx --system isdbs`
//! / `isdbs3`): reading or capturing, the front end, and the receiver run
//! at once, on their own threads, with bounded memory.
//!
//! Blocks are contiguous stretches of samples: recording segments or radio
//! captures, separated by gaps, so each block's front end is independent
//! of the others' given an acquisition (carrier, trim). The first block
//! acquires; every later block tries the first block's acquisition (the
//! front end verifies it on the block's first samples and re-acquires if
//! it no longer holds). That makes every block's result depend only on its
//! samples and the first block, so the later blocks' front ends run on
//! [`FRONT_WORKERS`] threads at once, with the same output as one after
//! another; the receiver takes the blocks in order.

use anyhow::Result;
use isdb_iq::C32;
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

/// Front ends running at once (blocks after the first). The front end is
/// one sequential stream per block (the bit-exact model's timing loop), so
/// this is what lets the front-end stage keep up when its blocks come
/// faster than one front end processes them (recordings; a radio's
/// captures are slower than one front end). Each block in flight holds its
/// samples and symbols (≈ 0.9 GB for a 0.83 s block at 40 Msps).
pub const FRONT_WORKERS: usize = 3;

/// One contiguous block of samples and their rate.
pub struct Block {
    /// Unit-scale samples.
    pub x: Vec<C32>,
    /// Sample rate, Hz.
    pub fs: f64,
}

/// A block's front-end result: its symbols, sample count, front-end
/// seconds and whether it reused the first block's acquisition.
type Fronted<S> = Result<(S, u64, f64, bool)>;

/// Stage times and acquisition reuse, for the summary.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timing {
    /// Seconds in the front end, summed over blocks.
    pub front_s: f64,
    /// Seconds in the receiver, summed over blocks.
    pub back_s: f64,
    /// Seconds from the start of the pipeline to the last block written.
    pub wall_s: f64,
    /// Blocks that reused the first block's acquisition.
    pub reused: u64,
}

/// Run `produce` (which sends blocks) → `front` (a block, the first
/// block's acquisition if any → symbols, this block's acquisition) →
/// `back` (symbols and the block's sample count, in block order), then
/// `after` with the running [`Timing`].
pub fn run<S, P, B>(
    produce: impl FnOnce(&SyncSender<Block>) -> Result<()> + Send,
    front: impl Fn(&Block, Option<&P>) -> Result<(S, P)> + Sync,
    mut back: B,
    mut after: impl FnMut(Timing),
) -> Result<()>
where
    S: Send,
    P: Clone + PartialEq + Send + Sync,
    B: FnMut(S, u64) -> Result<()>,
{
    let (btx, brx) = sync_channel::<Block>(1);
    // results may arrive out of order: at most FRONT_WORKERS in flight
    let (stx, srx) = sync_channel::<(usize, Fronted<S>)>(FRONT_WORKERS);
    let t0 = Instant::now();
    let mut timing = Timing::default();
    std::thread::scope(|sc| -> Result<()> {
        let producer = sc.spawn(move || produce(&btx));
        let front = &front;
        sc.spawn(move || front_stage(brx, stx, front));
        let mut pending: BTreeMap<usize, Fronted<S>> = BTreeMap::new();
        let mut next = 0usize;
        for (i, r) in srx {
            pending.insert(i, r);
            while let Some(r) = pending.remove(&next) {
                next += 1;
                let (s, n, front_s, reused) = r?;
                let t = Instant::now();
                back(s, n)?;
                timing.back_s += t.elapsed().as_secs_f64();
                timing.front_s += front_s;
                timing.reused += u64::from(reused);
                timing.wall_s = t0.elapsed().as_secs_f64();
                after(timing);
            }
        }
        // a panic of the producer is re-raised, not turned into an error
        producer
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// The front-end stage: the first block alone (its acquisition is every
/// later block's prior), then the rest on [`FRONT_WORKERS`] threads, each
/// result tagged with its block index.
fn front_stage<S, P>(
    blocks: Receiver<Block>,
    out: SyncSender<(usize, Fronted<S>)>,
    front: &(impl Fn(&Block, Option<&P>) -> Result<(S, P)> + Sync),
) where
    S: Send,
    P: Clone + PartialEq + Send + Sync,
{
    let one = |b: Block, prior: Option<&P>| -> (Fronted<S>, Option<P>) {
        let t = Instant::now();
        match front(&b, prior) {
            Ok((s, p)) => {
                let reused = prior == Some(&p);
                (
                    Ok((s, b.x.len() as u64, t.elapsed().as_secs_f64(), reused)),
                    Some(p),
                )
            }
            Err(e) => (Err(e), None),
        }
    };
    let Ok(first) = blocks.recv() else { return };
    let (r, prior) = one(first, None);
    let failed = r.is_err();
    if out.send((0, r)).is_err() || failed {
        return;
    }
    let Some(prior) = prior else { return };
    let queue = Arc::new(Mutex::new(blocks.into_iter().enumerate()));
    std::thread::scope(|sc| {
        for _ in 0..FRONT_WORKERS {
            let (queue, out, prior) = (Arc::clone(&queue), out.clone(), &prior);
            sc.spawn(move || {
                loop {
                    // The queue is only advanced: a worker that panicked
                    // while holding the lock leaves it consistent.
                    let Some((i, b)) = queue.lock().unwrap_or_else(PoisonError::into_inner).next()
                    else {
                        return;
                    };
                    let (r, _) = one(b, Some(prior));
                    let failed = r.is_err();
                    if out.send((i + 1, r)).is_err() || failed {
                        return;
                    }
                }
            });
        }
    });
}
