//! The eval bank: task format + loader + load-time discipline
//! (the deleted Python leg's bank.py, ported per the dual-implementation
//! protocol). A bank task is one evals/bank/<id>.yaml file, or a
//! skills/<name>/evals/<id>.yaml file beside the skill it grades; the two
//! disciplines: a task without a runnable grade is rejected naming the id
//! and file, and an all-trivial grade warns so a task cannot silently
//! always-pass. The goldens pin the bytes.

pub const VALID_TIERS_REPR: &str = "('capability', 'regression')";
pub const VALID_CHECK_KINDS_REPR: &str =
    "('exit', 'file-exists', 'grep', 'tool-budget', 'tests', 'rubric')";
pub const VALID_CRITERION_TAGS_REPR: &str = "('hurdle', 'principle')";

pub const COHORTS_FILENAME: &str = "cohorts.yaml";

/// One mechanical grade check. `kind` selects the params that matter.
#[derive(Clone, Debug)]
pub struct GradeCheck {
    pub kind: String,
    pub command: Option<String>,
    pub expect: i64,
    pub path: Option<String>,
    pub pattern: Option<String>,
    pub max_tool_calls: Option<i64>,
    pub tests: Option<TestsSpec>,
    pub rubric: Option<RubricSpec>,
}

/// hurdle: a failed criterion fails the whole check. principle: it only
/// lowers the criterion pass rate the detail reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RubricTag {
    Hurdle,
    Principle,
}

/// One binary criterion: one fact, outcome only, no scale.
#[derive(Clone, Debug)]
pub struct RubricCriterion {
    pub id: String,
    pub text: String,
    pub tag: RubricTag,
}

/// A `rubric` check: judges read one work product (`path`, or the git diff
/// against the grade base when `diff`) against one-fact criteria. The task
/// must be capability-tier and carry a prompt: the judge sees it.
#[derive(Clone, Debug)]
pub struct RubricSpec {
    pub path: Option<String>,
    pub diff: bool,
    pub criteria: Vec<RubricCriterion>,
}

/// A `tests` check: re-apply the real fix's test files, run the named ids, and
/// pass only when every fail-to-pass and pass-to-pass id reads ok.
#[derive(Clone, Debug)]
pub struct TestsSpec {
    pub tests_ref: String,
    pub test_files: Vec<String>,
    pub package: String,
    pub f2p: Vec<String>,
    pub p2p: Vec<String>,
}

impl TaskSpec {
    /// A work-product task grades the fix's tests, so its fixture is the fix's
    /// parent commit and `--ref` varies only the rules a worker reads.
    pub fn is_work_product(&self) -> bool {
        self.grade.iter().any(|c| c.tests.is_some())
    }
}

/// Why a task exists: the real case it came from and the rule it holds. A
/// golden states its rule, so a failure names what broke and not only a task.
#[derive(Clone, Debug)]
pub struct Golden {
    pub source_kind: String,
    pub source_ref: String,
    pub expected: String,
    pub rule: String,
    pub failure_type: String,
    pub cost: Option<String>,
    /// `failed` or `passed`: what the source case did. Not a current result.
    pub outcome: String,
}

/// A loaded, validated bank task. A task with no prompt is grade-only:
/// the runner skips the worker spawn and grades the fixture directly.
#[derive(Clone, Debug)]
pub struct TaskSpec {
    pub id: String,
    pub tier: String,
    pub grade: Vec<GradeCheck>,
    pub prompt: Option<String>,
    pub repo_fixture: String,
    pub timeout_minutes: i64,
    pub tags: Vec<String>,
    /// Source path prefixes this task covers. A PR head that changes one runs
    /// the task, with no `gate` tag needed.
    pub covers: Vec<String>,
    pub golden: Option<Golden>,
    pub source_path: std::path::PathBuf,
}

/// The tag `evals graduate --demote` writes on a task whose score predicts
/// nothing. A person ships it in a PR; nothing demotes at runtime.
pub const DEMOTED_TAG: &str = "demoted";

impl TaskSpec {
    /// A demoted task stays in the bank but leaves tag and gate selection.
    /// `--task <id>` still runs it.
    pub fn is_demoted(&self) -> bool {
        self.tags.iter().any(|tag| tag == DEMOTED_TAG)
    }
}

/// The load-time discipline refusals name the id and file.
#[derive(Debug)]
pub struct BankError(pub String);

/// Why loading one bank task refused. The unknown-kind arm marks the one
/// refusal a newer bank produces in an older binary: the gate's head-leg
/// read skips that file, every other caller refuses the whole bank.
#[derive(Debug)]
pub enum LoadError {
    UnknownCheckKind(BankError),
    Refusal(BankError),
}

impl From<BankError> for LoadError {
    fn from(e: BankError) -> Self {
        LoadError::Refusal(e)
    }
}

impl From<LoadError> for BankError {
    fn from(e: LoadError) -> Self {
        match e {
            LoadError::UnknownCheckKind(inner) | LoadError::Refusal(inner) => inner,
        }
    }
}

