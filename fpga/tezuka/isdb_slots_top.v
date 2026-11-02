// Block-design wrapper of the isdb-rs ISDB-S3 receive block
// (isdb_slots_rx: fpga/build/isdb_slots_rx.v from `mise run
// fpga:slots-verilog`, fpga/isdb_fpga/chain/top.py SlotsRx) for Tezuka's
// PlutoSky R2 design. Glue only: the generated block's ports, with its fast
// domain's reset made here.
//
// isdb_slots_rx has a synchronous reset per domain: `rst` for the symbol
// clock `clk` (util_ad9361_divclk = l_clk / 4) and `fast_rst` for its 4x
// `fast_clk` (axi_ad9361 l_clk). The block design has a reset in the symbol
// clock's domain only (util_ad9361_adc_fifo dout_rstn, inverted). Driving
// fast_rst from that net directly would time a fanout of several hundred
// fast-domain resets from an outside register at ADI's 4 ns rx_clk
// requirement; instead the reset is registered twice in the fast domain
// here: the first register is the only endpoint of the clk -> fast_clk
// crossing (the clocks are related: divclk is a generated clock of rx_clk),
// the second drives the fanout and is one of the block's fast registers,
// which fpga/tezuka/isdb_slots.xdc times at the real 6.25 ns. fast_rst
// follows rst by two fast cycles (asserted and released within one symbol
// clock); the block's clock-enable phasing (common.Phase4) finds its phase
// whatever the reset states.
//
// ldpc_ref is clk_fpga_0 (100 MHz) into the block's MMCM (200 MHz for the
// LDPC decoder); ldpc_locked is its lock output, left for debugging.
//
// Copyright isdb-rs contributors; this file is part of isdb-rs (see the
// repository licence).

`timescale 1ns/1ps

module isdb_slots_top (
  input             clk,
  input             rst,
  input             fast_clk,
  input             ldpc_ref,
  output            ldpc_locked,
  input             adc_valid,
  input      [15:0] adc_i,
  input      [15:0] adc_q,
  input             pass_valid,
  input      [15:0] pass_data0,
  input      [15:0] pass_data1,
  output            pack_valid,
  output     [15:0] pack_data0,
  output     [15:0] pack_data1,
  input      [31:0] gpio_out,
  output     [31:0] gpio_in
);

  (* shreg_extract = "no" *) reg fast_rst_m = 1'b1;
  (* shreg_extract = "no", max_fanout = 64 *) reg fast_rst_q = 1'b1;
  always @(posedge fast_clk) begin
    fast_rst_m <= rst;
    fast_rst_q <= fast_rst_m;
  end

  isdb_slots_rx rx (
    .clk (clk),
    .rst (rst),
    .fast_clk (fast_clk),
    .fast_rst (fast_rst_q),
    .ldpc_ref (ldpc_ref),
    .ldpc_locked (ldpc_locked),
    .adc_valid (adc_valid),
    .adc_i (adc_i),
    .adc_q (adc_q),
    .pass_valid (pass_valid),
    .pass_data0 (pass_data0),
    .pass_data1 (pass_data1),
    .pack_valid (pack_valid),
    .pack_data0 (pack_data0),
    .pack_data1 (pack_data1),
    .gpio_out (gpio_out),
    .gpio_in (gpio_in));

endmodule
