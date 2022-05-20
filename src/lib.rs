#![cfg(feature = "benchmarking")]
#![feature(portable_simd)]
#![allow(unused)]

#[macro_use]
extern crate log;

// This is only for benchmarking
pub mod natsort;

mod com;

#[allow(unused)]
pub mod resample;
