//! Running independent work on all cores with results in order.

/// `f` over `items` on all cores, results in input order (each item
/// computed exactly as a sequential `map` would).
pub fn map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let workers = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let chunk = items.len().div_ceil(workers).max(1);
    let f = &f;
    std::thread::scope(|sc| {
        let hs: Vec<_> = items
            .chunks(chunk)
            .map(|c| sc.spawn(move || c.iter().map(f).collect::<Vec<R>>()))
            .collect();
        hs.into_iter()
            .flat_map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    })
}

/// The first index in `range` with the largest `f` (strictly greater
/// replaces, as a sequential scan from the start; values ≤ `floor` never
/// win and the start is returned with `floor`), evaluated on all cores.
pub fn first_max(
    range: std::ops::Range<usize>,
    floor: f64,
    f: impl Fn(usize) -> f64 + Sync,
) -> (f64, usize) {
    let workers = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
    let len = range.len();
    let chunk = len.div_ceil(workers).max(1);
    let f = &f;
    let start = range.start;
    let parts: Vec<(f64, usize)> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..len)
            .step_by(chunk)
            .map(|lo| {
                sc.spawn(move || {
                    let mut best = (floor, start);
                    for i in start + lo..start + (lo + chunk).min(len) {
                        let m = f(i);
                        if m > best.0 {
                            best = (m, i);
                        }
                    }
                    best
                })
            })
            .collect();
        hs.into_iter()
            .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
            .collect()
    });
    let mut best = (floor, start);
    for p in parts {
        if p.0 > best.0 {
            best = p;
        }
    }
    best
}
