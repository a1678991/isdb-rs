//! Fixed delay lines used by the bit interleaver (§3.9.3), the time
//! interleaver (§3.11.1) and the receiver's alignment buffers.

/// A bank of independent FIFO delay lines advancing in lock-step: every
/// [`MultiDelay::push`] feeds one element into each line and returns the
/// element that entered `delay[i]` pushes earlier (or the fill value while
/// the line is priming).  Memory is `Σ delay[i]`.
#[derive(Debug, Clone)]
pub struct MultiDelay<T> {
    delays: Vec<usize>,
    base: Vec<usize>,
    buf: Vec<T>,
    t: usize,
}

impl<T: Copy> MultiDelay<T> {
    /// Lines with the given delays (in pushes), initially holding `fill`.
    pub fn new(delays: Vec<usize>, fill: T) -> Self {
        let mut base = Vec::with_capacity(delays.len());
        let mut acc = 0;
        for &d in &delays {
            base.push(acc);
            acc += d;
        }
        Self {
            delays,
            base,
            buf: vec![fill; acc],
            t: 0,
        }
    }

    /// Lines with the given delays, line `i` initially holding `fills[i]`.
    pub fn with_fills(delays: Vec<usize>, fills: &[T]) -> Self {
        assert_eq!(delays.len(), fills.len());
        let mut buf = Vec::with_capacity(delays.iter().sum());
        let mut base = Vec::with_capacity(delays.len());
        for (&d, &f) in delays.iter().zip(fills) {
            base.push(buf.len());
            buf.extend(std::iter::repeat_n(f, d));
        }
        Self {
            delays,
            base,
            buf,
            t: 0,
        }
    }

    /// Lines with the given delays and explicit initial contents:
    /// `contents[i]` (length `delays[i]`) is output by line `i` during the
    /// first `delays[i]` pushes, in order.
    pub fn with_contents(delays: Vec<usize>, contents: Vec<Vec<T>>) -> Self {
        assert_eq!(delays.len(), contents.len());
        let mut buf = Vec::with_capacity(delays.iter().sum());
        let mut base = Vec::with_capacity(delays.len());
        for (&d, c) in delays.iter().zip(contents) {
            assert_eq!(c.len(), d);
            base.push(buf.len());
            buf.extend(c);
        }
        Self {
            delays,
            base,
            buf,
            t: 0,
        }
    }

    /// Number of lines.
    pub fn lines(&self) -> usize {
        self.delays.len()
    }

    /// Delay of line `i`.
    pub fn delay(&self, i: usize) -> usize {
        self.delays[i]
    }

    /// Push one element per line (`input.len() == lines()`), writing the
    /// delayed outputs to `output`.
    pub fn push(&mut self, input: &[T], output: &mut [T]) {
        assert_eq!(input.len(), self.delays.len());
        assert_eq!(output.len(), self.delays.len());
        for (i, (&x, y)) in input.iter().zip(output.iter_mut()).enumerate() {
            let d = self.delays[i];
            if d == 0 {
                *y = x;
            } else {
                let k = self.base[i] + self.t % d;
                *y = self.buf[k];
                self.buf[k] = x;
            }
        }
        self.t = self.t.wrapping_add(1);
        // Keep `t % d` consistent across wrap-around: reset well before overflow.
        if self.t == usize::MAX / 2 {
            self.rebase();
        }
    }

    fn rebase(&mut self) {
        // Rotate every line so that the next read index is 0 again.
        for i in 0..self.delays.len() {
            let d = self.delays[i];
            if d > 0 {
                let r = self.t % d;
                self.buf[self.base[i]..self.base[i] + d].rotate_left(r);
            }
        }
        self.t = 0;
    }
}

/// A single FIFO of `len` elements over a stream (used for whole-stream
/// alignment delays).
#[derive(Debug, Clone)]
pub struct Fifo<T> {
    buf: std::collections::VecDeque<T>,
}

impl<T: Copy> Fifo<T> {
    /// FIFO pre-filled with `len` copies of `fill`.
    pub fn new(len: usize, fill: T) -> Self {
        Self {
            buf: std::iter::repeat_n(fill, len).collect(),
        }
    }

    /// Push a block and pop the same number of elements.
    pub fn process(&mut self, data: &mut [T]) {
        if self.buf.is_empty() {
            return;
        }
        for x in data.iter_mut() {
            self.buf.push_back(*x);
            *x = self.buf.pop_front().expect("non-empty");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multidelay_delays() {
        let mut m = MultiDelay::new(vec![0, 1, 3], -1i32);
        let mut out = [0; 3];
        let mut got = Vec::new();
        for t in 0..6 {
            m.push(&[t, t, t], &mut out);
            got.push(out);
        }
        assert_eq!(
            got,
            vec![
                [0, -1, -1],
                [1, 0, -1],
                [2, 1, -1],
                [3, 2, 0],
                [4, 3, 1],
                [5, 4, 2]
            ]
        );
    }

    #[test]
    fn multidelay_rebase_keeps_order() {
        let mut m = MultiDelay::new(vec![3, 5], 0u32);
        let mut out = [0; 2];
        for t in 0..20u32 {
            m.push(&[t, t], &mut out);
        }
        m.rebase();
        m.push(&[20, 20], &mut out);
        assert_eq!(out, [17, 15]);
    }

    #[test]
    fn fifo() {
        let mut f = Fifo::new(2, 0u8);
        let mut d = [1, 2, 3];
        f.process(&mut d);
        assert_eq!(d, [0, 0, 1]);
    }
}
