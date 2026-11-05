//! The rubric judge: a criterion panel over one work product.
//!
//! The bank stayed deterministic by construction until this kind: `rubric`
//! is the one model path, and the fences are the rules that keep a bank
//! whose verdicts can flip without the tree changing from serving
//! `tier: regression`. A rubric check refuses regression tier at load, its
//! verdicts cache by (work product, criterion, lane, template) so one tree
//! grades the same within a cohort, and a golden carrying one reads
//! unproved until recorded verdicts and a judge-agreement bar hold.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::evals::bank::{RubricCriterion, RubricTag};
use crate::evals::runner::LaneRow;

/// Bumped when the judge prompt wording changes: old cache rows and old
/// recorded verdicts stop matching, so nothing stale reads as fresh.
pub const TEMPLATE_ID: &str = "rubric-criterion-v1";
/// One judge call's bound; a lane slower than this reads a fault.
pub const JUDGE_TIMEOUT_S: u64 = 240;
/// The judge-agreement bar (the APEX-Accounting judge read 97.1 percent and kappa 0.857).
pub const MIN_LABELS: usize = 100;
pub const PASS_AGREEMENT: f64 = 0.90;
pub const PASS_KAPPA: f64 = 0.80;

/// The question one criterion panel answers.
pub struct RubricAsk<'a> {
    pub task_prompt: &'a str,
    pub criterion: &'a RubricCriterion,
    pub work_product: &'a str,
}

/// One judge lane's answer. `fault` names a judge fault (timeout, spawn
/// error, unparseable reply): the cell reads a missing result, never a pass.
pub struct LaneVerdict {
    pub lane: String,
    pub passed: bool,
    pub fault: Option<String>,
}

impl LaneVerdict {
    fn fault(lane: &str, why: String) -> LaneVerdict {
        LaneVerdict {
            lane: lane.to_string(),
            passed: false,
            fault: Some(why),
        }
    }
}

/// The judge panel over one criterion. Implementations: the real lane
/// spawn (production), recorded verdicts (the metric proof), fakes (tests).
pub trait Judge {
    fn panel(&self, ask: &RubricAsk) -> Vec<LaneVerdict>;
}

/// The two-lane spawn judge from `config.evals.rubric.judges`.
pub struct SpawnJudge {
    lanes: Vec<LaneRow>,
    cache_dir: Option<PathBuf>,
}

impl SpawnJudge {
    /// The judges for this tree. `Ok(None)` = unset: rubric checks read
    /// missing results. `Err` = a misconfigured list: the caller prints the
    /// refusal and grades with no judge, so a bad key never fabricates a pass.
    pub fn from_config(cwd: &Path, cache_root: &Path) -> Result<Option<SpawnJudge>, String> {
        let Some(value) =
            crate::agents_config::config_value_deep(cwd, &["evals", "rubric", "judges"])
        else {
            return Ok(None);
        };
        let Some(names) = value.as_array() else {
            return Err("config.evals.rubric.judges must be a list of two lane names".into());
        };
        let names: Vec<String> = names
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        require_two(&names)?;
        let mut lanes = Vec::new();
        for name in &names {
            lanes.push(crate::evals::runner::resolve_lane(name)?);
        }
        let (a, b) = (&lanes[0], &lanes[1]);
        require_two(&[a.name.clone(), b.name.clone()])?;
        if vendor(a) == vendor(b) {
            return Err(format!(
                "config.evals.rubric.judges lanes '{}' and '{}' share one vendor '{}'; self-preference bias needs judges from different vendors",
                a.name,
                b.name,
                vendor(a)
            ));
        }
        Ok(Some(SpawnJudge {
            lanes,
            cache_dir: Some(cache_root.join("evals-rubric-cache")),
        }))
    }
}

fn require_two(names: &[String]) -> Result<(), String> {
    if names.len() != 2 {
        return Err(format!(
            "config.evals.rubric.judges must name exactly two lanes, got {} ({})",
            names.len(),
            names.join(", ")
        ));
    }
    Ok(())
}

