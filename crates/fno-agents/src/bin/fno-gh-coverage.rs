//! `fno-gh-coverage`: the coverage-purpose gh adapter the finalize and
//! review-coverage commands spawn by name. Ported from the pip console script
//! that pointed at `fno.pr._internal_graph` entry point (pr port 7).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(fno_agents::pr_status::adapter::run_main("coverage", args));
}
