#![cfg(feature = "benchmarking")]
#![allow(unused)]
#![allow(clippy::missing_panics_doc)]

#[macro_use]
extern crate log;

// This is only for benchmarking
pub mod natsort;

mod com;

#[allow(unused)]
pub mod resample;
