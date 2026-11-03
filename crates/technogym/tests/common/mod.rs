#![allow(
    dead_code,
    reason = "shared by test binaries that use different helpers"
)]
use serde_json::Value;

pub fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect("valid fixture")
}
