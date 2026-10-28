//! Raw declarations from `libhackrf/hackrf.h` (only what this crate uses).
//!
//! Declarations follow libhackrf's hackrf.h (BSD-3-Clause, © Great Scott Gadgets, Jared Boone, Benjamin Vernoux)
//!
//! Only the function signatures, the `hackrf_transfer` layout and error
//! codes are re-typed here; no libhackrf code is included.
// C names and fields mirror hackrf.h exactly; they are documented there.
#![allow(non_camel_case_types, missing_docs)]

use std::os::raw::{c_char, c_int, c_void};

#[repr(C)]
pub struct hackrf_device {
    _private: [u8; 0],
}

#[repr(C)]
pub struct hackrf_transfer {
    pub device: *mut hackrf_device,
    pub buffer: *mut u8,
    pub buffer_length: c_int,
    pub valid_length: c_int,
    pub rx_ctx: *mut c_void,
    pub tx_ctx: *mut c_void,
}

pub type hackrf_sample_block_cb_fn = unsafe extern "C" fn(transfer: *mut hackrf_transfer) -> c_int;

pub const HACKRF_SUCCESS: c_int = 0;
pub const HACKRF_TRUE: c_int = 1;

unsafe extern "C" {
    pub fn hackrf_init() -> c_int;
    pub fn hackrf_exit() -> c_int;
    pub fn hackrf_library_version() -> *const c_char;
    pub fn hackrf_library_release() -> *const c_char;
    pub fn hackrf_open(device: *mut *mut hackrf_device) -> c_int;
    pub fn hackrf_open_by_serial(serial: *const c_char, device: *mut *mut hackrf_device) -> c_int;
    pub fn hackrf_close(device: *mut hackrf_device) -> c_int;
    pub fn hackrf_start_rx(
        device: *mut hackrf_device,
        cb: hackrf_sample_block_cb_fn,
        ctx: *mut c_void,
    ) -> c_int;
    pub fn hackrf_stop_rx(device: *mut hackrf_device) -> c_int;
    pub fn hackrf_start_tx(
        device: *mut hackrf_device,
        cb: hackrf_sample_block_cb_fn,
        ctx: *mut c_void,
    ) -> c_int;
    pub fn hackrf_stop_tx(device: *mut hackrf_device) -> c_int;
    pub fn hackrf_is_streaming(device: *mut hackrf_device) -> c_int;
    pub fn hackrf_set_freq(device: *mut hackrf_device, freq_hz: u64) -> c_int;
    pub fn hackrf_set_sample_rate_manual(
        device: *mut hackrf_device,
        freq_hz: u32,
        divider: u32,
    ) -> c_int;
    pub fn hackrf_set_baseband_filter_bandwidth(
        device: *mut hackrf_device,
        bandwidth_hz: u32,
    ) -> c_int;
    pub fn hackrf_compute_baseband_filter_bw(bandwidth_hz: u32) -> u32;
    pub fn hackrf_set_amp_enable(device: *mut hackrf_device, value: u8) -> c_int;
    pub fn hackrf_set_lna_gain(device: *mut hackrf_device, value: u32) -> c_int;
    pub fn hackrf_set_vga_gain(device: *mut hackrf_device, value: u32) -> c_int;
    pub fn hackrf_set_txvga_gain(device: *mut hackrf_device, value: u32) -> c_int;
    pub fn hackrf_set_antenna_enable(device: *mut hackrf_device, value: u8) -> c_int;
    pub fn hackrf_board_id_read(device: *mut hackrf_device, value: *mut u8) -> c_int;
    pub fn hackrf_version_string_read(
        device: *mut hackrf_device,
        version: *mut c_char,
        length: u8,
    ) -> c_int;
    pub fn hackrf_error_name(errcode: c_int) -> *const c_char;
}
