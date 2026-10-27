//! Hierarchical-layer combining (ARIB STD-B31 §3.10, Fig. 3-21) and the
//! data-segment configuration (§3.9.5, Fig. 3-20).
//!
//! Per OFDM symbol, layer A's `nc·N_A` carrier symbols fill data segments
//! `0..N_A` in order (data segment `k`, carrier symbol `i` ↔ layer position
//! `(k − first)·nc + i`), then layer B, then layer C.  Data segments are
//! numbered 0..12 as in §3.11.2 (partial reception, differential,
//! coherent); a combined symbol is an array of `13·nc` values indexed
//! `k·nc + i` ("data-segment order").

use isdbt_core::{Layer, Params};

/// First data segment and segment count of `layer` (None if unused).
pub fn layer_segments(params: &Params, layer: Layer) -> Option<(usize, usize)> {
    params
        .layer_segments()
        .find(|(id, _, _)| *id == layer)
        .map(|(_, _, segs)| (segs.start, segs.len()))
}

/// Combine the per-layer symbol vectors of one OFDM symbol into
/// data-segment order.  `layers[l]` must hold `nc·N_l` values for active
/// layers (ignored for unused ones).
///
/// # Panics
///
/// If `out` does not hold `13·nc` values or an active layer's slice not
/// `nc·N_l`.
pub fn combine<T: Copy>(params: &Params, layers: [&[T]; 3], out: &mut [T]) {
    let nc = params.mode().data_carriers_per_segment();
    assert_eq!(out.len(), 13 * nc);
    for layer in Layer::ALL {
        if let Some((first, n)) = layer_segments(params, layer) {
            let src = layers[layer.index()];
            assert_eq!(src.len(), n * nc, "layer {layer}");
            out[first * nc..(first + n) * nc].copy_from_slice(src);
        }
    }
}

/// Split one combined symbol (data-segment order) back into layers.
///
/// # Panics
///
/// If `data` does not hold `13·nc` values.
pub fn split<'a, T: Copy>(params: &Params, data: &'a [T]) -> [Option<&'a [T]>; 3] {
    let nc = params.mode().data_carriers_per_segment();
    assert_eq!(data.len(), 13 * nc);
    Layer::ALL.map(|layer| {
        layer_segments(params, layer).map(|(first, n)| &data[first * nc..(first + n) * nc])
    })
}
