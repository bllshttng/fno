//! The ported target-family doors behind `bin/client.rs`'s one dispatch
//! line: the commands `cli/src/fno/target_cli.py`, `state/cli.py`, and
//! `stub_manifest.py` served before the port. Hidden daemon-free leaves in
//! the graph-get mold: dispatched by `==` on the first token and never
//! registered in ALL_CLIENT_ACTIONS (the shrink law allows no added token).

/// One dispatch line for the client entrypoint: `Some(exit)` when the argv
/// names a family door.
pub fn client_dispatch(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        Some("target-gates") => Some(crate::target_gates::run(&args[1..])),
        Some("state-cli") => Some(crate::state::cli::run(&args[1..])),
        Some("stub-manifest") => Some(crate::stub_manifest::run(&args[1..])),
        _ => None,
    }
}
