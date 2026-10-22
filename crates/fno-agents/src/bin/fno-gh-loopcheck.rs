//! `fno-gh-loopcheck`: the discretionary-purpose gh adapter the stop gate
//! spawns by name. Ported from the pip console script that pointed at
//! `fno.pr._internal_gh:discretionary_main` (pr port 7).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(fno_agents::pr_status::adapter::run_main(
        "discretionary",
        args,
    ));
}
