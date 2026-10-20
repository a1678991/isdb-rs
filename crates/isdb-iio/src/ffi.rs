//! Raw declarations from libiio 0.x `iio.h` (only what this crate uses).
//!
//! Declarations follow libiio's iio.h (LGPL-2.1-or-later, © Analog Devices, Inc.)
//!
//! Only the function signatures are re-typed here; no libiio code is
//! included. libiio 1.x renamed and reshaped most of these calls; this crate
//! targets the 0.x API (0.21 or later).
// C names mirror iio.h exactly; they are documented there.
#![allow(non_camel_case_types, missing_docs)]

use std::os::raw::{c_char, c_int, c_longlong, c_uint};

#[repr(C)]
pub struct iio_context {
    _private: [u8; 0],
}

#[repr(C)]
pub struct iio_device {
    _private: [u8; 0],
}

#[repr(C)]
pub struct iio_channel {
    _private: [u8; 0],
}

#[repr(C)]
pub struct iio_buffer {
    _private: [u8; 0],
}

unsafe extern "C" {
    pub fn iio_library_get_version(major: *mut c_uint, minor: *mut c_uint, git_tag: *mut c_char);
    pub fn iio_strerror(err: c_int, dst: *mut c_char, len: usize);

    pub fn iio_create_context_from_uri(uri: *const c_char) -> *mut iio_context;
    pub fn iio_context_destroy(ctx: *mut iio_context);
    pub fn iio_context_get_description(ctx: *const iio_context) -> *const c_char;
    pub fn iio_context_get_attr_value(
        ctx: *const iio_context,
        name: *const c_char,
    ) -> *const c_char;
    pub fn iio_context_set_timeout(ctx: *mut iio_context, timeout_ms: c_uint) -> c_int;
    pub fn iio_context_find_device(ctx: *const iio_context, name: *const c_char)
    -> *mut iio_device;

    pub fn iio_device_find_channel(
        dev: *const iio_device,
        name: *const c_char,
        output: bool,
    ) -> *mut iio_channel;
    pub fn iio_device_attr_read(
        dev: *const iio_device,
        attr: *const c_char,
        dst: *mut c_char,
        len: usize,
    ) -> isize;
    pub fn iio_device_attr_write(
        dev: *const iio_device,
        attr: *const c_char,
        src: *const c_char,
    ) -> isize;
    pub fn iio_device_debug_attr_read(
        dev: *const iio_device,
        attr: *const c_char,
        dst: *mut c_char,
        len: usize,
    ) -> isize;
    pub fn iio_device_debug_attr_write(
        dev: *const iio_device,
        attr: *const c_char,
        src: *const c_char,
    ) -> isize;
    pub fn iio_device_reg_read(dev: *mut iio_device, address: u32, value: *mut u32) -> c_int;
    pub fn iio_device_reg_write(dev: *mut iio_device, address: u32, value: u32) -> c_int;
    pub fn iio_device_set_kernel_buffers_count(dev: *const iio_device, nb_buffers: c_uint)
    -> c_int;
    pub fn iio_device_get_sample_size(dev: *const iio_device) -> isize;

    pub fn iio_channel_attr_read(
        chn: *const iio_channel,
        attr: *const c_char,
        dst: *mut c_char,
        len: usize,
    ) -> isize;
    pub fn iio_channel_attr_write(
        chn: *const iio_channel,
        attr: *const c_char,
        src: *const c_char,
    ) -> isize;
    pub fn iio_channel_attr_read_longlong(
        chn: *const iio_channel,
        attr: *const c_char,
        val: *mut c_longlong,
    ) -> c_int;
    pub fn iio_channel_attr_write_longlong(
        chn: *const iio_channel,
        attr: *const c_char,
        val: c_longlong,
    ) -> c_int;
    pub fn iio_channel_attr_read_double(
        chn: *const iio_channel,
        attr: *const c_char,
        val: *mut f64,
    ) -> c_int;
    pub fn iio_channel_attr_write_double(
        chn: *const iio_channel,
        attr: *const c_char,
        val: f64,
    ) -> c_int;
    pub fn iio_channel_enable(chn: *mut iio_channel);
    pub fn iio_channel_disable(chn: *mut iio_channel);

    pub fn iio_device_create_buffer(
        dev: *const iio_device,
        samples_count: usize,
        cyclic: bool,
    ) -> *mut iio_buffer;
    pub fn iio_buffer_destroy(buf: *mut iio_buffer);
    pub fn iio_buffer_refill(buf: *mut iio_buffer) -> isize;
    pub fn iio_buffer_push(buf: *mut iio_buffer) -> isize;
    pub fn iio_buffer_cancel(buf: *mut iio_buffer);
    pub fn iio_buffer_start(buf: *const iio_buffer) -> *mut std::ffi::c_void;
    pub fn iio_buffer_end(buf: *const iio_buffer) -> *mut std::ffi::c_void;
}
