/// The changed-surface packet (`fno doctor test smoke --changed`).
///
/// Partial evidence, never full. The selector maps changed paths to a subset
/// of the same work the full runner owns; execution, step naming and exit
/// semantics stay with the runner. Three separate mechanisms stop a partial
/// green from reading as a full one: the CHANGED SUBSET label (never FULL), a
/// failure record and receipt in their own namespace, and typed exit codes
/// that let a caller distinguish "passed" from "nothing mapped" from "no
/// trustworthy diff".
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::test_smoke::SmokeOpts;
use crate::test_smoke_discovery as disc;
use crate::test_smoke_registry as reg;
use crate::test_smoke_registry::{
    CHANGED_RC_PREREQ, CHANGED_RC_UNEVALUATED, CHANGED_RC_UNSELECTED,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    pub rule: &'static str,
    pub path: String,
    pub kind: &'static str,
    pub target: String,
}

fn git(root: &Path, args: &[&str]) -> (i32, String) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output();
    match out {
        Err(e) => (127, e.to_string()),
        Ok(o) => {
            let text = if o.status.success() {
                String::from_utf8_lossy(&o.stdout)
            } else {
                String::from_utf8_lossy(&o.stderr)
            };
            (o.status.code().unwrap_or(1), text.trim().to_string())
        }
    }
}

