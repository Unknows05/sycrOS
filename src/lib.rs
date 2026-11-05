//! sycrOS - one USB stick for Windows, Linux and macOS installers.
//!
//! Windows/Linux ISO files boot through Ventoy's boot engine (partitions 1+2,
//! byte-compatible with Ventoy). macOS DMG/ISO images become their own HFS+/APFS
//! partitions that the Mac boot menu can start (installer or Recovery).

pub mod engine;
pub mod gpt;
pub mod hosts;
pub mod layout;
pub mod library;
pub mod macimg;
pub mod rawdisk;
pub mod udif;
pub mod ui;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
