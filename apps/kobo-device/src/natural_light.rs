//! Logical warmth through the selected Kobo device profile.
use crate::frontlight::with_controller;
use std::io;
pub fn read() -> io::Result<Option<u8>> { with_controller(|controller| controller.warmth()) }
pub fn set(percent: u8) -> io::Result<u8> { with_controller(|controller| controller.set_warmth(percent)) }