fn nonempty_lines(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// One deterministic changed-path snapshot -> (paths, unevaluated reason,
/// resolved base).
///
/// Explicit base+head (CI) diffs those exact revisions, so behavior never
/// depends on a mutable remote-tracking ref. Local mode diffs the merge-base
/// with origin/main (origin/master when main is absent) and adds untracked
/// files. A non-empty reason means the result is UNEVALUATED, never an empty
/// changeset. The resolved base names what actually sized the diff.
pub fn changed_snapshot(root: &Path, base: &str, head: &str) -> (Vec<String>, String, String) {
    if base.is_empty() != head.is_empty() {
        return (
            Vec::new(),
            "--changed takes both --base and --head, or neither (local mode)".into(),
            String::new(),
        );
    }
    if !base.is_empty() {
        let mut shas: Vec<String> = Vec::new();
        for rev in [base, head] {
            let probe = format!("{rev}^{{commit}}");
            let (rc, out) = git(root, &["rev-parse", "--verify", "--quiet", &probe]);
            if rc != 0 || out.is_empty() {
                return (
                    Vec::new(),
                    format!(
                        "revision {rev:?} does not resolve here (shallow checkout or missing ref)"
                    ),
                    String::new(),
                );
            }
            shas.push(out);
        }
        let spec = format!("{}...{}", shas[0], shas[1]);
        let (rc, out) = git(root, &["diff", "--name-only", &spec]);
        if rc != 0 {
            return (
                Vec::new(),
                format!("git diff {base}...{head} failed: {out}"),
                String::new(),
            );
        }
        let mut paths = nonempty_lines(&out);
        paths.sort();
        paths.dedup();
        return (paths, String::new(), base.to_string());
    }
    // Only an ABSENT ref advances the fallback: a resolvable ref with no
    // merge-base (unrelated histories) means this branch cannot be sized
    // against that ref, not that a possibly stale sibling ref should answer
    // instead.
    let mut mb = String::new();
    let mut won = String::new();
    for candidate in ["origin/main", "origin/master"] {
        let probe = format!("{candidate}^{{commit}}");
        let (rc, out) = git(root, &["rev-parse", "--verify", "--quiet", &probe]);
        if rc != 0 || out.is_empty() {
            continue;
        }
        let (rc, out) = git(root, &["merge-base", candidate, "HEAD"]);
        if rc == 0 && !out.is_empty() {
            mb = out;
            won = candidate.to_string();
        }
        break;
    }
    if mb.is_empty() {
        return (
            Vec::new(),
            "cannot resolve merge-base with origin/main or origin/master (fetch it, or pass --base/--head)".into(),
            String::new(),
        );
    }
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for args in [
        vec!["diff", "--name-only", mb.as_str()],
        vec!["ls-files", "--others", "--exclude-standard"],
    ] {
        let (rc, out) = git(root, &args);
        if rc != 0 {
            return (
                Vec::new(),
                format!("git {} failed: {out}", args.join(" ")),
                String::new(),
            );
        }
        paths.extend(nonempty_lines(&out));
    }
    let mut list: Vec<String> = paths.into_iter().collect();
    list.sort();
    (
        list,
        String::new(),
        format!("{won}@{}", &mb[..12.min(mb.len())]),
    )
}

/// cli/src/fno/mail/envelope.py -> fno.mail.envelope; "" for anything else.
/// A co-located `__init__.py` maps to its package, not `pkg.__init__`.
pub fn module_name(rel: &str) -> String {
    let prefix = "cli/src/";
    if !rel.starts_with(prefix) || !rel.ends_with(".py") {
        return String::new();
    }
    let dotted = rel[prefix.len()..]
        .strip_suffix(".py")
        .unwrap_or(&rel[prefix.len()..])
        .replace('/', ".");
    dotted
        .strip_suffix(".__init__")
        .unwrap_or(&dotted)
        .to_string()
}

/// Every module name `text` references through import, at any depth.
///
/// Function-level imports are the point (test_mux_inject imports dispatch
/// inside its fixtures), so this scans every indented import line, not just
/// the first column. `from X import y` records both X and X.y: which of the
/// two is a module is not knowable without executing, and recording both is
/// the optimistic reading a selector should take. Every ancestor package is
/// recorded too: importing any submodule executes its __init__, so a change
/// to the package owns those importers. Relative imports resolve against the
/// current module's package by dot count, as the AST walk did.
pub fn import_refs(text: &str, module: &str, is_package: bool) -> BTreeSet<String> {
    let mut refs: BTreeSet<String> = BTreeSet::new();
    let import_re = regex::Regex::new(r"^\s*import\s+(.+)$").expect("static regex");
    let from_re =
        regex::Regex::new(r"^\s*from\s+(\.+)?([\w.]*)\s+import\s+(.+)$").expect("static regex");
    for line in text.lines() {
        let code = line.split('#').next().unwrap_or(line).trim_end();
        if let Some(caps) = from_re.captures(code) {
            let dots = caps.get(1).map(|m| m.as_str().len()).unwrap_or(0);
            let base_raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let names: Vec<String> = caps[3]
                .split(',')
                .map(|s| s.split_whitespace().next().unwrap_or("").to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let base = if dots > 0 {
                let parts: Vec<&str> = module.split('.').collect();
                // A module file's package is its parent; a package's package
                // is itself. One dot resolves inside that package.
                let mut keep = if is_package {
                    parts.len()
                } else {
                    parts.len().saturating_sub(1)
                };
                keep = keep.saturating_sub(dots - 1);
                parts[..keep].join(".")
            } else {
                base_raw.to_string()
            };
            if base.is_empty() && dots == 0 {
                continue;
            }
            if !base.is_empty() {
                refs.insert(base.clone());
                for name in &names {
                    refs.insert(format!("{base}.{name}"));
                }
            }
            continue;
        }
        if let Some(caps) = import_re.captures(code) {
            for piece in caps[1].split(',') {
                let name = piece.split_whitespace().next().unwrap_or("");
                if !name.is_empty() {
                    refs.insert(name.to_string());
                }
            }
        }
    }
    let mut expanded = BTreeSet::new();
    for reference in &refs {
        let parts: Vec<&str> = reference.split('.').collect();
        for i in 1..=parts.len() {
            expanded.insert(parts[..i].join("."));
        }
    }
    expanded
}

/// Module -> the modules it imports, across cli/src/fno, one scan each.
pub fn fno_importers(root: &Path) -> BTreeMap<String, BTreeSet<String>> {
    let mut graph = BTreeMap::new();
    let src = root.join("cli/src/fno");
    for rel in walk_py(&src, root) {
        let module = module_name(&rel);
        if module.is_empty() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        let refs = import_refs(&text, &module, rel.ends_with("__init__.py"));
        if !refs.is_empty() {
            graph.insert(module, refs);
        }
    }
    graph
}

fn walk_py(base: &Path, root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "py").unwrap_or(false) {
                out.push(
                    path.strip_prefix(root)
                        .unwrap_or(path.as_path())
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }
    out.sort();
    out
}

/// `mod` plus the modules that import it directly (one hop).
///
/// The red-on-main mux chain was exactly this shape: the test imports
/// dispatch, dispatch imports envelope. Full transitive closure is honest
/// reach but selects ~950 of ~1100 test files for a hub module, so the
/// widening stops at one hop and leaves deeper chains to the full gate.
pub fn importer_neighbourhood(
    graph: &BTreeMap<String, BTreeSet<String>>,
    module: &str,
) -> BTreeSet<String> {
    let mut hit = BTreeSet::new();
    hit.insert(module.to_string());
    for (m, refs) in graph {
        if refs.contains(module) {
            hit.insert(m.clone());
        }
    }
    hit
}

/// (rel, text, import refs) for every test file, read once per selection.
/// Covers cli/tests, the root tests/ tree, and tests co-located in cli/src.
pub fn test_reach_index(root: &Path) -> Vec<(String, String, BTreeSet<String>)> {
    let mut out = Vec::new();
    for base in ["cli/tests", "tests", "cli/src"] {
        for rel in walk_py(&root.join(base), root) {
            if !rel.rsplit('/').next().unwrap_or("").starts_with("test_") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
                continue;
            };
            let refs = import_refs(&text, &module_name(&rel), rel.ends_with("__init__.py"));
            out.push((rel, text, refs));
        }
    }
    out
}

/// Same-stem tests: `test_<stem>.py` plus the `test_<stem>_*.py` family.
/// The suffixed family is how a third of the covered modules are actually
/// named (claims.py -> test_claims_core.py, test_claims_io.py), so
/// exact-match alone leaves them unmapped.
pub fn conventional_tests(root: &Path, stem: &str) -> Vec<String> {
    let tests = root.join("cli/tests");
    let hits: BTreeSet<String> = walk_py(&tests, root)
        .into_iter()
        .filter(|rel| {
            let name = rel.rsplit('/').next().unwrap_or("");
            name == &format!("test_{stem}.py") || name.starts_with(&format!("test_{stem}_"))
        })
        .collect();
    let mut list: Vec<String> = hits.into_iter().collect();
    list.sort();
    list
}

/// Repo-rooted source paths listed in skill-bundles.yaml.
pub fn bundle_sources(root: &Path) -> BTreeSet<String> {
    let Ok(text) = std::fs::read_to_string(root.join("skill-bundles.yaml")) else {
        return BTreeSet::new();
    };
    let re = regex::Regex::new(r"^\s*-\s*source:\s*(\S+)\s*$").expect("static regex");
    re.captures_iter(&text)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

/// Map changed paths to work, in rule order -> (selections, unmapped).
///
/// Each selection names the rule that produced it, so a bad mapping is
/// diagnosable from the receipt. Selection is deterministic and intentionally
/// incomplete: an unmapped path stays visible instead of becoming "green".
pub fn select_changed(root: &Path, paths: &[String]) -> (Vec<Selection>, Vec<String>) {
    // Subtract the quarantine, exactly as the full gate does: these harnesses
    // are pre-existing rot the full gate refuses to run; selecting one turns
    // an ordinary edit into a red packet that aborts preflight BEFORE the
    // real gate, on a test CI would never have run.
    let harnesses: BTreeSet<String> = disc::discover_shell_harnesses(root)
        .into_iter()
        .filter(|rel| !crate::test_smoke_discovery::is_deferred(rel))
        .collect();
    let edges = disc::source_edges(root);
    let mut selections: Vec<Selection> = Vec::new();
    let mut unmapped: Vec<String> = Vec::new();
    let mut seen: BTreeSet<(&'static str, String)> = BTreeSet::new();

    let add = |rule: &'static str,
               path: &str,
               kind: &'static str,
               target: String,
               out: &mut Vec<Selection>,
               seen: &mut BTreeSet<(&'static str, String)>| {
        if seen.insert((kind, target.clone())) {
            out.push(Selection {
                rule,
                path: path.to_string(),
                kind,
                target,
            });
        }
    };
    let fallback = |rel: &str,
                    out: &mut Vec<Selection>,
                    seen: &mut BTreeSet<(&'static str, String)>,
                    unmapped: &mut Vec<String>| {
        // Last resort before unmapped: a registry step that names this path.
        // Applies to every path shape; an orchestrated subtree names only its
        // runner in the registry, so the substring match cannot see the
        // individual file - the prefix map is the mirror of discovery
        // excluding the same tree.
        let mut owners: Vec<&str> = reg::STRUCTURAL_STEPS
            .iter()
            .filter(|(_, _, cmd)| cmd.contains(rel))
            .map(|(name, _, _)| *name)
            .collect();
        for (prefix, owner) in reg::ORCHESTRATED_SUBTREES {
            if rel.starts_with(prefix) {
                owners.push(owner);
            }
        }
        if owners.is_empty() {
            unmapped.push(rel.to_string());
            return;
        }
        owners.dedup();
        for owner in owners {
            add("registry-step", rel, "step", owner.to_string(), out, seen);
        }
    };
    for rel in paths {
        let base = rel.rsplit('/').next().unwrap_or(rel);
        if reg::INFRA_EXACT.contains(&rel.as_str())
            || reg::INFRA_BASENAME.contains(&base)
            || reg::INFRA_PREFIX.iter().any(|p| rel.starts_with(p))
        {
            for target in reg::INFRA_SELECTS {
                if root.join(target).exists() {
                    let kind = if target.ends_with(".py") {
                        "pytest"
                    } else {
                        "shell"
                    };
                    add(
                        "infra-broad",
                        rel,
                        kind,
                        target.to_string(),
                        &mut selections,
                        &mut seen,
                    );
                }
            }
            continue;
        }
        if rel.ends_with(".py") && base.starts_with("test_") && rel.contains("/tests/") {
            // A deleted test cannot be run. The path stays visible rather
            // than being handed to pytest, which would fail on the missing
            // file.
            if root.join(rel).exists() {
                add(
                    "test-file-self",
                    rel,
                    "pytest",
                    rel.clone(),
                    &mut selections,
                    &mut seen,
                );
            } else {
                fallback(rel, &mut selections, &mut seen, &mut unmapped);
            }
            continue;
        }
        if rel.starts_with("cli/src/") && rel.ends_with(".py") {
            // Discovery only returns files that exist, so a deleted source
            // still correctly selects its surviving test family.
            let hits = conventional_tests(root, base.strip_suffix(".py").unwrap_or(base));
            if !hits.is_empty() {
                for hit in hits {
                    add(
                        "python-source-stem",
                        rel,
                        "pytest",
                        hit,
                        &mut selections,
                        &mut seen,
                    );
                }
                continue;
            }
            fallback(rel, &mut selections, &mut seen, &mut unmapped);
            continue;
        }
        if rel.ends_with(".sh") {
            if harnesses.contains(rel) {
                add(
                    "shell-harness-self",
                    rel,
                    "shell",
                    rel.clone(),
                    &mut selections,
                    &mut seen,
                );
                continue;
            }
            let mut owners: Vec<String> = edges
                .get(rel)
                .map(|owners| {
                    owners
                        .iter()
                        .filter(|o| harnesses.contains(*o))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if !owners.is_empty() {
                owners.sort();
                for o in owners {
                    add(
                        "shell-helper-reverse",
                        rel,
                        "shell",
                        o,
                        &mut selections,
                        &mut seen,
                    );
                }
                continue;
            }
            fallback(rel, &mut selections, &mut seen, &mut unmapped);
            continue;
        }
        if rel.starts_with("crates/") {
            let fam = rust_family(rel);
            if !fam.is_empty() {
                for name in fam {
                    add(
                        "rust-family",
                        rel,
                        "step",
                        name.to_string(),
                        &mut selections,
                        &mut seen,
                    );
                }
                continue;
            }
            fallback(rel, &mut selections, &mut seen, &mut unmapped);
            continue;
        }
        fallback(rel, &mut selections, &mut seen, &mut unmapped);
    }
    // Reach widening: the rules above match names; a test can also be reached
    // by an infix file name, an import chain, or a path literal, and a bundle
    // member drags its freshness tests. Additive to the rules above; `add`
    // dedupes. The indices build lazily and at most once, so a diff with no
    // widened shapes pays nothing.
    let mut reach: Option<Vec<(String, String, BTreeSet<String>)>> = None;
    let bundle_sources = bundle_sources(root);
    let mut graph: Option<BTreeMap<String, BTreeSet<String>>> = None;
    for rel in paths {
        let base = rel.rsplit('/').next().unwrap_or(rel);
        if bundle_sources.contains(rel) {
            for target in reg::BUNDLE_TEST_TARGETS {
                if root.join(target).exists() {
                    add(
                        "bundle-member",
                        rel,
                        "pytest",
                        target.to_string(),
                        &mut selections,
                        &mut seen,
                    );
                }
            }
        }
        if rel.starts_with("cli/src/") && rel.ends_with(".py") {
            let stem = base.strip_suffix(".py").unwrap_or(base);
            if reach.is_none() {
                reach = Some(test_reach_index(root));
            }
            let index = reach.as_ref().expect("just filled");
            if stem.len() >= 4 {
                for (trel, _text, _refs) in index {
                    if trel.rsplit('/').next().unwrap_or("").contains(stem) {
                        add(
                            "python-source-infix",
                            rel,
                            "pytest",
                            trel.clone(),
                            &mut selections,
                            &mut seen,
                        );
                    }
                }
            }
            let module = module_name(rel);
            if !module.is_empty() {
                if graph.is_none() {
                    graph = Some(fno_importers(root));
                }
                let mods = importer_neighbourhood(graph.as_ref().expect("just filled"), &module);
                for (trel, _text, refs) in index {
                    if refs.iter().any(|r| mods.contains(r)) {
                        add(
                            "python-source-importers",
                            rel,
                            "pytest",
                            trel.clone(),
                            &mut selections,
                            &mut seen,
                        );
                    }
                }
            }
        }
        if reach.is_none() {
            reach = Some(test_reach_index(root));
        }
        let index = reach.as_ref().expect("just filled");
        for (trel, text, _refs) in index {
            if text.contains(rel.as_str()) {
                add(
                    "path-literal",
                    rel,
                    "pytest",
                    trel.clone(),
                    &mut selections,
                    &mut seen,
                );
            }
        }
    }

    // The repo-wide lints ride every packet: each reads the whole tree, so no
    // per-path rule can fire it, and all of them are seconds long. Gated on
    // the script existing so the selector contract tests' synthetic fixtures
    // keep their exact selection lists.
    for (name, script) in reg::ALWAYS_LINTS {
        if root.join(script).exists() {
            add(
                "repo-wide-lint",
                "*",
                "step",
                name.to_string(),
                &mut selections,
                &mut seen,
            );
        }
    }
    (selections, unmapped)
}

/// Registry step names that own the crate `rel` lives in.
pub fn rust_family(rel: &str) -> Vec<&'static str> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() < 2 {
        return Vec::new();
    }
    let crate_dir = format!("{}/{}", parts[0], parts[1]);
    reg::STRUCTURAL_STEPS
        .iter()
        .filter(|(_, cwd, cmd)| *cwd == crate_dir || cmd.contains(&crate_dir))
        .map(|(name, _, _)| *name)
        .collect()
}

/// A journey harness that cannot find the debug binary exits 77 (a SKIP),
/// which the packet counts as a failure - so selecting one without its build
/// step produces a false red. The registry owns the build; selection carries
/// it along.
pub fn needs_rust_binary(root: &Path, rel: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(root.join(rel)) else {
        return false;
    };
    reg::RUST_BIN_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
}

/// Turn selections into runner steps, fastest-signal first.
///
/// "Fastest-signal first" has one exception: the graph store's read path is
/// the keeper, and the keeper needs `fno-agents-worker` on disk. A packet
/// that runs pytest before the build makes every graph-touching test crash
/// with StoreUnavailable instead of answering. So when pytest AND the build
/// are both mapped, the build goes first, exactly as the full smoke orders
/// it.
///
/// `shard` (`I/N`, the full lane's contract) splits only the pytest file
/// list, round-robin by sorted-file index: leg k of N always runs the same
/// files, and the non-pytest steps run in every leg because each matrix leg
/// is its own fresh runner.
pub fn changed_steps(root: &Path, selections: &[Selection], shard: &str) -> Vec<disc::Step> {
    let mut pytest_targets: Vec<String> = selections
        .iter()
        .filter(|s| s.kind == "pytest")
        .map(|s| s.target.clone())
        .collect();
    if !shard.is_empty() {
        let (index, total) = crate::test_smoke::parse_smoke_shard(shard).unwrap_or((1, 1));
        let mut sorted: Vec<String> = pytest_targets.clone();
        sorted.sort();
        sorted.dedup();
        pytest_targets = sorted
            .into_iter()
            .enumerate()
            .filter(|(position, _)| position % total == index - 1)
            .map(|(_, t)| t)
            .collect();
    }
    let shell_rels: BTreeSet<String> = selections
        .iter()
        .filter(|s| s.kind == "shell")
        .map(|s| s.target.clone())
        .collect();
    let build_selected = reg::STRUCTURAL_STEPS
        .iter()
        .any(|(n, _, _)| *n == reg::RUST_BUILD_STEP)
        && (selections
            .iter()
            .any(|s| s.kind == "step" && s.target == reg::RUST_BUILD_STEP)
            || shell_rels.iter().any(|rel| needs_rust_binary(root, rel)));
    let mut steps: Vec<disc::Step> = Vec::new();
    let build_step = reg::STRUCTURAL_STEPS
        .iter()
        .find(|(n, _, _)| *n == reg::RUST_BUILD_STEP)
        .map(|(n, c, m)| (n.to_string(), c.to_string(), m.to_string()));
    if !pytest_targets.is_empty() && build_selected {
        if let Some(b) = &build_step {
            steps.push(b.clone());
        }
    }
    if !pytest_targets.is_empty() {
        let mut targets: Vec<String> = pytest_targets.clone();
        targets.sort();
        targets.dedup();
        // xdist costs more than it saves on a handful of files.
        let par = if targets.len() > 3 {
            " -n auto --maxprocesses=4 --dist=loadgroup"
        } else {
            ""
        };
        let quoted: Vec<String> = targets.iter().map(|t| shell_quote_arg(t)).collect();
        steps.push((
            format!("Pytest (changed subset, {} file(s))", targets.len()),
            ".".to_string(),
            format!(
                "uv run --project cli pytest --tb=short -q{par} {}",
                quoted.join(" ")
            ),
        ));
    }
    // The pytest shard DELETES the debug binary on exit (the @requires_rust
    // seam), so any shell harness after pytest needs a build of its own
    // there; after a warm pre-build it is a no-op, never a second compile.
    if shell_rels.iter().any(|rel| needs_rust_binary(root, rel)) && build_step.is_some() {
        if let Some(b) = &build_step {
            steps.push(b.clone());
        }
    }
    for rel in &shell_rels {
        steps.push((
            rel.clone(),
            ".".to_string(),
            format!("bash {}", shell_quote_arg(rel)),
        ));
    }
    let by_name: BTreeMap<&str, &(&str, &str, &str)> =
        reg::STRUCTURAL_STEPS.iter().map(|s| (s.0, s)).collect();
    let step_targets: BTreeSet<&str> = selections
        .iter()
        .filter(|s| s.kind == "step")
        .map(|s| s.target.as_str())
        .collect();
    for name in &step_targets {
        if *name == reg::RUST_BUILD_STEP {
            continue;
        }
        let Some(step) = by_name.get(name) else {
            continue;
        };
        let refs: BTreeSet<String> = shell_refs_in(step.2);
        if !refs.is_empty() && refs.iter().all(|r| shell_rels.contains(r)) {
            // every harness it wraps is already selected directly
            continue;
        }
        steps.push((step.0.to_string(), step.1.to_string(), step.2.to_string()));
    }
    steps
}

fn shell_quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    if arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/_-=.,:@+".contains(c))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

/// The .sh paths one structural step's cmd references, for the dedup above.
fn shell_refs_in(cmd: &str) -> BTreeSet<String> {
    let re = regex::Regex::new(r"[\w./@{}$+-]*\.sh\b").expect("static regex");
    re.find_iter(cmd)
        .map(|m| m.as_str().trim_start_matches("./").to_string())
        .collect()
}

/// (pytest files, shell harnesses) the packet maps, deduped by target. One
/// spelling for both consumers of the counts: the estimate prices them and
/// the receipt line prints them.
fn changed_packet_counts(selections: &[Selection]) -> (usize, usize) {
    let pytest_files = selections
        .iter()
        .filter(|s| s.kind == "pytest")
        .map(|s| s.target.clone())
        .collect::<BTreeSet<_>>()
        .len();
    let shell = selections
        .iter()
        .filter(|s| s.kind == "shell")
        .map(|s| s.target.clone())
        .collect::<BTreeSet<_>>()
        .len();
    (pytest_files, shell)
}

/// Minutes the changed packet plausibly needs, from its selection alone.
///
/// The packet scales with the diff, so its CI cap must too; this estimate is
/// what the changed-smoke cap is sized from. Constants are measured
/// ceilings, not guesses: the full tree runs 894 pytest files in a 1036s
/// step under xdist (about 1.2s a file), so 1.5 tenths of a minute (9s) a
/// file carries roughly 7x headroom; a shell harness is at most about a
/// minute; provisioning gets a flat 5 with the build step adding 3 more.
/// The build price is measured: a Rust-source change invalidates the
/// workspace cargo cache, and the CI rebuild ran 15 to 25 minutes across
/// four consecutive runs on 2026-09-09.
pub fn estimate_changed_minutes(root: &Path, selections: &[Selection]) -> i64 {
    let (pytest_files, shell) = changed_packet_counts(selections);
    let structural: BTreeSet<&str> = selections
        .iter()
        .filter(|s| s.kind == "step")
        .map(|s| s.target.as_str())
        .collect();
    // The build is priced whether it arrives as an explicit rust-family
    // selection or is injected by changed_steps for a harness whose content
    // needs the binary - one spelling of does-the-packet-build.
    let shell_rels: Vec<&String> = selections
        .iter()
        .filter(|s| s.kind == "shell")
        .map(|s| &s.target)
        .collect();
    let build = i64::from(
        structural.contains(reg::RUST_BUILD_STEP)
            || shell_rels.iter().any(|rel| needs_rust_binary(root, rel)),
    );
    // Tenths of a minute, so the arithmetic stays in integers. The build is
    // priced at 150 on its own; the other structural steps at 10 each.
    let tenths = 50
        + (pytest_files as i64 * 3) / 2
        + shell as i64 * 10
        + (structural.len() as i64 - build) * 10
        + build * 150;
    15.max((tenths + 9) / 10)
}

/// Receipt in the changed-mode namespace; never the full record's path.
fn write_changed_receipt(path: &str, payload: &serde_json::Value) {
    if let Some(parent) = Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::to_string_pretty(payload).unwrap_or_else(|_| "{}".into());
    let _ = std::fs::write(path, format!("{body}\n"));
}

fn receipt_path(root: &Path) -> String {
    std::env::var("SMOKE_CHANGED_RECEIPT").unwrap_or_else(|_| {
        root.join(reg::CHANGED_RECEIPT_DEFAULT)
            .to_string_lossy()
            .to_string()
    })
}

/// The changed-mode entry, called from the full runner when `--changed`.
pub fn run_changed(
    root: &Path,
    opts: &SmokeOpts,
    env: &BTreeMap<String, String>,
    _sandbox: &Path,
) -> i32 {
    let t0 = std::time::Instant::now();
    let (paths, reason, resolved_base) = changed_snapshot(root, &opts.base, &opts.head);
    let (rc_head, head_sha) = git(root, &["rev-parse", "HEAD"]);
    let candidate = if rc_head == 0 {
        head_sha.clone()
    } else {
        "unknown".to_string()
    };
    println!(
        "smoke: mode=CHANGED SUBSET - partial evidence; the full 'fno doctor test smoke' gate still decides merge readiness"
    );
    let receipt_file = receipt_path(root);
    if !reason.is_empty() {
        // Overwrite any earlier receipt: leaving the previous run's green in
        // place would let a stale verdict outlive the candidate it described.
        write_changed_receipt(
            &receipt_file,
            &serde_json::json!({
                "mode": "CHANGED SUBSET", "candidate": candidate,
                "verdict": "unevaluated", "reason": reason,
                "base": opts.base, "head": opts.head,
            }),
        );
        eprintln!("smoke: CHANGED SUBSET UNEVALUATED - {reason}");
        eprintln!("smoke: no changed verdict was earned (this is not a green result)");
        return CHANGED_RC_UNEVALUATED;
    }
    let (selections, unmapped) = select_changed(root, &paths);
    let steps = changed_steps(root, &selections, &opts.shard);
    let select_s = t0.elapsed().as_secs_f64();
    let estimate = estimate_changed_minutes(root, &selections);
    println!(
        "smoke: base={} head={} changed={} selected={} unmapped={}",
        if opts.base.is_empty() {
            &resolved_base
        } else {
            &opts.base
        },
        if opts.head.is_empty() {
            candidate[..12.min(candidate.len())].to_string()
        } else {
            opts.head.clone()
        },
        paths.len(),
        steps.len(),
        unmapped.len(),
    );
    let (pytest_files, shell) = changed_packet_counts(&selections);
    println!(
        "smoke: changed-estimate minutes={estimate} pytest_files={pytest_files} shell={shell} steps={}",
        steps.len()
    );
    for s in &selections {
        println!("  select  {:20} {} -> {}", s.rule, s.path, s.target);
    }
    for u in &unmapped {
        println!("  unmapped {u}  (no rule; covered only by the full gate)");
    }

    let mut receipt = serde_json::json!({
        "mode": "CHANGED SUBSET", "shard": opts.shard, "candidate": candidate,
        "base": if opts.base.is_empty() { resolved_base.clone() } else { opts.base.clone() },
        "head": if opts.head.is_empty() { candidate.clone() } else { opts.head.clone() },
        "changed_paths": paths,
        "unmapped_paths": unmapped,
        "selections": selections.iter().map(|s| serde_json::json!({
            "rule": s.rule, "path": s.path, "kind": s.kind, "target": s.target,
        })).collect::<Vec<_>>(),
        "selected_count": steps.len(),
        "unmapped_count": unmapped.len(),
        "selection_seconds": (select_s * 1000.0).round() / 1000.0,
    });

    if opts.list {
        return 0;
    }
    if steps.is_empty() {
        receipt["verdict"] = serde_json::json!("unselected");
        receipt["execution_seconds"] = serde_json::json!(0.0);
        receipt["first_signal_seconds"] = serde_json::json!(null);
        write_changed_receipt(&receipt_file, &receipt);
        eprintln!("smoke: CHANGED SUBSET selected NOTHING - that is evidence about the selector, not that the change is safe");
        return CHANGED_RC_UNSELECTED;
    }

    let cmds: Vec<&str> = steps.iter().map(|(_, _, c)| c.as_str()).collect();
    if let Some(miss) = crate::test_smoke_discovery::smoke_prereqs(&cmds) {
        receipt["verdict"] = serde_json::json!("unevaluated");
        receipt["reason"] = serde_json::json!(format!("missing prerequisite: {miss}"));
        write_changed_receipt(&receipt_file, &receipt);
        eprintln!("smoke: missing prerequisite: {miss}");
        return CHANGED_RC_PREREQ;
    }

    // Same faithful-ordering guard the full run applies: the pytest step must
    // see NO checkout fno-agents binary so the @requires_rust parity tests
    // skip, as they do in CI. The claim door is preserved outside target/
    // first.
    let mut env = env.clone();
    if steps
        .iter()
        .any(|(n, _, _)| n.starts_with("Pytest (changed subset"))
    {
        crate::test_smoke::preserve_claim_door(&root, &mut env);
        let has_build = steps.iter().any(|(n, _, _)| n == reg::RUST_BUILD_STEP);
        if !has_build {
            crate::test_smoke::scrub_target_bins(&root);
        }
    }

    let e0 = std::time::Instant::now();
    let indices: Vec<usize> = (0..steps.len()).collect();
    let (results, mut rc) =
        crate::test_smoke::execute_steps(root, &env, &steps, &indices, opts.keep_going, "");
    let exec_s = e0.elapsed().as_secs_f64();
    if rc == CHANGED_RC_UNSELECTED || rc == CHANGED_RC_UNEVALUATED || rc == CHANGED_RC_PREREQ {
        // In-band signalling hazard. Propagating a child code that happens to
        // equal a sentinel would tell preflight "nothing selected" / "no
        // trustworthy diff", and it would fall THROUGH to the full gate
        // instead of stopping on a real failure - a red silently downgraded
        // to a non-verdict. The sentinels have to stay unambiguous, so a
        // genuine step failure that collides with one reports as a plain 1.
        println!("smoke: step exit {rc} collides with a changed-mode sentinel; reporting 1 (a failure, not a non-verdict)");
        rc = 1;
    }
    // Time to first signal: elapsed up to and including the first failing
    // step, or the whole packet when it is green. Summing every step would
    // overstate it under --keep-going, where execution continues past the
    // first failure.
    let mut signal = 0.0;
    for (_, status, dur) in &results {
        signal += dur;
        if status == "fail" {
            break;
        }
    }

    println!();
    println!("smoke: summary (changed-subset, {} steps)", results.len());
    for (name, status, dur) in &results {
        println!("  {status:6} {dur:4.0}s  {name}");
    }
    let verdict = if rc != 0 { "red" } else { "green" };
    println!(
        "smoke: CHANGED SUBSET verdict={verdict} selected={} unmapped={} select={select_s:.1}s exec={exec_s:.1}s first_signal={signal:.1}s",
        steps.len(),
        unmapped.len()
    );
    if verdict == "green" {
        println!("smoke: CHANGED SUBSET green means the selected packet passed - run 'fno doctor test smoke' full before the settle-green push");
    }

    let failed: Vec<&str> = results
        .iter()
        .filter(|(_, s, _)| s == "fail")
        .map(|(n, _, _)| n.as_str())
        .collect();
    receipt["verdict"] = serde_json::json!(verdict);
    receipt["execution_seconds"] = serde_json::json!((exec_s * 1000.0).round() / 1000.0);
    receipt["first_signal_seconds"] = serde_json::json!((signal * 1000.0).round() / 1000.0);
    receipt["failed"] = serde_json::json!(failed);
    // The receipt is the changed mode's ONLY durable artifact, and it lives
    // in its own namespace - the full runner's failure record is never
    // touched.
    write_changed_receipt(&receipt_file, &receipt);
    rc
}
