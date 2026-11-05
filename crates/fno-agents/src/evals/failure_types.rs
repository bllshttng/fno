//! Named failure types and the labelled cases that prove their graders.
//!
//! `evals/bank/failure-types.yaml` names each type and its cost tag. A type's
//! grader is the `grade:` list of each golden labelled with it. Each golden
//! has a fixture at `evals/fixtures/metrics/<golden id>.yaml` with good and
//! bad cases. A type counts as proved only when every one of its goldens
//! passes each good case and fails each bad case, with at least
//! `MIN_CASES` of each. Proving spends no model: the graders run on files.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use super::bank::TaskSpec;

pub const FILENAME: &str = "failure-types.yaml";
/// The label on a golden that records a passing case. It names no failure.
pub const NO_FAILURE: &str = "none";
const MIN_CASES: usize = 3;
const CASE_TIMEOUT_S: i64 = 60;

#[derive(Deserialize)]
pub struct FailureType {
    pub id: String,
    pub cost: String,
    pub description: String,
}

/// `cost_order` lists the cost tags, costliest first.
#[derive(Deserialize)]
pub struct Catalog {
    pub cost_order: Vec<String>,
    pub types: Vec<FailureType>,
}

impl Catalog {
    pub fn load(bank_dir: &Path) -> Result<Catalog, String> {
        let path = bank_dir.join(FILENAME);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let catalog: Catalog = serde_yaml_ng::from_str(&text)
            .map_err(|e| format!("malformed {}: {e}", path.display()))?;
        for t in &catalog.types {
            if !catalog.cost_order.contains(&t.cost) {
                return Err(format!(
                    "{}: type '{}' has cost '{}', which cost_order does not list",
                    path.display(),
                    t.id,
                    t.cost
                ));
            }
        }
        Ok(catalog)
    }
}

#[derive(Deserialize)]
pub(crate) struct Fixture {
    /// Repo files the grader needs, copied into each case.
    #[serde(default)]
    copy: Vec<String>,
    pub(crate) cases: Vec<Case>,
}

#[derive(Deserialize)]
pub(crate) struct Case {
    pub(crate) id: String,
    label: Label,
    /// The files one case writes; a rubric spend or agreement run reads the
    /// work product out of them.
    #[serde(default)]
    pub(crate) files: BTreeMap<String, String>,
    /// Stub binaries by name, shell script bodies. They lead the grader's
    /// PATH, so a CLI golden is proved with no real binary and no network.
    #[serde(default)]
    stub: BTreeMap<String, String>,
}

#[derive(Deserialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum Label {
    Good,
    Bad,
}

/// One type's proof. `failures` names each case or golden that broke it.
pub struct Proof {
    pub proved: bool,
    pub failures: Vec<String>,
}

/// The metrics fixture directory beside the bank.
fn metrics_dir(bank_dir: &Path) -> std::path::PathBuf {
    bank_dir.join("../fixtures/metrics")
}

