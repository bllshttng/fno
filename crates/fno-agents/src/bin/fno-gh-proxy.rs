//! `fno-gh-proxy`: the gh shim's delegate binary (the shim line
//! `exec fno-gh-proxy "$@"` lands here). Ported from the pip console script
//! that pointed at `fno.pr.gh_proxy:main` (pr port 7).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(fno_agents::pr_status::proxy::run_proxy_main(args));
}
