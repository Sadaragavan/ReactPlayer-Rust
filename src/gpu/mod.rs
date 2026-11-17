//! GPU compositing and the CPU-side capture path.

pub mod capture;
pub mod renderer;

/// Dynamic uniform offsets are 256-byte aligned on every adapter we target; the
/// compositor raises this to the device's own limit when it is larger.
pub const UNIFORM_STRIDE: u64 = 256;