/// A golden's parsed metrics fixture.
pub(crate) fn load_fixture(golden_id: &str, bank_dir: &Path) -> Result<Fixture, String> {
    let path = metrics_dir(bank_dir).join(format!("{golden_id}.yaml"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_yaml_ng::from_str(&text).map_err(|e| format!("malformed {}: {e}", path.display()))
}

/// The `--spend` run: real judges grade every fixture case of a rubric
/// golden and the verdicts land whole in `<metrics>/<id>.verdicts.jsonl`.
/// A diff-sourced work product refuses: fixture cases hold file contents.
pub(crate) fn record_rubric_fixture(
    golden: &TaskSpec,
    bank_dir: &Path,
    judge: &dyn crate::evals::rubric::Judge,
) -> Result<usize, String> {
    let fixture = load_fixture(&golden.id, bank_dir)?;
    let check = golden
        .grade
        .iter()
        .find(|c| c.rubric.is_some())
        .ok_or_else(|| format!("{}: no rubric check on the golden", golden.id))?;
    let spec = check
        .rubric
        .as_ref()
        .ok_or_else(|| format!("{}: rubric spec missing", golden.id))?;
    let Some(rel) = &spec.path else {
        return Err(format!(
            "{}: a diff-sourced rubric check has no spend form; use a path work product",
            golden.id
        ));
    };
    let prompt = golden
        .prompt
        .as_deref()
        .ok_or_else(|| format!("{}: no task prompt", golden.id))?;
    let mut rows: Vec<String> = Vec::new();
    for case in &fixture.cases {
        let work_product = case.files.get(rel).ok_or_else(|| {
            format!(
                "{}: fixture case '{}' does not write the work product {rel}",
                golden.id, case.id
            )
        })?;
        for criterion in &spec.criteria {
            let ask = crate::evals::rubric::RubricAsk {
                task_prompt: prompt,
                criterion,
                work_product,
            };
            for v in judge.panel(&ask) {
                let verdict = match &v.fault {
                    Some(why) => format!("fault: {why}"),
                    None => {
                        if v.passed {
                            "pass".to_string()
                        } else {
                            "fail".to_string()
                        }
                    }
                };
                // serde_json escapes the verdict text; a fault carrying
                // quotes or backslashes must not corrupt the record line.
                rows.push(
                    serde_json::json!({
                        "case": case.id,
                        "criterion": criterion.id,
                        "lane": v.lane,
                        "verdict": verdict,
                        "work_product_sha": sha256_text(work_product),
                        "prompt_sha": crate::evals::rubric::prompt_sha(&ask),
                    })
                    .to_string(),
                );
            }
        }
    }
    let out = metrics_dir(bank_dir).join(format!("{}.verdicts.jsonl", golden.id));
    std::fs::write(&out, format!("{}\n", rows.join("\n")))
        .map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    Ok(rows.len())
}

/// The hex sha256 of one text, the hash a recorded verdict row carries.
fn sha256_text(text: &str) -> String {
    crate::evals::rubric::text_sha256(text)
}

fn run_case(
    golden: &TaskSpec,
    fixture: &Fixture,
    case: &Case,
    repo_root: &Path,
    metrics: &Path,
) -> Result<bool, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let write = |rel: &str, bytes: &[u8]| -> Result<(), String> {
        let target = dir.path().join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(target, bytes).map_err(|e| e.to_string())
    };
    for rel in &fixture.copy {
        let bytes = std::fs::read(repo_root.join(rel)).map_err(|e| format!("copy {rel}: {e}"))?;
        write(rel, &bytes)?;
    }
    for (rel, body) in &case.files {
        write(rel, body.as_bytes())?;
    }
    // Outside the graded dir, so a grader that lists files never sees the stub.
    let bin = tempfile::tempdir().map_err(|e| e.to_string())?;
    for (name, body) in &case.stub {
        // A child sh writes the bytes, so no write fd of this process can leak
        // into a sibling fork and fail the exec with Text file busy.
        let path = bin.path().join(name);
        let wrote = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                r#"printf '#!/bin/sh\n%s\n' "$2" > "$1" && chmod 755 "$1""#,
                "sh",
            ])
            .arg(&path)
            .arg(body)
            .status()
            .map_err(|e| e.to_string())?;
        if !wrote.success() {
            return Err(format!("could not write stub {name}: {wrote}"));
        }
    }
    let judge = super::rubric::RecordedJudge::load(metrics, &golden.id, &case.id);
    let outcome = super::grading::grade_checks(
        &golden.grade,
        dir.path(),
        CASE_TIMEOUT_S,
        None,
        Some(bin.path()),
        golden.prompt.as_deref(),
        Some(&judge),
    );
    Ok(outcome.passed)
}

/// What one golden's fixture proves: the empty list means its grader holds.
pub fn golden_failures(golden: &TaskSpec, bank_dir: &Path) -> Vec<String> {
    let id = &golden.id;
    let path = metrics_dir(bank_dir).join(format!("{id}.yaml"));
    let fixture: Fixture = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_yaml_ng::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(f) => f,
        Err(e) => {
            return vec![format!(
                "{id}: no readable fixture at {}: {e}",
                path.display()
            )]
        }
    };
    let mut failures = Vec::new();
    for (label, word) in [(Label::Good, "good"), (Label::Bad, "bad")] {
        let n = fixture.cases.iter().filter(|c| c.label == label).count();
        if n < MIN_CASES {
            failures.push(format!("{id}: {n} {word} cases, needs {MIN_CASES}"));
        }
    }
    let repo_root = bank_dir.join("../..");
    for case in &fixture.cases {
        let want = case.label == Label::Good;
        match run_case(golden, &fixture, case, &repo_root, &metrics_dir(bank_dir)) {
            Ok(got) if got == want => {}
            Ok(true) => failures.push(format!("{id}: grader passed bad case '{}'", case.id)),
            Ok(false) => failures.push(format!("{id}: grader failed good case '{}'", case.id)),
            Err(e) => failures.push(format!("{id}: case '{}' could not run: {e}", case.id)),
        }
    }
    failures
}

/// The proof of every failure type the catalog names or a golden carries. A
/// type with no golden has no grader, so it reads not proved.
pub fn prove(catalog: &Catalog, tasks: &[TaskSpec], bank_dir: &Path) -> BTreeMap<String, Proof> {
    let mut by_type: BTreeMap<String, Vec<&TaskSpec>> = catalog
        .types
        .iter()
        .map(|t| (t.id.clone(), Vec::new()))
        .collect();
    for task in tasks {
        if let Some(g) = &task.golden {
            if g.failure_type != NO_FAILURE {
                by_type
                    .entry(g.failure_type.clone())
                    .or_default()
                    .push(task);
            }
        }
    }
    let listed = |id: &str| catalog.types.iter().any(|t| t.id == id);
    by_type
        .into_iter()
        .map(|(id, goldens)| {
            let mut failures: Vec<String> = goldens
                .iter()
                .flat_map(|g| golden_failures(g, bank_dir))
                .collect();
            if goldens.is_empty() {
                failures.push("no golden carries this type".into());
            }
            if !listed(&id) {
                failures.push(format!("{FILENAME} does not list this type"));
            }
            let proof = Proof {
                proved: failures.is_empty(),
                failures,
            };
            (id, proof)
        })
        .collect()
}

