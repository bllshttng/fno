//! `fno do research`: the scout backbone. ddgs search, a guarded self-fetch
//! of each result, a `sources.jsonl` evidence store, and the cited brief
//! that ships to `config.research.output_dir` and ends `DoneAdvisory`.

pub mod cli;
pub mod deliverable;
pub mod engine;
pub mod html;
pub mod pyfmt;

/// The door: `argv` is everything after `fno do research`.
pub fn run(args: &[String]) -> i32 {
    cli::run(args)
}
