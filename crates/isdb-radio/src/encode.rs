//! What a [`QueuedSink`](crate::QueuedSink) converts before queueing:
//! the [`Encode`] trait, implemented by [`Quantizer`] (unit-RMS samples
//! to integer I/Q) and by [`CodeEncoder`] (ISDB-S symbol codes to the
//! DMA words of an FPGA transmit block).

use crate::{C32, IntFormat, Quantizer};
use isdb_frontend::txformat::{self, CodePacker};

/// A conversion of what a transmitter writes into the buffer components a
/// radio's stream takes.
///
/// [`QueuedSink`](crate::QueuedSink) is an [`IqSink`](crate::IqSink) over
/// an encoder whose [`Encode::Input`] is [`C32`] and a
/// [`SymbolSink`](crate::SymbolSink) over one whose input is `u8`.
pub trait Encode {
    /// What is written: `C32` samples or `u8` symbol codes.
    type Input;
    /// One element of the radio's buffers (an I or Q component).
    type Component: Copy + Send + 'static;
    /// Short name for descriptions and events (`cs12`, `TX codes`).
    fn name(&self) -> &'static str;
    /// Convert `x` into `out` (cleared first); an encoder may hold a
    /// partial unit for the next call ([`Encode::flush`]).
    fn encode(&mut self, x: &[Self::Input], out: &mut Vec<Self::Component>);
    /// What the encoder still holds, as a last buffer into `out` (cleared
    /// first; empty when it holds nothing, the default).
    fn flush(&mut self, out: &mut Vec<Self::Component>) {
        out.clear();
    }
    /// Its counters, for status lines and the summary
    /// (`clipped 0 of 2000 components (0.00e0)`).
    fn describe(&self) -> String;
}

impl<F: IntFormat> Encode for Quantizer<F> {
    type Input = C32;
    type Component = F::Component;

    fn name(&self) -> &'static str {
        F::NAME
    }

    fn encode(&mut self, x: &[C32], out: &mut Vec<F::Component>) {
        self.convert_into(x, out);
    }

    fn describe(&self) -> String {
        Quantizer::describe(self)
    }
}

/// ISDB-S symbol codes (`isdb_frontend::txformat`, TX format
/// [`txformat::FORMAT_VERSION`]) into the two-channel DMA samples of an
/// FPGA transmit block: eight 4-bit codes per sample, the I word
/// (`voltage0`) then the Q word (`voltage1`) as `i16` components, the
/// first code in time in the lowest nibble of I. A partial sample is held
/// across calls and padded with zero symbols by [`Encode::flush`].
#[derive(Debug, Clone, Default)]
pub struct CodeEncoder {
    packer: CodePacker,
    words: Vec<u32>,
    codes: u64,
    samples: u64,
}

impl CodeEncoder {
    /// An empty encoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Codes taken so far.
    pub fn codes(&self) -> u64 {
        self.codes
    }

    /// DMA samples (eight codes each) produced so far.
    pub fn samples(&self) -> u64 {
        self.samples
    }
}

impl Encode for CodeEncoder {
    type Input = u8;
    type Component = i16;

    fn name(&self) -> &'static str {
        "TX codes"
    }

    fn encode(&mut self, codes: &[u8], out: &mut Vec<i16>) {
        out.clear();
        self.words.clear();
        self.packer.push(codes, &mut self.words);
        self.codes += codes.len() as u64;
        self.samples += self.words.len() as u64;
        txformat::to_dma(&self.words, out);
    }

    fn flush(&mut self, out: &mut Vec<i16>) {
        out.clear();
        self.words.clear();
        self.packer.finish(&mut self.words);
        self.samples += self.words.len() as u64;
        txformat::to_dma(&self.words, out);
    }

    fn describe(&self) -> String {
        format!(
            "{} symbol codes in {} DMA samples",
            self.codes, self.samples
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Cs12;
    use isdb_frontend::txformat::{codes, from_components, word};

    /// The DMA layout of `isdb_frontend::txformat`: code 0 in I bits 3..0,
    /// code 4 in Q bits 3..0; a partial sample padded with zero symbols.
    #[test]
    fn codes_become_two_channel_dma_samples() {
        let mut e = CodeEncoder::new();
        let mut out = Vec::new();
        e.encode(&[1, 2, 3, 4, 5, 6, 7, 15, 8, 9], &mut out);
        assert_eq!(out, [0x4321, 0xF765_u16.cast_signed()]);
        e.encode(&[10], &mut out);
        assert!(out.is_empty(), "3 codes held");
        e.flush(&mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(
            from_components([out[0], out[1]]),
            word([8, 9, 10, 0, 0, 0, 0, 0])
        );
        e.flush(&mut out);
        assert!(out.is_empty());
        assert_eq!((e.codes(), e.samples()), (11, 2));
        assert_eq!(e.describe(), "11 symbol codes in 2 DMA samples");
        assert_eq!(e.name(), "TX codes");
    }

    /// Any chunking gives the same components, and they unpack to the
    /// codes.
    #[test]
    fn chunking_is_invisible() {
        let x: Vec<u8> = (0u8..=255)
            .cycle()
            .take(1000)
            .map(|i| {
                if i % 5 == 0 {
                    0
                } else {
                    8 | (i.wrapping_mul(7) & 7)
                }
            })
            .collect();
        let mut whole = CodeEncoder::new();
        let (mut a, mut tail) = (Vec::new(), Vec::new());
        whole.encode(&x, &mut a);
        whole.flush(&mut tail);
        a.extend(&tail);
        let mut cut = CodeEncoder::new();
        let mut b: Vec<i16> = Vec::new();
        let mut out = Vec::new();
        for c in x.chunks(37) {
            cut.encode(c, &mut out);
            b.extend(&out);
        }
        cut.flush(&mut out);
        b.extend(&out);
        assert_eq!(a, b);
        let back: Vec<u8> = a
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|&c| codes(from_components(c)))
            .collect();
        assert_eq!(back, x);
    }

    #[test]
    fn the_quantizer_is_an_encoder() {
        let mut q = Quantizer::<Cs12>::new(0.0);
        let mut out = vec![1, 2, 3];
        Encode::encode(&mut q, &[C32::new(0.5, -0.5)], &mut out);
        assert_eq!(out, [1024 << 4, -1024 << 4]);
        Encode::flush(&mut q, &mut out);
        assert!(out.is_empty());
        assert_eq!(Encode::name(&q), "cs12");
        assert!(Encode::describe(&q).starts_with("clipped 0 of 2"));
    }
}
