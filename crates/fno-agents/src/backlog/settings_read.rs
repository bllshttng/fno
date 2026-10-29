//! The hidden `settings-read` door: the whole settings fact set the Python
//! graph leftovers used to read in-process (node-id knobs, config
//! candidates, the work map, the cwd -> project detect), answered in one
//! spawn. One JSON request in, one JSON receipt out; every field is
//! fail-open, so an advisory caller keeps its legacy default on any read
//! failure. The decision never splits across the boundary
//! (docs/architecture/rust-python-seam.md).

use serde_json::Value;
use std::io::Read;

/// The door's name, held in one const so the dispatcher can name it without
/// a string literal: the menu-caps counter reads literals inside `run()`,
/// and a hidden door is not a menu command.
pub const DOOR_NAME: &str = "settings-read";

/// Run the door: read the request from stdin, print the fact-set receipt,
/// always exit 0. The request is `{"detect": "<cwd>"}` (or `{}`); a missing
/// or unparseable request simply carries no detect.
pub fn run_settings_read(args: &[String]) -> i32 {
    let _ = args;
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let request: Value = serde_json::from_str(input.trim()).unwrap_or(Value::Null);
    let detect = request
        .get("detect")
        .and_then(Value::as_str)
        .map(str::to_string);
    let pairs = super::settings::work_map_pairs();
    let project = super::settings::detect_project(detect.as_deref());
    let projects: Vec<Value> = pairs
        .iter()
        .map(|(name, path)| serde_json::json!([name, path]))
        .collect();
    let payload = serde_json::json!({
        "prefix": super::settings::node_id_prefix(),
        "hex_width": super::settings::node_id_hex_width(),
        "candidates": super::settings::candidates(),
        "projects": projects,
        "project": project,
    });
    println!("{payload}");
    0
}