/// The goldens whose type is not proved. They read `unproved` in a compare,
/// and they cannot fail a gate.
pub fn unproved_tasks(bank_dir: &Path) -> Result<std::collections::BTreeSet<String>, String> {
    let tasks = super::bank::discover_bank(bank_dir).map_err(|e| e.0)?;
    let catalog = Catalog::load(bank_dir)?;
    let proofs = prove(&catalog, &tasks, bank_dir);
    Ok(tasks
        .iter()
        .filter(|t| {
            t.golden.as_ref().is_some_and(|g| {
                g.failure_type != NO_FAILURE
                    && !proofs.get(&g.failure_type).is_some_and(|p| p.proved)
            })
        })
        .map(|t| t.id.clone())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden_task(dir: &Path, id: &str) -> TaskSpec {
        let body = format!(
            "id: {id}\ntier: capability\ngrade:\n  - kind: grep\n    path: a\n    pattern: ok\ngolden:\n  source: {{kind: c, ref: r}}\n  expected: e\n  rule: r\n  failure_type: t\n  outcome: failed\n"
        );
        let path = dir.join(format!("{id}.yaml"));
        std::fs::write(&path, body).unwrap();
        super::super::bank::load_task(&path).unwrap()
    }

    #[test]
    fn a_grader_that_passes_a_bad_case_reads_not_proved_and_names_the_case() {
        let root = tempfile::tempdir().unwrap();
        let bank = root.path().join("evals/bank");
        let metrics = root.path().join("evals/fixtures/metrics");
        std::fs::create_dir_all(&bank).unwrap();
        std::fs::create_dir_all(&metrics).unwrap();
        let task = golden_task(&bank, "g");
        let catalog = Catalog {
            cost_order: vec!["rework".into()],
            types: vec![FailureType {
                id: "t".into(),
                cost: "rework".into(),
                description: "d".into(),
            }],
        };
        let case = |id: &str, label: &str, body: &str| {
            format!("  - {{id: {id}, label: {label}, files: {{a: \"{body}\"}}}}\n")
        };
        let good: String = (0..3)
            .map(|i| case(&format!("g{i}"), "good", "ok"))
            .collect();
        let mut fixture = format!("cases:\n{good}");
        fixture.push_str(&case("b0", "bad", "no"));
        fixture.push_str(&case("b1", "bad", "no"));
        fixture.push_str(&case("slips-through", "bad", "ok"));
        std::fs::write(metrics.join("g.yaml"), &fixture).unwrap();

        let proofs = prove(&catalog, std::slice::from_ref(&task), &bank);
        assert!(!proofs["t"].proved);
        assert_eq!(
            proofs["t"].failures,
            ["g: grader passed bad case 'slips-through'"]
        );

        let fixed = fixture.replace(
            "slips-through, label: bad, files: {a: \"ok\"}",
            "b2, label: bad, files: {a: \"no\"}",
        );
        std::fs::write(metrics.join("g.yaml"), &fixed).unwrap();
        assert!(prove(&catalog, std::slice::from_ref(&task), &bank)["t"].proved);

        let short = fixed.replace("  - {id: b2, label: bad, files: {a: \"no\"}}\n", "");
        std::fs::write(metrics.join("g.yaml"), short).unwrap();
        assert_eq!(
            prove(&catalog, std::slice::from_ref(&task), &bank)["t"].failures,
            ["g: 2 bad cases, needs 3"]
        );
    }

    #[test]
    fn the_repo_catalog_is_proved_and_every_golden_type_is_listed() {
        let bank = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/bank");
        let tasks = super::super::bank::discover_bank(&bank).unwrap();
        let catalog = Catalog::load(&bank).unwrap();
        let proofs = prove(&catalog, &tasks, &bank);
        let broken: Vec<String> = proofs
            .iter()
            .flat_map(|(id, p)| p.failures.iter().map(move |f| format!("{id}: {f}")))
            // The seed rubric golden reads unproved until its verdict record
            // lands (report --check-metrics --spend); that is the fence
            // working, not a regression.
            .filter(|f| !f.contains("golden-rubric-no-new-python"))
            .collect();
        assert!(broken.is_empty(), "{broken:#?}");
        for task in &tasks {
            let Some(g) = &task.golden else { continue };
            if g.failure_type == NO_FAILURE {
                continue;
            }
            let t = catalog
                .types
                .iter()
                .find(|t| t.id == g.failure_type)
                .unwrap();
            assert_eq!(g.cost.as_deref(), Some(t.cost.as_str()), "{}", task.id);
        }
    }
}