/// The vendor a lane speaks for: a codex or zcode harness names it; a claude
/// lane's route is vendor/model, so the vendor is the prefix; a claude lane
/// with no route is Anthropic's own.
fn vendor(lane: &LaneRow) -> String {
    match lane.harness.as_str() {
        "codex" | "zcode" => lane.harness.clone(),
        _ => lane
            .route
            .split('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&lane.harness)
            .to_string(),
    }
}

/// The one prompt one judge call grades: the task, the criterion, the work
/// product. Nothing else, so a judge cannot grade its own preferences.
pub fn judge_prompt(ask: &RubricAsk) -> String {
    let tag = match ask.criterion.tag {
        RubricTag::Hurdle => "hurdle",
        RubricTag::Principle => "principle",
    };
    format!(
        "Grade one criterion of an agent's work product. Read the criterion, then the work product, then answer with one final line: VERDICT: pass or VERDICT: fail.\n\n## The task\n{}\n\n## Criterion ({tag})\n{}\n\n## Work product\n{}",
        ask.task_prompt, ask.criterion.text, ask.work_product
    )
}

/// The last `VERDICT:` line wins; anything else is a fault.
fn parse_reply(text: &str) -> Result<bool, String> {
    let mut verdict: Option<bool> = None;
    for line in text.lines().rev() {
        let line = line.trim();
        if let Some(rest) = line
            .strip_prefix("VERDICT:")
            .map(str::trim)
            .map(str::to_lowercase)
        {
            match rest.as_str() {
                "pass" => verdict = Some(true),
                "fail" => verdict = Some(false),
                _ => {}
            }
            if verdict.is_some() {
                break;
            }
        }
    }
    verdict.ok_or_else(|| {
        format!(
            "no VERDICT: pass/fail line in reply: {}",
            truncate(text.trim(), 120)
        )
    })
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The cache key: what one verdict depends on, hashed. A moved work product,
/// criterion text, lane or template never reads an old verdict.
fn cache_key(lane: &str, ask: &RubricAsk) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(TEMPLATE_ID.as_bytes());
    hasher.update([0]);
    hasher.update(lane.as_bytes());
    hasher.update([0]);
    hasher.update(ask.criterion.text.as_bytes());
    hasher.update([0]);
    hasher.update(ask.work_product.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn cache_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.json"))
}

fn cache_read(dir: &Path, key: &str) -> Option<bool> {
    let text = std::fs::read_to_string(cache_path(dir, key)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    match value.get("verdict").and_then(|v| v.as_str()) {
        Some("pass") => Some(true),
        Some("fail") => Some(false),
        _ => None,
    }
}

fn cache_write(dir: &Path, key: &str, passed: bool) {
    let body = if passed {
        r#"{"verdict":"pass"}"#.to_string()
    } else {
        r#"{"verdict":"fail"}"#.to_string()
    };
    if let Some(parent) = dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(cache_path(dir, key), body);
}

/// The spawn argv one lane answers, harness-shaped: codex resolves its
/// provider from its own config and takes `--model`; a claude-harness lane
/// takes the reader fence (`--tools ""`, no settings, no chrome) with
/// `--route` when the lane carries a vendor route, else `--model`.
fn judge_argv(
    lane: &LaneRow,
    prompt: &str,
    cwd: &Path,
    timeout_secs: u64,
    name: &str,
) -> Vec<String> {
    let mut argv = vec![
        "agents".into(),
        "spawn".into(),
        "--name".into(),
        name.to_string(),
        prompt.to_string(),
        "--harness".into(),
        lane.harness.clone(),
        "--once".into(),
        "--cwd".into(),
        cwd.display().to_string(),
        "--timeout".into(),
        timeout_secs.to_string(),
    ];
    if lane.harness == "codex" || lane.harness == "zcode" {
        argv.push("--model".into());
        argv.push(lane.model.clone());
    } else if lane.route.is_empty() {
        argv.push("--".into());
        argv.push("--model".into());
        argv.push(lane.model.clone());
        for flag in [
            "--setting-sources",
            "--strict-mcp-config",
            "--tools",
            "--disable-slash-commands",
            "--no-chrome",
            "--no-session-persistence",
        ] {
            argv.push(flag.into());
            if flag == "--setting-sources" || flag == "--tools" {
                argv.push(String::new());
            }
        }
    } else {
        argv.push("--route".into());
        argv.push(lane.route.clone());
        argv.push("--".into());
        for flag in [
            "--setting-sources",
            "--strict-mcp-config",
            "--tools",
            "--disable-slash-commands",
            "--no-chrome",
            "--no-session-persistence",
        ] {
            argv.push(flag.into());
            if flag == "--setting-sources" || flag == "--tools" {
                argv.push(String::new());
            }
        }
    }
    argv
}

fn sha8(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

impl Judge for SpawnJudge {
    fn panel(&self, ask: &RubricAsk) -> Vec<LaneVerdict> {
        self.lanes
            .iter()
            .map(|lane| self.judge_lane(lane, ask))
            .collect()
    }
}

impl SpawnJudge {
    fn judge_lane(&self, lane: &LaneRow, ask: &RubricAsk) -> LaneVerdict {
        let key = cache_key(&lane.name, ask);
        if let Some(dir) = &self.cache_dir {
            if let Some(passed) = cache_read(dir, &key) {
                return LaneVerdict {
                    lane: lane.name.clone(),
                    passed,
                    fault: None,
                };
            }
        }
        let prompt = judge_prompt(ask);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let argv = judge_argv(
            lane,
            &prompt,
            &cwd,
            JUDGE_TIMEOUT_S,
            &format!("rubric-{}", sha8(&key)),
        );
        let spawned = crate::blueprint_judge::bounded_fno(&argv, JUDGE_TIMEOUT_S, Path::new("."));
        let (code, stdout, stderr) = match spawned {
            Ok(triple) => triple,
            Err(e) => return LaneVerdict::fault(&lane.name, format!("spawn failed: {e}")),
        };
        if code != 0 {
            return LaneVerdict::fault(
                &lane.name,
                format!("spawn exit {code}: {}", truncate(stderr.trim(), 160)),
            );
        }
        match parse_reply(&stdout) {
            Ok(passed) => {
                if let Some(dir) = &self.cache_dir {
                    cache_write(dir, &key, passed);
                }
                LaneVerdict {
                    lane: lane.name.clone(),
                    passed,
                    fault: None,
                }
            }
            Err(e) => LaneVerdict::fault(&lane.name, e),
        }
    }
}

/// The recorded verdicts one golden's proof reads:
/// `evals/fixtures/metrics/<golden id>.verdicts.jsonl`, written by
/// `report --check-metrics --spend`. A case with no rows, or rows whose
/// hashes no longer match, faults every cell: the proof reads not proved.
pub struct RecordedJudge {
    rows: Vec<RecordedRow>,
}

#[derive(Deserialize)]
struct RecordedRow {
    case: String,
    criterion: String,
    lane: String,
    verdict: String,
    #[serde(default)]
    work_product_sha: String,
    #[serde(default)]
    prompt_sha: String,
}

impl RecordedJudge {
    pub fn load(metrics_dir: &Path, golden_id: &str, case_id: &str) -> RecordedJudge {
        let path = metrics_dir.join(format!("{golden_id}.verdicts.jsonl"));
        let rows: Vec<RecordedRow> = std::fs::read_to_string(&path)
            .map(|text| {
                text.lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .filter(|r: &RecordedRow| r.case == case_id)
                    .collect()
            })
            .unwrap_or_default();
        RecordedJudge { rows }
    }
}

impl Judge for RecordedJudge {
    fn panel(&self, ask: &RubricAsk) -> Vec<LaneVerdict> {
        let matching: Vec<&RecordedRow> = self
            .rows
            .iter()
            .filter(|r| r.criterion == ask.criterion.id)
            .collect();
        if matching.is_empty() {
            return vec![LaneVerdict::fault(
                "(recorded)",
                format!(
                    "no recorded verdict for criterion '{}' (run report --check-metrics --spend)",
                    ask.criterion.id
                ),
            )];
        }
        let prompt_hash = prompt_sha(ask);
        let work_hash = text_sha256(ask.work_product);
        matching
            .iter()
            .map(|row| {
                if !row.prompt_sha.is_empty() && row.prompt_sha != prompt_hash {
                    return LaneVerdict::fault(
                        &row.lane,
                        format!(
                            "recorded verdict for criterion '{}' is stale (prompt hash moved)",
                            ask.criterion.id
                        ),
                    );
                }
                if !row.work_product_sha.is_empty() && row.work_product_sha != work_hash {
                    return LaneVerdict::fault(
                        &row.lane,
                        format!(
                            "recorded verdict for criterion '{}' is stale (work product moved)",
                            ask.criterion.id
                        ),
                    );
                }
                match row.verdict.as_str() {
                    "pass" => LaneVerdict {
                        lane: row.lane.clone(),
                        passed: true,
                        fault: None,
                    },
                    "fail" => LaneVerdict {
                        lane: row.lane.clone(),
                        passed: false,
                        fault: None,
                    },
                    other => LaneVerdict::fault(
                        &row.lane,
                        format!("recorded verdict '{other}' is not pass or fail"),
                    ),
                }
            })
            .collect()
    }
}

/// The hash a recorded row carries: what the judge was asked, minus the
/// work product (that rides its own sha on the row).
pub fn prompt_sha(ask: &RubricAsk) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(TEMPLATE_ID.as_bytes());
    hasher.update([0]);
    hasher.update(ask.task_prompt.as_bytes());
    hasher.update([0]);
    hasher.update(ask.criterion.text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// The plain sha256 of one text, the work-product hash a recorded row
/// carries for its own staleness check.
pub fn text_sha256(text: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// One judge lane tallied against the user labels.
pub struct LaneAgreement {
    pub lane: String,
    pub n: u32,
    pub agreed: u32,
    pub kappa: Option<f64>,
}

impl LaneAgreement {
    /// Raw agreement rate over the scored rows.
    pub fn agreement(&self) -> Option<f64> {
        if self.n == 0 {
            None
        } else {
            Some(self.agreed as f64 / self.n as f64)
        }
    }
}

/// Cohen's kappa over paired (label, verdict) booleans. Degenerate
/// agreement (both sides constant and equal) reads None: kappa is undefined
/// and a constant match proves nothing.
pub fn kappa(pairs: &[(bool, bool)]) -> Option<f64> {
    let n = pairs.len();
    if n == 0 {
        return None;
    }
    let n_f = n as f64;
    let po = pairs.iter().filter(|(a, b)| a == b).count() as f64 / n_f;
    let label_pass = pairs.iter().filter(|(a, _)| *a).count() as f64 / n_f;
    let judge_pass = pairs.iter().filter(|(_, b)| *b).count() as f64 / n_f;
    let pe = label_pass * judge_pass + (1.0 - label_pass) * (1.0 - judge_pass);
    if (1.0 - pe).abs() < 1e-9 {
        return None;
    }
    Some((po - pe) / (1.0 - pe))
}

/// The agreement read over one lane's (label, verdict) pairs.
pub fn lane_agreement(lane: &str, pairs: &[(bool, bool)]) -> LaneAgreement {
    LaneAgreement {
        lane: lane.to_string(),
        n: pairs.len() as u32,
        agreed: pairs.iter().filter(|(a, b)| a == b).count() as u32,
        kappa: kappa(pairs),
    }
}

/// The agreement verdict across every judge lane: proved only when every
/// lane clears the bar (n, agreement and kappa).
pub fn agreement_proved(rows: &[LaneAgreement]) -> bool {
    !rows.is_empty()
        && rows.iter().all(|r| {
            r.n as usize >= MIN_LABELS
                && r.agreement().is_some_and(|a| a >= PASS_AGREEMENT)
                && r.kappa.is_some_and(|k| k >= PASS_KAPPA)
        })
}

/// The per-lane tally over `rows` of (lane, label, verdict).
pub fn tally_lanes(rows: &[(String, bool, bool)]) -> Vec<LaneAgreement> {
    let mut by_lane: BTreeMap<String, Vec<(bool, bool)>> = BTreeMap::new();
    for (lane, label, verdict) in rows {
        by_lane
            .entry(lane.clone())
            .or_default()
            .push((*label, *verdict));
    }
    by_lane
        .into_iter()
        .map(|(lane, pairs)| lane_agreement(&lane, &pairs))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evals::bank::RubricCriterion;

    fn criterion(id: &str, text: &str, hurdle: bool) -> RubricCriterion {
        RubricCriterion {
            id: id.into(),
            text: text.into(),
            tag: if hurdle {
                RubricTag::Hurdle
            } else {
                RubricTag::Principle
            },
        }
    }

    fn work(id: &str, body: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(id);
        std::fs::write(&path, body).unwrap();
        body.to_string()
    }

    #[test]
    fn parse_reply_reads_the_last_verdict_line_and_faults_garbage() {
        assert!(parse_reply("reasoning\nVERDICT: pass").unwrap());
        assert!(!parse_reply("VERDICT: pass\nVERDICT: fail").unwrap());
        assert!(parse_reply("looks fine to me").is_err());
        assert!(parse_reply("VERDICT: unknown").is_err());
    }

    #[test]
    fn cache_round_trips_and_keys_on_every_input() {
        let dir = tempfile::tempdir().unwrap().path().to_path_buf();
        let crit = criterion("c", "one fact", true);
        let ask = RubricAsk {
            task_prompt: "task",
            criterion: &crit,
            work_product: "work",
        };
        let key = cache_key("lane-a", &ask);
        assert!(cache_read(&dir, &key).is_none());
        cache_write(&dir, &key, false);
        assert_eq!(cache_read(&dir, &key), Some(false));
        // A moved input (lane, criterion, work product, template) misses.
        let other_lane = RubricAsk {
            task_prompt: "task",
            criterion: &crit,
            work_product: "work",
        };
        assert_ne!(cache_key("lane-b", &other_lane), key);
        let other_text = criterion("c", "another fact", true);
        assert_ne!(
            cache_key(
                "lane-a",
                &RubricAsk {
                    task_prompt: "task",
                    criterion: &other_text,
                    work_product: "work",
                }
            ),
            key
        );
        assert_ne!(
            cache_key(
                "lane-a",
                &RubricAsk {
                    task_prompt: "task",
                    criterion: &crit,
                    work_product: "other work",
                }
            ),
            key
        );
        assert_eq!(work("unused", ""), "");
    }

    #[test]
    fn recorded_judge_faults_every_cell_when_no_rows_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("g.verdicts.jsonl"),
            r#"{"case":"other","criterion":"c","lane":"l","verdict":"pass"}"#,
        )
        .unwrap();
        let judge = RecordedJudge::load(dir.path(), "g", "mine");
        let crit = criterion("c", "one fact", true);
        let panel = judge.panel(&RubricAsk {
            task_prompt: "t",
            criterion: &crit,
            work_product: "w",
        });
        assert_eq!(panel.len(), 1);
        assert!(panel[0].fault.is_some(), "a missing record must fault");
    }

    #[test]
    fn recorded_judge_reads_pass_fail_and_flags_a_moved_prompt_hash() {
        let dir = tempfile::tempdir().unwrap();
        let crit = criterion("c", "one fact", true);
        let ask = RubricAsk {
            task_prompt: "t",
            criterion: &crit,
            work_product: "w",
        };
        let row = |verdict: &str, prompt_sha: &str| {
            format!(
                r#"{{"case":"m","criterion":"c","lane":"l{verdict}","verdict":"{verdict}","prompt_sha":"{prompt_sha}"}}"#
            )
        };
        std::fs::write(
            dir.path().join("g.verdicts.jsonl"),
            format!(
                "{}\n{}\n",
                row("pass", &prompt_sha(&ask)),
                row("fail", "moved")
            ),
        )
        .unwrap();
        let judge = RecordedJudge::load(dir.path(), "g", "m");
        let panel = judge.panel(&ask);
        assert_eq!(panel.len(), 2);
        let fresh = panel.iter().find(|v| v.lane == "lpass").unwrap();
        let stale = panel.iter().find(|v| v.lane == "lfail").unwrap();
        assert!(fresh.passed && fresh.fault.is_none());
        assert!(stale.fault.as_ref().is_some_and(|f| f.contains("stale")));
    }

    #[test]
    fn kappa_reads_perfect_and_worse_than_chance_and_refuses_degenerate() {
        let n = 10;
        let same: Vec<(bool, bool)> = (0..n).map(|i| (i % 2 == 0, i % 2 == 0)).collect();
        let k_same = kappa(&same).unwrap();
        assert!((k_same - 1.0).abs() < 1e-9);
        let flipped: Vec<(bool, bool)> = (0..n).map(|i| (i % 2 == 0, i % 2 == 1)).collect();
        assert!(kappa(&flipped).unwrap() < 0.0);
        let all_pass: Vec<(bool, bool)> = vec![(true, true); n];
        assert!(
            kappa(&all_pass).is_none(),
            "degenerate agreement is undefined"
        );
    }

    #[test]
    fn the_agreement_bar_holds_only_when_every_lane_clears_it() {
        let rows = |n: u32, agreed: u32, k: f64| {
            vec![LaneAgreement {
                lane: "a".into(),
                n,
                agreed,
                kappa: Some(k),
            }]
        };
        assert!(agreement_proved(&rows(100, 95, 0.85)));
        assert!(
            agreement_proved(&rows(100, 90, 0.85)),
            "the agreement bar is inclusive at 0.90"
        );
        assert!(!agreement_proved(&rows(99, 99, 0.95)), "n under the bar");
        assert!(
            !agreement_proved(&rows(100, 89, 0.85)),
            "agreement under the bar fails"
        );
        assert!(!agreement_proved(&rows(100, 95, 0.79)));
        assert!(!agreement_proved(&[]), "no judge reads unproved");
    }

    #[test]
    fn tally_lanes_splits_rows_by_lane_and_the_vendor_refusal_fires() {
        let rows = vec![
            ("a".to_string(), true, true),
            ("a".to_string(), true, false),
            ("b".to_string(), false, false),
        ];
        let tallies = tally_lanes(&rows);
        assert_eq!(tallies.len(), 2);
        let a = tallies.iter().find(|t| t.lane == "a").unwrap();
        assert_eq!((a.n, a.agreed), (2, 1));
        let lane = LaneRow {
            name: "x".into(),
            harness: "claude".into(),
            route: "zai/glm".into(),
            account: String::new(),
            model: "glm".into(),
            effort: String::new(),
        };
        assert_eq!(vendor(&lane), "zai");
        let plain = LaneRow {
            name: "plain".into(),
            harness: "claude".into(),
            route: String::new(),
            account: String::new(),
            model: "m".into(),
            effort: String::new(),
        };
        assert_eq!(vendor(&plain), "claude");
        let codex = LaneRow {
            name: "luna".into(),
            harness: "codex".into(),
            route: "gpt-6-luna".into(),
            account: String::new(),
            model: "gpt-6-luna".into(),
            effort: String::new(),
        };
        assert_eq!(vendor(&codex), "codex");
    }

    #[test]
    fn judge_argv_is_harness_shaped() {
        let zai = LaneRow {
            name: "zai-flash".into(),
            harness: "claude".into(),
            route: "zai/glm-5.3-flash[1m]".into(),
            account: String::new(),
            model: "glm-5.3-flash[1m]".into(),
            effort: String::new(),
        };
        let argv = judge_argv(&zai, "prompt here", Path::new("."), 240, "rubric-x");
        assert!(argv.contains(&"--route".into()) && argv.contains(&"zai/glm-5.3-flash[1m]".into()));
        assert!(
            argv.contains(&"--".into()),
            "routed claude judges take the reader fence"
        );
        let anthropic = LaneRow {
            name: "haiku".into(),
            harness: "claude".into(),
            route: String::new(),
            account: String::new(),
            model: "claude-haiku".into(),
            effort: String::new(),
        };
        let argv = judge_argv(&anthropic, "p", Path::new("."), 240, "rubric-x");
        let fence = argv.iter().position(|a| a == "--").unwrap();
        assert!(argv[fence + 1..].contains(&"--tools".into()));
        let codex = LaneRow {
            name: "luna".into(),
            harness: "codex".into(),
            route: "gpt-6-luna".into(),
            account: String::new(),
            model: "gpt-6-luna".into(),
            effort: String::new(),
        };
        let argv = judge_argv(&codex, "p", Path::new("."), 240, "rubric-x");
        assert!(!argv.contains(&"--route".into()));
        assert!(argv
            .windows(2)
            .any(|w| w[0] == "--model" && w[1] == "gpt-6-luna"));
        assert!(!argv.contains(&"--".into()), "codex takes no reader fence");
    }
}
