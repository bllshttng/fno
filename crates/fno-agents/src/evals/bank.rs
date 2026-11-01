//! The eval bank: task format + loader + load-time discipline
//! (the deleted Python leg's bank.py, ported per the dual-implementation
//! protocol). A bank task is one evals/bank/<id>.yaml file; the two
//! disciplines: a task without a runnable grade is rejected naming the id
//! and file, and an all-trivial grade warns so a task cannot silently
//! always-pass. The goldens pin the bytes.

pub const VALID_TIERS_REPR: &str = "('capability', 'regression')";
pub const VALID_CHECK_KINDS_REPR: &str = "('exit', 'file-exists', 'grep', 'tool-budget')";

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
    pub golden: Option<Golden>,
    pub source_path: std::path::PathBuf,
}

/// The load-time discipline refusals name the id and file.
#[derive(Debug)]
pub struct BankError(pub String);

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
    let raw: serde_yaml_ng::Value = match std::fs::read(path) {
        Ok(bytes) => serde_yaml_ng::from_slice(&bytes).map_err(|e| {
            BankError(format!(
                "malformed YAML in bank task {}: {}",
                path.display(),
                e
            ))
        })?,
        Err(e) => {
            return Err(BankError(format!(
                "cannot read bank task {}: {}",
                path.display(),
                e
            )))
        }
    };
    let Some(map) = raw.as_mapping() else {
        return Err(BankError(format!(
            "bank task {}: top level must be a mapping",
            path.display()
        )));
    };
    let id_v = map.get(serde_yaml_ng::Value::String("id".into()));
    let Some(id) = id_v.and_then(|v| v.as_str()) else {
        return Err(BankError(format!(
            "bank task {}: missing 'id'",
            path.display()
        )));
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
        return Err(BankError(format!("task '{id}' ({where_}): 'grade' must be a non-empty list of mechanical checks (a task with no runnable grade is invalid)")));
    };
    if grade_raw.is_empty() {
        return Err(BankError(format!("task '{id}' ({where_}): 'grade' must be a non-empty list of mechanical checks (a task with no runnable grade is invalid)")));
    };
    let mut checks: Vec<GradeCheck> = Vec::new();
    for (i, item) in grade_raw.iter().enumerate() {
        checks.push(parse_check(item, &id, path, i)?);
    }
    warn_if_decorative(&id, path, &checks);
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
    let golden = match map.get(serde_yaml_ng::Value::String("golden".into())) {
        Some(raw) => Some(parse_golden(raw, &id, path)?),
        None => None,
    };
    Ok(TaskSpec {
        id,
        golden,
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
) -> Result<GradeCheck, BankError> {
    let where_ = path.display().to_string();
    let where_prefix = format!("task '{task_id}' ({where_}) grade[{index}]");
    let Some(map) = raw.as_mapping() else {
        return Err(BankError(format!(
            "{where_prefix}: each grade check must be a mapping"
        )));
    };
    let kind = map.get(serde_yaml_ng::Value::String("kind".into()));
    let kind = kind.and_then(|v| v.as_str()).unwrap_or("");
    require(
        kind == "exit" || kind == "file-exists" || kind == "grep" || kind == "tool-budget",
        format!("{where_prefix}: kind must be one of {VALID_CHECK_KINDS_REPR}, got '{kind}'"),
    )?;
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
        });
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

/// Load every *.yaml under bank_dir, sorted by id; duplicate ids refuse.
pub fn discover_bank(bank_dir: &std::path::Path) -> Result<Vec<TaskSpec>, BankError> {
    require(
        bank_dir.is_dir(),
        format!("bank directory not found: {}", bank_dir.display()),
    )?;
    let mut tasks: std::collections::BTreeMap<String, TaskSpec> = Default::default();
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
    paths.sort();
    for yaml_path in paths {
        if yaml_path
            .file_name()
            .map(|n| n == std::ffi::OsStr::new(COHORTS_FILENAME))
            .unwrap_or(false)
        {
            continue;
        }
        let task = load_task(&yaml_path)?;
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
    Ok(tasks.into_values().collect())
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
}
