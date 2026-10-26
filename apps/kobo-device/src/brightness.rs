//! Logical brightness through the selected Kobo device profile.
use crate::frontlight::with_controller;
use std::io;
pub fn read() -> io::Result<u8> { with_controller(|controller| controller.brightness()) }
pub fn set(percent: u8) -> io::Result<u8> { with_controller(|controller| controller.set_brightness(percent)) }