impl std::fmt::Display for BankError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn require(cond: bool, msg: String) -> Result<(), BankError> {
    if cond {
        Ok(())
    } else {
        Err(BankError(msg))
    }
}

/// The display form Python's frozenset repr takes at seed 0 for the
/// trivial commands (the capture pins PYTHONHASHSEED=0).
pub const TRIVIAL_COMMANDS_REPR: &str = "frozenset({'', 'true', ':'})";

/// Load and validate one bank YAML file; refusals name the id and file.
pub fn load_task(path: &std::path::Path) -> Result<TaskSpec, BankError> {
    load_task_typed(path).map_err(Into::into)
}

/// The same load with the unknown-kind refusal left marked, so the gate's
/// tolerant head-leg read can skip the file instead of failing the bank.
fn load_task_typed(path: &std::path::Path) -> Result<TaskSpec, LoadError> {
    let raw: serde_yaml_ng::Value = match std::fs::read(path) {
        Ok(bytes) => serde_yaml_ng::from_slice(&bytes).map_err(|e| {
            BankError(format!(
                "malformed YAML in bank task {}: {}",
                path.display(),
                e
            ))
        })?,
        Err(e) => {
            return Err(
                BankError(format!("cannot read bank task {}: {}", path.display(), e)).into(),
            )
        }
    };
    let Some(map) = raw.as_mapping() else {
        return Err(BankError(format!(
            "bank task {}: top level must be a mapping",
            path.display()
        ))
        .into());
    };
    let id_v = map.get(serde_yaml_ng::Value::String("id".into()));
    let Some(id) = id_v.and_then(|v| v.as_str()) else {
        return Err(BankError(format!("bank task {}: missing 'id'", path.display())).into());
    };
    let id = id.to_string();
    let where_ = path.display().to_string();
    let tier = map.get(serde_yaml_ng::Value::String("tier".into()));
    let tier = tier.and_then(|v| v.as_str()).unwrap_or("");
    let tiers =
        format!("task '{id}' ({where_}): tier must be one of {VALID_TIERS_REPR}, got '{tier}'");
    require(tier == "capability" || tier == "regression", tiers)?;

    let mut prompt: Option<String> = map
        .get(serde_yaml_ng::Value::String("prompt".into()))
        .and_then(|v| v.as_str().map(str::to_string));
    if let Some(p) = &prompt {
        if p.trim().is_empty() {
            prompt = None;
        }
    }
    let grade_list = map.get(serde_yaml_ng::Value::String("grade".into()));
    let Some(grade_raw) = grade_list.and_then(|v| v.as_sequence()) else {
        return Err(BankError(format!("task '{id}' ({where_}): 'grade' must be a non-empty list of mechanical checks (a task with no runnable grade is invalid)")).into());
    };
    if grade_raw.is_empty() {
        return Err(BankError(format!("task '{id}' ({where_}): 'grade' must be a non-empty list of mechanical checks (a task with no runnable grade is invalid)")).into());
    };
    let mut checks: Vec<GradeCheck> = Vec::new();
    for (i, item) in grade_raw.iter().enumerate() {
        checks.push(parse_check(item, &id, path, i)?);
    }
    warn_if_decorative(&id, path, &checks);
    // A rubric check is judge-graded: it can never sit on a regression task
    // (a verdict that flips without the tree changing cannot serve
    // `tier: regression`), and it grades a worker's answer, so the task
    // needs the prompt the judge reads.
    if checks.iter().any(|c| c.rubric.is_some()) {
        require(
            tier == "capability",
            format!(
                "task '{id}' ({where_}): a rubric check requires tier capability, got '{tier}'"
            ),
        )?;
        require(
            prompt.is_some(),
            format!(
                "task '{id}' ({where_}): a rubric check requires a task 'prompt' (the judge reads it)"
            ),
        )?;
    }
    let work_product = checks.iter().any(|c| c.tests.is_some());
    let timeout = map.get(serde_yaml_ng::Value::String("timeout_minutes".into()));
    let timeout = timeout.and_then(|v| v.as_i64()).unwrap_or(15);
    require(
        timeout > 0,
        format!("task '{id}' ({where_}): 'timeout_minutes' must be a positive int"),
    )?;
    let tags: Vec<String> = map
        .get(serde_yaml_ng::Value::String("tags".into()))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let covers = parse_covers(
        map.get(serde_yaml_ng::Value::String("covers".into())),
        &id,
        path,
    )?;
    let golden = match map.get(serde_yaml_ng::Value::String("golden".into())) {
        Some(raw) => Some(parse_golden(raw, &id, path)?),
        None => None,
    };
    if work_product {
        require(
            golden.is_some(),
            format!("task '{id}' ({where_}): a tests task requires a golden block (it runs in a sealed tree)"),
        )?;
        let qualified = map.get(serde_yaml_ng::Value::String("qualified".into()));
        require(
            qualified.and_then(|q| q.as_mapping()).is_some_and(|q| {
                q.contains_key(serde_yaml_ng::Value::String("gold_rev".into()))
            }),
            format!("task '{id}' ({where_}): a tests task requires a qualified block written by a green gold solve"),
        )?;
    }
    Ok(TaskSpec {
        id,
        golden,
        covers,
        tier: tier.to_string(),
        prompt,
        grade: checks,
        repo_fixture: map
            .get(serde_yaml_ng::Value::String("repo_fixture".into()))
            .and_then(|v| v.as_str())
            .unwrap_or("HEAD")
            .to_string(),
        timeout_minutes: timeout,
        tags,
        source_path: path.to_path_buf(),
    })
}

