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
    ///
    /// # Panics
    ///
    /// If `fills` does not have one value per line.
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
    ///
    /// # Panics
    ///
    /// If `contents` does not have one vector per line, each of its line's
    /// delay.
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
    ///
    /// # Panics
    ///
    /// If `input` or `output` does not have one element per line.
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
    /// A ring of the held elements; the oldest is at `head`.
    ring: Vec<T>,
    head: usize,
}

impl<T: Copy> Fifo<T> {
    /// FIFO pre-filled with `len` copies of `fill`.
    pub fn new(len: usize, fill: T) -> Self {
        Self::from_vec(vec![fill; len])
    }

    /// FIFO holding `contents`, oldest first: they are the first
    /// `contents.len()` elements out.
    pub fn from_vec(contents: Vec<T>) -> Self {
        Self {
            ring: contents,
            head: 0,
        }
    }

    /// Number of elements held (the delay).
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// True for a zero delay.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Push a block and pop the same number of elements: each element
    /// trades places with the oldest one held.
    pub fn process(&mut self, mut data: &mut [T]) {
        let len = self.ring.len();
        while !data.is_empty() && len > 0 {
            let n = (len - self.head).min(data.len());
            let (now, rest) = data.split_at_mut(n);
            now.swap_with_slice(&mut self.ring[self.head..self.head + n]);
            self.head = (self.head + n) % len;
            data = rest;
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
        // Blocks shorter and longer than the ring, across its wrap.
        let mut f = Fifo::from_vec(vec![10u32, 11, 12]);
        let mut out = Vec::new();
        for block in [
            &[0u32][..],
            &[1, 2, 3, 4],
            &[],
            &[5, 6],
            &[7, 8, 9, 10, 11, 12, 13],
        ] {
            let mut b = block.to_vec();
            f.process(&mut b);
            out.extend(b);
        }
        let want: Vec<u32> = [10, 11, 12].into_iter().chain(0..11).collect();
        assert_eq!(out, want);
        assert_eq!(f.len(), 3);
        let mut z = Fifo::new(0, 0u8);
        let mut d = [4, 5];
        z.process(&mut d);
        assert_eq!(d, [4, 5]);
        assert!(z.is_empty());
    }
}
