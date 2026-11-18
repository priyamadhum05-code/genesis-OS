//! Project Genesis — File Systems Subsystem.

pub mod fat32;

pub use fat32::{locate_volume, to_8_3, FatVolume};
