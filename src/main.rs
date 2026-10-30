#[cfg(not(target_arch = "wasm32"))]
use std::error::Error;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn Error>> {
    gravity_spy::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
