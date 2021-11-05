use std::any::Any;
use std::thread;

use crate::closing;

pub mod extracting;
pub mod loading;
#[allow(unused)]
mod resample;
pub mod scanning;
pub mod upscaling;

fn handle_panic(_e: Box<dyn Any + Send>) {
    error!(
        "Unexpected panic in thread {}",
        thread::current().name().unwrap_or("unnamed")
    );
    closing::close();
}
