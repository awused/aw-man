use std::any::Any;
use std::thread;

use crate::closing;

pub mod downscaling;
pub mod extracting;
pub mod loading;
pub mod upscaling;

fn handle_panic(_e: Box<dyn Any + Send>) {
    closing::fatal(format!(
        "Unexpected panic in thread {}",
        thread::current().name().unwrap_or("unnamed")
    ));
}