/// `covers:` is a list of directory prefixes. A prefix with no trailing `/`
/// would match a sibling (`crates/fno` also names `crates/fno-agents`).
fn parse_covers(
    raw: Option<&serde_yaml_ng::Value>,
    task_id: &str,
    path: &std::path::Path,
) -> Result<Vec<String>, BankError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let bad = || {
        BankError(format!(
            "task '{task_id}' ({}): 'covers' must be a list of path prefixes ending in '/'",
            path.display()
        ))
    };
    let seq = raw.as_sequence().ok_or_else(bad)?;
    seq.iter()
        .map(|v| match v.as_str() {
            Some(s) if s.ends_with('/') => Ok(s.to_string()),
            _ => Err(bad()),
        })
        .collect()
}

fn parse_golden(
    raw: &serde_yaml_ng::Value,
    task_id: &str,
    path: &std::path::Path,
) -> Result<Golden, BankError> {
    let prefix = format!("task '{task_id}' ({}) golden", path.display());
    let Some(map) = raw.as_mapping() else {
        return Err(BankError(format!("{prefix}: must be a mapping")));
    };
    let text = |m: &serde_yaml_ng::Mapping, key: &str| -> Option<String> {
        m.get(serde_yaml_ng::Value::String(key.into()))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let need = |key: &str| {
        text(map, key).ok_or_else(|| BankError(format!("{prefix}: requires a non-empty '{key}'")))
    };
    let source = map
        .get(serde_yaml_ng::Value::String("source".into()))
        .and_then(|v| v.as_mapping());
    let source_field = |key: &str| {
        source
            .and_then(|m| text(m, key))
            .ok_or_else(|| BankError(format!("{prefix}: requires a non-empty 'source.{key}'")))
    };
    let outcome = need("outcome")?;
    require(
        outcome == "failed" || outcome == "passed",
        format!("{prefix}: outcome must be 'failed' or 'passed', got '{outcome}'"),
    )?;
    Ok(Golden {
        source_kind: source_field("kind")?,
        source_ref: source_field("ref")?,
        expected: need("expected")?,
        rule: need("rule")?,
        failure_type: need("failure_type")?,
        cost: text(map, "cost"),
        outcome,
    })
}

fn parse_check(
    raw: &serde_yaml_ng::Value,
    task_id: &str,
    path: &std::path::Path,
    index: usize,
) -> Result<GradeCheck, LoadError> {
    let where_ = path.display().to_string();
    let where_prefix = format!("task '{task_id}' ({where_}) grade[{index}]");
    let Some(map) = raw.as_mapping() else {
        return Err(BankError(format!(
            "{where_prefix}: each grade check must be a mapping"
        ))
        .into());
    };
    let kind = map.get(serde_yaml_ng::Value::String("kind".into()));
    let kind = kind.and_then(|v| v.as_str()).unwrap_or("");
    if !matches!(
        kind,
        "exit" | "file-exists" | "grep" | "tool-budget" | "tests" | "rubric"
    ) {
        return Err(LoadError::UnknownCheckKind(BankError(format!(
            "{where_prefix}: kind must be one of {VALID_CHECK_KINDS_REPR}, got '{kind}'"
        ))));
    }
    if kind == "exit" {
        let command = map.get(serde_yaml_ng::Value::String("command".into()));
        let command = command.and_then(|v| v.as_str()).unwrap_or("");
        require(
            !command.trim().is_empty(),
            format!("{where_prefix}: kind=exit requires a non-empty command"),
        )?;
        let expect = map.get(serde_yaml_ng::Value::String("expect".into()));
        let expect = expect.and_then(|v| v.as_i64()).unwrap_or(0);
        return Ok(GradeCheck {
            kind: kind.to_string(),
            command: Some(command.to_string()),
            expect,
            path: None,
            pattern: None,
            max_tool_calls: None,
            tests: None,
            rubric: None,
        });
    }
    if kind == "tests" {
        return parse_tests(map, kind, &where_prefix);
    }
    if kind == "rubric" {
        return parse_rubric(map, kind, &where_prefix);
    }
    if kind == "tool-budget" {
        let max = map.get(serde_yaml_ng::Value::String("max_tool_calls".into()));
        let max = max.and_then(|v| v.as_i64()).unwrap_or(0);
        require(
            max > 0,
            format!(
                "{where_prefix}: kind=tool-budget requires max_tool_calls as a positive integer"
            ),
        )?;
        return Ok(GradeCheck {
            kind: kind.to_string(),
            command: None,
            expect: 0,
            path: None,
            pattern: None,
            max_tool_calls: Some(max),
            tests: None,
            rubric: None,
        });
    }
    let fpath = map.get(serde_yaml_ng::Value::String("path".into()));
    let fpath = fpath.and_then(|v| v.as_str()).unwrap_or("");
    require(
        !fpath.trim().is_empty(),
        format!("{where_prefix}: requires a non-empty path"),
    )?;
    if kind == "file-exists" {
        return Ok(GradeCheck {
            kind: kind.to_string(),
            command: None,
            expect: 0,
            path: Some(fpath.to_string()),
            pattern: None,
            max_tool_calls: None,
            tests: None,
            rubric: None,
        });
    }
    let pattern = map.get(serde_yaml_ng::Value::String("pattern".into()));
    let pattern = pattern.and_then(|v| v.as_str()).unwrap_or("");
    require(
        !pattern.is_empty(),
        format!("{where_prefix}: requires a non-empty pattern"),
    )?;
    Ok(GradeCheck {
        kind: kind.to_string(),
        command: None,
        expect: 0,
        path: Some(fpath.to_string()),
        pattern: Some(pattern.to_string()),
        max_tool_calls: None,
        tests: None,
        rubric: None,
    })
}

/// `rubric`: one work-product source (`path`, or `diff: true`) and a
/// criteria list where each row is one binary fact tagged hurdle or
/// principle. Refusals name the criterion, so a bad row is findable.
fn parse_rubric(
    map: &serde_yaml_ng::Mapping,
    kind: &str,
    where_prefix: &str,
) -> Result<GradeCheck, BankError> {
    let has_path = map
        .get(serde_yaml_ng::Value::String("path".into()))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    let diff = map
        .get(serde_yaml_ng::Value::String("diff".into()))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    require(
        has_path ^ diff,
        format!(
            "{where_prefix}: kind=rubric requires exactly one work-product source, 'path' or 'diff: true'"
        ),
    )?;
    let path = if has_path {
        Some(
            map.get(serde_yaml_ng::Value::String("path".into()))
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .trim()
                .to_string(),
        )
    } else {
        None
    };
    let raw = map.get(serde_yaml_ng::Value::String("criteria".into()));
    let Some(seq) = raw.and_then(|v| v.as_sequence()) else {
        return Err(BankError(format!(
            "{where_prefix}: kind=rubric requires a non-empty 'criteria' list"
        )));
    };
    if seq.is_empty() {
        return Err(BankError(format!(
            "{where_prefix}: kind=rubric requires a non-empty 'criteria' list"
        )));
    }
    let mut criteria: Vec<RubricCriterion> = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let row_prefix = format!("{where_prefix} criteria[{i}]");
        let Some(row) = item.as_mapping() else {
            return Err(BankError(format!(
                "{row_prefix}: each criterion must be a mapping with id, text and tag"
            )));
        };
        let field = |key: &str| -> Option<String> {
            row.get(serde_yaml_ng::Value::String(key.into()))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let cid = field("id")
            .ok_or_else(|| BankError(format!("{row_prefix}: requires a non-empty 'id'")))?;
        let text = field("text").ok_or_else(|| {
            BankError(format!(
                "{row_prefix} ({cid}): requires a non-empty 'text' (one fact)"
            ))
        })?;
        let tag_word = field("tag").ok_or_else(|| {
            BankError(format!(
                "{row_prefix} ({cid}): requires a 'tag' of {VALID_CRITERION_TAGS_REPR}"
            ))
        })?;
        let tag = match tag_word.as_str() {
            "hurdle" => RubricTag::Hurdle,
            "principle" => RubricTag::Principle,
            _ => {
                return Err(BankError(format!(
                    "{row_prefix} ({cid}): tag must be one of {VALID_CRITERION_TAGS_REPR}, got '{tag_word}'"
                )))
            }
        };
        if criteria.iter().any(|c| c.id == cid) {
            return Err(BankError(format!(
                "{where_prefix}: duplicate criterion id '{cid}'"
            )));
        }
        criteria.push(RubricCriterion { id: cid, text, tag });
    }
    let spec = RubricSpec {
        path: path.clone(),
        diff,
        criteria,
    };
    Ok(GradeCheck {
        kind: kind.to_string(),
        command: None,
        expect: 0,
        path,
        pattern: None,
        max_tool_calls: None,
        tests: None,
        rubric: Some(spec),
    })
}

fn parse_tests(
    map: &serde_yaml_ng::Mapping,
    kind: &str,
    where_prefix: &str,
) -> Result<GradeCheck, LoadError> {
    let text = |key: &str| {
        map.get(serde_yaml_ng::Value::String(key.into()))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let list = |key: &str| -> Vec<String> {
        map.get(serde_yaml_ng::Value::String(key.into()))
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let runner = text("runner");
    require(
        runner == "cargo",
        format!("{where_prefix}: kind=tests runner must be 'cargo', got '{runner}'"),
    )?;
    let tests_ref = text("tests_ref");
    require(
        (7..=40).contains(&tests_ref.len()) && tests_ref.chars().all(|c| c.is_ascii_hexdigit()),
        format!(
            "{where_prefix}: kind=tests requires tests_ref as a 7-to-40 hex sha, got '{tests_ref}'"
        ),
    )?;
    let package = text("package");
    require(
        !package.is_empty(),
        format!("{where_prefix}: kind=tests requires a non-empty package"),
    )?;
    let test_files = list("test_files");
    require(
        !test_files.is_empty(),
        format!("{where_prefix}: kind=tests requires a non-empty test_files list"),
    )?;
    let f2p = list("f2p");
    require(
        !f2p.is_empty(),
        format!("{where_prefix}: kind=tests requires a non-empty f2p list"),
    )?;
    Ok(GradeCheck {
        kind: kind.to_string(),
        command: None,
        expect: 0,
        path: None,
        pattern: None,
        max_tool_calls: None,
        tests: Some(TestsSpec {
            tests_ref,
            test_files,
            package,
            f2p,
            p2p: list("p2p"),
        }),
        rubric: None,
    })
}

/// Warn when every check is exit-only against a trivial no-op command.
pub fn warn_if_decorative(task_id: &str, path: &std::path::Path, checks: &[GradeCheck]) {
    let all_trivial = !checks.is_empty()
        && checks.iter().all(|c| {
            c.kind == "exit"
                && c.command
                    .as_deref()
                    .map(|cmd| {
                        let t = cmd.trim();
                        t == "true" || t == ":" || t.is_empty()
                    })
                    .unwrap_or(false)
        });
    if all_trivial {
        let where_ = path.display().to_string();
        eprintln!(
            "WARNING: bank task '{task_id}' ({where_}): every grade check is a trivial exit-only command ({TRIVIAL_COMMANDS_REPR}); this task always passes and grades nothing task-specific."
        );
    }
}

/// The task files a skill carries beside its SKILL.md: `skills/<name>/evals/*.yaml`
/// next to a bank at `<root>/evals/bank`. Case directories (`evals/<case>/case.yaml`,
/// the `claude plugin eval` layout) sit one level deeper and are not read here.
fn skill_task_paths(bank_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let root = match (bank_dir.file_name(), bank_dir.parent()) {
        (Some(n), Some(evals))
            if n == "bank" && evals.file_name().is_some_and(|e| e == "evals") =>
        {
            evals.parent()
        }
        _ => None,
    };
    let Some(skills) = root.map(|r| r.join("skills")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(skills) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| std::fs::read_dir(e.path().join("evals")).ok())
        .flat_map(|d| d.flatten().map(|f| f.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yaml") && p.is_file())
        .collect()
}

/// How a bank walk treats a task file this binary cannot fully read.
enum Tolerance {
    /// Any refusal fails the walk.
    Strict,
    /// A check kind newer than this binary skips the file and its name joins
    /// the skipped list; every other refusal still fails the walk.
    SkipUnknownCheckKind,
}

fn discover_bank_with(
    bank_dir: &std::path::Path,
    tolerance: Tolerance,
) -> Result<(Vec<TaskSpec>, Vec<String>), BankError> {
    require(
        bank_dir.is_dir(),
        format!("bank directory not found: {}", bank_dir.display()),
    )?;
    let mut tasks: std::collections::BTreeMap<String, TaskSpec> = Default::default();
    let mut skipped: Vec<String> = Vec::new();
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(bank_dir)
        .map_err(|e| {
            BankError(format!(
                "cannot read bank task {}: {}",
                bank_dir.display(),
                e
            ))
        })?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "yaml").unwrap_or(false))
        .collect();
    paths.extend(skill_task_paths(bank_dir));
    paths.sort();
    for yaml_path in paths {
        if yaml_path.file_name().is_some_and(|n| {
            n == std::ffi::OsStr::new(COHORTS_FILENAME)
                || n == std::ffi::OsStr::new(super::failure_types::FILENAME)
        }) {
            continue;
        }
        let task = match load_task_typed(&yaml_path) {
            Ok(task) => task,
            Err(LoadError::UnknownCheckKind(_))
                if matches!(tolerance, Tolerance::SkipUnknownCheckKind) =>
            {
                let name = yaml_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| yaml_path.display().to_string());
                skipped.push(name);
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if tasks.contains_key(&task.id) {
            let first = tasks
                .get(&task.id)
                .map(|t| t.source_path.display().to_string())
                .unwrap_or_default();
            return Err(BankError(format!(
                "duplicate bank task id '{id}': {first} and {second}",
                id = task.id,
                first = first,
                second = yaml_path.display().to_string(),
            )));
        }
        tasks.insert(task.id.clone(), task);
    }
    Ok((tasks.into_values().collect(), skipped))
}

/// Load every *.yaml under bank_dir and every skill-local `skills/*/evals/*.yaml`,
/// sorted by id; duplicate ids refuse.
pub fn discover_bank(bank_dir: &std::path::Path) -> Result<Vec<TaskSpec>, BankError> {
    discover_bank_with(bank_dir, Tolerance::Strict).map(|(tasks, _)| tasks)
}

/// The gate's head-leg read: a bank carrying a check kind newer than this
/// binary still grades its readable tasks. The skipped file names come back
/// so the caller can say what it did not read; malformed YAML, duplicate
/// ids and every other refusal still error the whole call.
pub fn discover_bank_tolerant(
    bank_dir: &std::path::Path,
) -> Result<(Vec<TaskSpec>, Vec<String>), BankError> {
    discover_bank_with(bank_dir, Tolerance::SkipUnknownCheckKind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_task(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn a_task_without_a_grade_refuses_naming_id_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_task(
            dir.path(),
            "t.yaml",
            "id: no-grade\ntier: regression\ngrade: []\ntimeout_minutes: 1\n",
        );
        let err = load_task(&p).unwrap_err();
        assert!(err.0.contains("task 'no-grade'"), "{0}", err.0);
        assert!(
            err.0.contains("(a task with no runnable grade is invalid)"),
            "{0}",
            err.0
        );
    }

    #[test]
    fn a_skill_local_task_loads_beside_the_bank_and_a_duplicate_id_refuses() {
        let root = tempfile::tempdir().unwrap();
        let bank = root.path().join("evals").join("bank");
        let body = |id: &str| {
            format!("id: {id}\ntier: capability\ngrade:\n  - kind: file-exists\n    path: x\ntimeout_minutes: 1\n")
        };
        write_task(&bank, "a.yaml", &body("bank-one"));
        let evals = root.path().join("skills").join("target").join("evals");
        write_task(&evals, "b.yaml", &body("skill-one"));
        write_task(&evals.join("case-dir"), "case.yaml", "name: not-a-task\n");
        let ids: Vec<String> = discover_bank(&bank)
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["bank-one", "skill-one"]);
        write_task(&evals, "c.yaml", &body("bank-one"));
        let err = discover_bank(&bank).unwrap_err();
        assert!(
            err.0.contains("duplicate bank task id 'bank-one'"),
            "{0}",
            err.0
        );
    }

    #[test]
    fn a_tests_task_loads_and_each_missing_field_refuses_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let task = |check: &str, extra: &str| {
            format!(
                "id: wp-1\ntier: capability\ngrade:\n  - kind: tests\n{check}timeout_minutes: 5\n{extra}"
            )
        };
        let check = "    runner: cargo\n    tests_ref: abc1234\n    package: p\n    test_files: [c/src/tests.rs]\n    f2p: [m::a]\n    p2p: [m::b]\n";
        let golden = "golden:\n  source: {kind: fix-pr, ref: abc1234}\n  expected: e\n  rule: r\n  failure_type: wrong-assumption\n  outcome: failed\n";
        let qualified = "qualified: {gold_rev: abc1234}\n";
        let load = |body: String| load_task(&write_task(dir.path(), "t.yaml", &body));

        let spec = load(task(check, &format!("{golden}{qualified}")))
            .unwrap()
            .grade
            .remove(0)
            .tests
            .unwrap();
        assert_eq!(
            (spec.f2p, spec.p2p),
            (vec!["m::a".into()], vec!["m::b".into()])
        );
        for (drop, want) in [
            ("    f2p: [m::a]\n", "non-empty f2p"),
            ("    test_files: [c/src/tests.rs]\n", "test_files"),
            ("    tests_ref: abc1234\n", "7-to-40 hex sha"),
        ] {
            let err = load(task(
                &check.replace(drop, ""),
                &format!("{golden}{qualified}"),
            ))
            .unwrap_err();
            assert!(err.0.contains(want) && err.0.contains("wp-1"), "{}", err.0);
        }
        assert!(load(task(check, qualified))
            .unwrap_err()
            .0
            .contains("golden block"));
        assert!(load(task(check, golden))
            .unwrap_err()
            .0
            .contains("qualified block"));
    }

    #[test]
    fn a_trivial_exit_only_grade_warns_and_loads() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_task(
            dir.path(),
            "d.yaml",
            "id: decoy\ntier: capability\ngrade:\n  - kind: exit\n    command: \"true\"\ntimeout_minutes: 1\n",
        );
        let task = load_task(&p).unwrap();
        assert_eq!(task.grade.len(), 1);
        // The warning prints to stderr; the task still loads (a warning,
        // not an error).
    }

    #[test]
    fn a_tool_budget_without_a_positive_integer_refuses_naming_id_file_and_field() {
        let dir = tempfile::tempdir().unwrap();
        for (name, field) in [
            ("missing", ""),
            ("zero", "    max_tool_calls: 0\n"),
            ("text", "    max_tool_calls: many\n"),
        ] {
            let body = format!(
                "id: budget-{name}\ntier: capability\ngrade:\n  - kind: tool-budget\n{field}"
            );
            let p = write_task(dir.path(), &format!("{name}.yaml"), &body);
            let err = load_task(&p).unwrap_err();
            assert!(
                err.0.contains(&format!("task 'budget-{name}'")),
                "{0}",
                err.0
            );
            assert!(err.0.contains(&format!("{name}.yaml")), "{0}", err.0);
            assert!(err.0.contains("max_tool_calls"), "{0}", err.0);
        }
        let p = write_task(
            dir.path(),
            "ok.yaml",
            "id: budget-ok\ntier: capability\ngrade:\n  - kind: tool-budget\n    max_tool_calls: 40\n",
        );
        assert_eq!(load_task(&p).unwrap().grade[0].max_tool_calls, Some(40));
    }

    #[test]
    fn a_golden_without_a_rule_refuses_naming_id_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let grade = "grade:\n  - kind: file-exists\n    path: x\n";
        let block = |rule: &str| {
            format!(
                "golden:\n  source: {{kind: correction, ref: abc}}\n  expected: e\n{rule}  failure_type: f\n  outcome: failed\n"
            )
        };
        let no_rule = format!("id: g\ntier: capability\n{grade}{}", block(""));
        let p = write_task(dir.path(), "no-rule.yaml", &no_rule);
        let err = load_task(&p).unwrap_err();
        assert!(err.0.contains("task 'g'"), "{0}", err.0);
        assert!(err.0.contains("no-rule.yaml"), "{0}", err.0);
        assert!(err.0.contains("'rule'"), "{0}", err.0);

        let whole = format!("id: g\ntier: capability\n{grade}{}", block("  rule: r\n"));
        let p = write_task(dir.path(), "whole.yaml", &whole);
        let golden = load_task(&p).unwrap().golden.unwrap();
        assert_eq!(
            (golden.rule.as_str(), golden.outcome.as_str()),
            ("r", "failed")
        );
    }

    #[test]
    fn the_repo_bank_holds_the_labelled_goldens() {
        let bank = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/bank");
        let tasks = discover_bank(&bank).unwrap();
        let count = |outcome: &str| {
            tasks
                .iter()
                .filter_map(|t| t.golden.as_ref())
                .filter(|g| g.outcome == outcome)
                .count()
        };
        assert!(count("failed") >= 20, "failed goldens: {}", count("failed"));
        assert!(count("passed") >= 5, "passed goldens: {}", count("passed"));
    }

    #[test]
    fn covers_loads_a_prefix_list_and_refuses_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        let grade = "grade:\n  - kind: file-exists\n    path: x\n";
        let body = |covers: &str| format!("id: c\ntier: regression\n{grade}{covers}");
        let ok = write_task(
            dir.path(),
            "ok.yaml",
            &body("covers: [\"crates/fno-agents/src/backlog/\"]\n"),
        );
        assert_eq!(
            load_task(&ok).unwrap().covers,
            vec!["crates/fno-agents/src/backlog/"]
        );
        for (name, covers) in [
            ("bare", "covers: [\"crates/fno\"]\n"),
            ("scalar", "covers: crates/fno/\n"),
            ("num", "covers: [3]\n"),
        ] {
            let p = write_task(dir.path(), &format!("{name}.yaml"), &body(covers));
            let err = load_task(&p).unwrap_err();
            assert!(err.0.contains("task 'c'"), "{0}", err.0);
            assert!(err.0.contains(&format!("{name}.yaml")), "{0}", err.0);
        }
    }

    #[test]
    fn a_rubric_task_loads_with_criteria_and_tags_and_the_format_refuses_naming_id_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let criteria = "    criteria:\n      - {id: stays-rust, text: the answer names a crates/ path, tag: hurdle}\n      - {id: no-new-python, text: the answer never adds Python to cli/src/fno, tag: principle}\n";
        let body = |check: &str, tier: &str, extra: &str| {
            format!("id: rubric-load\ntier: {tier}\nprompt: answer the question\ngrade:\n  - kind: rubric\n{check}timeout_minutes: 5\n{extra}")
        };
        let ok = write_task(
            dir.path(),
            "ok.yaml",
            &body(
                &format!("    path: decision.md\n{criteria}"),
                "capability",
                "golden:\n  source: {kind: revert, ref: r}\n  expected: e\n  rule: r\n  failure_type: wrong-language\n  outcome: failed\n",
            ),
        );
        let check = load_task(&ok).unwrap().grade.remove(0);
        let rubric = check.rubric.as_ref().unwrap();
        assert!(rubric.path.as_deref() == Some("decision.md") && !rubric.diff);
        let crit = rubric.criteria.as_slice();
        assert_eq!(crit.len(), 2);
        assert_eq!(crit[0].id, "stays-rust");
        assert_eq!(crit[0].tag, RubricTag::Hurdle);
        assert_eq!(crit[1].tag, RubricTag::Principle);

        let load = |name: &str, body: String| load_task(&write_task(dir.path(), name, &body));
        // AC1-EDGE: regression tier refuses, naming id and file. The check
        // must parse first, so the case carries criteria: the tier refusal
        // is task-level and fires after check parsing.
        let err = load(
            "reg.yaml",
            body(
                &format!("    path: decision.md\n{criteria}"),
                "regression",
                "",
            ),
        )
        .unwrap_err();
        assert!(
            err.0.contains("task 'rubric-load'") && err.0.contains("reg.yaml"),
            "{}",
            err.0
        );
        assert!(err.0.contains("requires tier capability"), "{}", err.0);
        // No prompt refuses.
        let err = load(
            "noprompt.yaml",
            format!(
                "id: rubric-load\ntier: capability\ngrade:\n  - kind: rubric\n    path: decision.md\n{criteria}timeout_minutes: 5\n"
            ),
        )
        .unwrap_err();
        assert!(err.0.contains("requires a task 'prompt'"), "{}", err.0);
        // Neither nor both sources refuse.
        for (name, src) in [
            ("nosrc.yaml", ""),
            ("both.yaml", "    path: decision.md\n    diff: true\n"),
        ] {
            let err = load(name, body(src, "capability", "")).unwrap_err();
            assert!(
                err.0.contains("exactly one work-product source"),
                "{}",
                err.0
            );
        }
    }

    #[test]
    fn a_rubric_check_with_bad_criteria_refuses_naming_the_row() {
        let dir = tempfile::tempdir().unwrap();
        let head = "id: rubric-crit\ntier: capability\nprompt: p\ngrade:\n  - kind: rubric\n    path: w.md\n";
        let load = |name: &str, criteria: &str| {
            load_task(&write_task(
                dir.path(),
                name,
                &format!("{head}{criteria}timeout_minutes: 5\n"),
            ))
        };
        let err = load("empty.yaml", "").unwrap_err();
        assert!(err.0.contains("non-empty 'criteria'"), "{}", err.0);
        let err = load(
            "duptag.yaml",
            "    criteria:\n      - {id: a, text: t, tag: bonus}\n",
        )
        .unwrap_err();
        assert!(err.0.contains("tag must be one of"), "{}", err.0);
        let err = load(
            "dup.yaml",
            "    criteria:\n      - {id: a, text: t, tag: hurdle}\n      - {id: a, text: u, tag: hurdle}\n",
        )
        .unwrap_err();
        assert!(err.0.contains("duplicate criterion id 'a'"), "{}", err.0);
    }

    #[test]
    fn discover_skips_cohorts_and_sorts_by_id() {
        let dir = tempfile::tempdir().unwrap();
        write_task(
            dir.path(),
            "b.yaml",
            "id: beta\ntier: regression\ngrade:\n  - kind: file-exists\n    path: x\n",
        );
        write_task(
            dir.path(),
            "a.yaml",
            "id: alpha\ntier: capability\ngrade:\n  - kind: file-exists\n    path: x\n",
        );
        std::fs::write(dir.path().join("cohorts.yaml"), "train: []\n").unwrap();
        let tasks = discover_bank(dir.path()).unwrap();
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "beta"]);
    }

    #[test]
    fn a_newer_check_kind_skips_the_file_in_the_tolerant_read_and_refuses_the_strict_one() {
        let dir = tempfile::tempdir().unwrap();
        let known = "id: known\ntier: capability\ngrade:\n  - kind: file-exists\n    path: x\n";
        write_task(dir.path(), "known.yaml", known);
        write_task(
            dir.path(),
            "newer.yaml",
            "id: newer\ntier: capability\ngrade:\n  - kind: rubric\n",
        );
        let (tasks, skipped) = discover_bank_tolerant(dir.path()).unwrap();
        let ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["known"]);
        assert_eq!(skipped, vec!["newer.yaml"]);
        let err = discover_bank(dir.path()).unwrap_err();
        assert!(err.0.contains("kind must be one of"), "{0}", err.0);
        assert!(err.0.contains("newer.yaml"), "{0}", err.0);
    }

    #[test]
    fn a_malformed_task_still_errors_the_tolerant_read() {
        let dir = tempfile::tempdir().unwrap();
        let known = "id: known\ntier: capability\ngrade:\n  - kind: file-exists\n    path: x\n";
        write_task(dir.path(), "known.yaml", known);
        write_task(dir.path(), "broken.yaml", "id: broken\ngrade: [unclosed\n");
        let err = discover_bank_tolerant(dir.path()).unwrap_err();
        assert!(err.0.contains("malformed YAML"), "{0}", err.0);
        assert!(err.0.contains("broken.yaml"), "{0}", err.0);
    }
}
