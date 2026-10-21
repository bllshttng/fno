//! The credentialed live gate for the GitHub Issues and Linear backends.
//!
//! Each test drives the public `fno` verbs as subprocesses through one
//! disposable issue: read, list, bind, target start, PR bind, a bad-credential
//! refusal, merge and close, board refresh, and the no-mirror check. It runs
//! only on purpose, against a scratch repo and team the user created
//! (docs/architecture/external-tracker.md, "Live gate"):
//! `cargo test --manifest-path crates/fno-agents/Cargo.toml --test tracker_live -- --ignored --nocapture`
//!
//! The gate reads its own `FNO_LIVE_*` names and maps them onto the
//! `FNO_TRACKER_*` family in child processes only, so a tracker env already in
//! the shell can never point it at a real repo. Tokens never reach argv, a
//! remote URL or the receipt. Every step lands in a JSON receipt under
//! `<state_dir>/live-gates/`; cleanup touches only ids this run created.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const LABEL: &str = "fno-live-gate";
const TITLE_PREFIX: &str = "[fno-live-gate]";

#[test]
#[ignore]
fn github_journey() {
    run_gate(Backend::GitHub);
}

#[test]
#[ignore]
fn linear_journey() {
    run_gate(Backend::Linear);
}

#[derive(Clone, Copy, PartialEq)]
enum Backend {
    GitHub,
    Linear,
}

impl Backend {
    fn name(self) -> &'static str {
        match self {
            Backend::GitHub => "github",
            Backend::Linear => "linear",
        }
    }
    /// The vars this journey needs. Linear also needs the GitHub pair: its PR
    /// leg pushes to the scratch repo.
    fn required(self) -> &'static [&'static str] {
        match self {
            Backend::GitHub => &["FNO_LIVE_GITHUB_REPO", "FNO_LIVE_GITHUB_TOKEN"],
            Backend::Linear => &[
                "FNO_LIVE_LINEAR_API_KEY",
                "FNO_LIVE_LINEAR_TEAM",
                "FNO_LIVE_GITHUB_REPO",
                "FNO_LIVE_GITHUB_TOKEN",
            ],
        }
    }
}

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_default().trim().to_string()
}

fn run_gate(backend: Backend) {
    if let Some(unset) = backend.required().iter().find(|n| var(n).is_empty()) {
        println!("skipped: {unset} unset");
        return;
    }
    let mut gate = Gate::new(backend);
    gate.journey();
    gate.cleanup();
    let receipt = gate.write_receipt();
    println!("receipt: {}", receipt.display());
    assert!(
        gate.failures.is_empty(),
        "live gate failed: {:?}",
        gate.failures
    );
}

struct Gate {
    backend: Backend,
    run_id: String,
    repo: String,
    fno: String,
    state: tempfile::TempDir,
    work: tempfile::TempDir,
    issue: Option<(String, String)>,
    pr_url: Option<String>,
    branch: String,
    steps: Vec<Value>,
    cleanup: Vec<Value>,
    failures: Vec<String>,
    identity: Value,
}

impl Gate {
    fn new(backend: Backend) -> Self {
        let run_id = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        Gate {
            backend,
            branch: format!("fno-live-gate/{run_id}"),
            run_id,
            repo: var("FNO_LIVE_GITHUB_REPO"),
            fno: std::env::var("FNO_LIVE_FNO").unwrap_or_else(|_| "fno".into()),
            state: tempfile::tempdir().unwrap(),
            work: tempfile::tempdir().unwrap(),
            issue: None,
            pr_url: None,
            steps: Vec::new(),
            cleanup: Vec::new(),
            failures: Vec::new(),
            identity: Value::Null,
        }
    }

    fn clone_dir(&self) -> PathBuf {
        self.work.path().join("clone")
    }

    fn id(&self) -> String {
        self.issue.as_ref().map(|i| i.0.clone()).unwrap_or_default()
    }

    /// The child env: a fresh state dir, every inherited tracker var dropped,
    /// then the `FNO_LIVE_*` values mapped onto the names the seam reads.
    fn command(&self, program: &str, backend: &str, gh_token: &str) -> Command {
        let mut cmd = Command::new(program);
        for (k, _) in std::env::vars() {
            if k.starts_with("FNO_TRACKER_") || k == "GH_TOKEN" || k == "GITHUB_TOKEN" {
                cmd.env_remove(k);
            }
        }
        cmd.env("FNO_STATE_DIR", self.state.path())
            .env("FNO_TRACKER_BACKEND", backend)
            .env("FNO_TRACKER_GITHUB_REPO", &self.repo)
            .env("GH_TOKEN", gh_token)
            .env("GIT_ASKPASS", self.work.path().join("askpass.sh"))
            .env("GIT_TERMINAL_PROMPT", "0");
        if self.backend == Backend::Linear {
            cmd.env("FNO_TRACKER_LINEAR_TEAM", var("FNO_LIVE_LINEAR_TEAM"));
            let key = if gh_token == "invalid" {
                "invalid".to_string()
            } else {
                var("FNO_LIVE_LINEAR_API_KEY")
            };
            cmd.env("FNO_TRACKER_LINEAR_API_KEY", key);
        }
        cmd
    }

    /// Run one step and record it. Returns the exit code and stdout.
    fn step(
        &mut self,
        name: &str,
        program: &str,
        args: &[&str],
        cwd: Option<&Path>,
        backend: &str,
        bad_credential: bool,
    ) -> (i32, String) {
        let token = if bad_credential {
            "invalid".to_string()
        } else {
            var("FNO_LIVE_GITHUB_TOKEN")
        };
        let mut cmd = self.command(program, backend, &token);
        cmd.args(args).stdin(Stdio::null());
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let (code, stdout, stderr) = match cmd.output() {
            Ok(o) => (
                o.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            ),
            Err(e) => (-1, String::new(), format!("spawn failed: {e}")),
        };
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().join(" | ");
        self.steps.push(json!({
            "step": name,
            "argv": std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>(),
            "exit": code,
            "stderr_tail": tail,
        }));
        (code, stdout)
    }

    fn fno(&mut self, name: &str, args: &[&str], cwd: Option<&Path>) -> (i32, String) {
        let fno = self.fno.clone();
        let backend = self.backend.name();
        self.step(name, &fno, args, cwd, backend, false)
    }

    fn expect(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        if !ok {
            self.failures.push(format!("{name}: {}", detail.into()));
        }
    }

    fn journey(&mut self) {
        let askpass = self.work.path().join("askpass.sh");
        std::fs::write(
            &askpass,
            "#!/bin/sh\ncase \"$1\" in Username*) echo x-access-token;; *) echo \"$GH_TOKEN\";; esac\n",
        )
        .unwrap();
        set_mode(&askpass, 0o700);
        if let Err(e) = self.preflight() {
            self.expect("preflight", false, e);
            return;
        }
        let title = format!("{TITLE_PREFIX} {} {}", self.backend.name(), self.run_id);
        match self.create(&title) {
            Ok(issue) => self.issue = Some(issue),
            Err(e) => return self.expect("create", false, e),
        }
        let id = self.id();

        let (code, out) = self.fno("read", &["backlog", "get", &id], None);
        let node: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        self.expect("read", code == 0 && node["title"] == json!(title), out);

        let (code, out) = self.fno("list-next", &["backlog", "list", "next", "--json"], None);
        self.expect("list-next", code == 0 && out.contains(&id), out);

        let clone = self.clone_dir();
        let url = format!("https://github.com/{}.git", self.repo);
        let clone_s = clone.to_string_lossy().into_owned();
        let (code, _) = self.step(
            "clone",
            "git",
            &["clone", &url, &clone_s],
            None,
            "graph",
            false,
        );
        if code != 0 {
            return self.expect("clone", false, "git clone of the scratch repo failed");
        }
        let plan = self.work.path().join("plan.md");
        std::fs::write(&plan, "---\nstatus: ready\n---\n# live gate plan\n").unwrap();
        let bind_cwd = format!("cwd={clone_s}");
        let bind_plan = format!("plan_path={}", plan.display());
        let (code, out) = self.fno(
            "bind",
            &["backlog", "update", &id, &bind_cwd, &bind_plan],
            None,
        );
        self.expect("bind", code == 0 && out.contains("bound"), out);
        let (_, out) = self.fno("read-bound", &["backlog", "get", &id], None);
        let node: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        self.expect("read-bound", node["status"] == json!("ready"), out);

        let (code, out) = self.fno(
            "target-start",
            &["do", "target", "start", &id],
            Some(&clone),
        );
        self.expect("target-start", code == 0, out);

        if let Err(e) = self.open_pr() {
            return self.expect("pr", false, e);
        }
        let pr_url = self.pr_url.clone().unwrap_or_default();
        let (code, out) = self.fno(
            "bind-created",
            &["do", "pr", "bind-created", "--url", &pr_url, "--node", &id],
            Some(&clone),
        );
        self.expect("bind-created", code == 0 && out.contains("\"bound\""), out);
        let (_, out) = self.fno("read-in-review", &["backlog", "get", &id], None);
        let node: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
        self.expect("read-in-review", node["status"] == json!("in_review"), out);

        let fno = self.fno.clone();
        let name = self.backend.name();
        let (code, out) = self.step(
            "done-bad-credential",
            &fno,
            &["backlog", "move", "done", &id],
            None,
            name,
            true,
        );
        let still_open = self.remote_state(&id) == Some("open");
        self.expect("done-bad-credential", code != 0 && still_open, out);
        let missing = match self.backend {
            Backend::GitHub => format!("{}#999999", self.repo),
            Backend::Linear => format!("{}-999999", var("FNO_LIVE_LINEAR_TEAM")),
        };
        let (code, out) = self.fno("get-missing", &["backlog", "get", &missing], None);
        self.expect("get-missing", code != 0, out);
        let (code, out) = self.fno("get-colon", &["backlog", "get", "a:b"], None);
        self.expect("get-colon", code != 0, out);

        let repo = self.repo.clone();
        let (code, _) = self.step(
            "pr-merge",
            "gh",
            &[
                "pr",
                "merge",
                &pr_url,
                "--squash",
                "--delete-branch",
                "-R",
                &repo,
            ],
            None,
            "graph",
            false,
        );
        self.expect("pr-merge", code == 0, "gh pr merge failed");
        let number = pr_url.rsplit('/').next().unwrap_or("").to_string();
        self.fno("pr-merged", &["do", "pr", "merged", &number], Some(&clone));
        if self.remote_state(&id) != Some("closed") {
            self.fno("done", &["backlog", "move", "done", &id], None);
        }
        let closed = self.remote_state(&id) == Some("closed");
        self.expect("closed", closed, "the remote issue still reads open");

        let (code, out) = self.fno("board", &["backlog", "board", "open", "--json"], None);
        self.expect("board", code == 0 && !out.contains(&id), out);

        let (code, out) = self.step(
            "no-mirror",
            &fno,
            &["backlog", "get", &id],
            None,
            "graph",
            false,
        );
        self.expect("no-mirror", code != 0, out);
        let sidecar = fno_agents::tracker::sidecar::load(&self.state.path().join("sidecar"), &id)
            .unwrap_or_default();
        self.expect(
            "sidecar-partition",
            sidecar.contains_key("pr_number") && sidecar.contains_key("cwd"),
            format!("{sidecar:?}"),
        );
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let script = root.join("scripts/ci/check-tracker-partition.sh");
        let script = script.to_string_lossy().into_owned();
        let (code, _) = self.step(
            "partition-gate",
            "bash",
            &[&script],
            Some(&root),
            "graph",
            false,
        );
        self.expect(
            "partition-gate",
            code == 0,
            "check-tracker-partition.sh failed",
        );
    }

    fn open_pr(&mut self) -> Result<(), String> {
        let clone = self.clone_dir();
        let branch = self.branch.clone();
        std::fs::write(
            clone.join(format!("live-gate-{}.txt", self.run_id)),
            &self.run_id,
        )
        .map_err(|e| e.to_string())?;
        for (name, args) in [
            ("branch", vec!["checkout", "-B", branch.as_str()]),
            ("add", vec!["add", "-A"]),
            (
                "commit",
                vec![
                    "-c",
                    "user.name=fno live gate",
                    "-c",
                    "user.email=live-gate@invalid",
                    "commit",
                    "-m",
                    "live gate fixture",
                ],
            ),
            ("push", vec!["push", "-u", "origin", branch.as_str()]),
        ] {
            let (code, _) = self.step(name, "git", &args, Some(&clone), "graph", false);
            if code != 0 {
                return Err(format!("git {name} failed"));
            }
        }
        let title = format!("{TITLE_PREFIX} {}", self.run_id);
        let repo = self.repo.clone();
        let (code, out) = self.step(
            "pr-create",
            "gh",
            &[
                "pr",
                "create",
                "-R",
                &repo,
                "--head",
                &branch,
                "--title",
                &title,
                "--body",
                "live gate fixture",
            ],
            Some(&clone),
            "graph",
            false,
        );
        let url = out
            .lines()
            .rev()
            .find(|l| l.contains("/pull/"))
            .map(str::trim);
        match (code, url) {
            (0, Some(url)) => {
                self.pr_url = Some(url.to_string());
                Ok(())
            }
            _ => Err("gh pr create failed".into()),
        }
    }

    /// Refuse before any write when the scratch repo or team holds an open
    /// item this gate did not make, or a gate run younger than 60 minutes.
    fn preflight(&mut self) -> Result<(), String> {
        let open = self.list_open_remote()?;
        let now = chrono::Utc::now();
        for (id, title, created) in open {
            if !title.starts_with(TITLE_PREFIX) {
                return Err(format!(
                    "scratch holds open item {id} the gate did not create; refusing"
                ));
            }
            let age = chrono::DateTime::parse_from_rfc3339(&created)
                .map(|t| now.signed_duration_since(t).num_minutes())
                .unwrap_or(0);
            if age < 60 {
                return Err(format!(
                    "gate item {id} is {age} min old: another run is live"
                ));
            }
            self.close_remote(&id, "leftover");
        }
        Ok(())
    }

    fn cleanup(&mut self) {
        if let Some(url) = self.pr_url.clone() {
            let repo = self.repo.clone();
            let (code, _) = self.step(
                "cleanup-pr",
                "gh",
                &["pr", "close", &url, "-R", &repo],
                None,
                "graph",
                false,
            );
            self.cleanup
                .push(json!({"pr": url, "closed_now": code == 0}));
        }
        if self.clone_dir().exists() {
            let branch = format!(":{}", self.branch);
            let clone = self.clone_dir();
            let (code, _) = self.step(
                "cleanup-branch",
                "git",
                &["push", "origin", &branch],
                Some(&clone),
                "graph",
                false,
            );
            self.cleanup
                .push(json!({"branch": self.branch, "deleted_now": code == 0}));
        }
        if self.issue.is_some() {
            let id = self.id();
            if self.remote_state(&id) != Some("closed") {
                self.close_remote(&id, "cleanup");
            } else {
                self.cleanup
                    .push(json!({"issue": id, "already_closed": true}));
            }
        }
    }

    fn write_receipt(&self) -> PathBuf {
        let cwd = std::env::current_dir().unwrap();
        let dir = fno_agents::agents_config::state_dir(&cwd)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("live-gates");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "tracker-{}-{}.json",
            self.backend.name(),
            self.run_id
        ));
        let (issue_id, issue_url) = self.issue.clone().unwrap_or_default();
        let token = var("FNO_LIVE_GITHUB_TOKEN");
        let scope = match self.backend {
            Backend::GitHub => self.repo.clone(),
            Backend::Linear => var("FNO_LIVE_LINEAR_TEAM"),
        };
        let receipt = json!({
            "backend": self.backend.name(),
            "run_id": self.run_id,
            "scope": scope,
            "issue": {"id": issue_id, "url": issue_url},
            "pr_url": self.pr_url,
            "github_token_kind": token_kind(&token),
            "linear_key_kind": token_kind(&var("FNO_LIVE_LINEAR_API_KEY")),
            "identity": self.identity,
            "steps": self.steps,
            "failures": self.failures,
            "cleanup": self.cleanup,
            "result": if self.failures.is_empty() { "pass" } else { "fail" },
        });
        let text = serde_json::to_string_pretty(&receipt).unwrap();
        for secret in [token, var("FNO_LIVE_LINEAR_API_KEY")] {
            assert!(
                secret.is_empty() || !text.contains(&secret),
                "a secret reached the receipt"
            );
        }
        std::fs::write(&path, text + "\n").unwrap();
        path
    }

    // ---- remote fixture ops: create, read state, list open, close ----

    fn gh_json(&mut self, name: &str, args: &[&str]) -> Result<Value, String> {
        let (code, out) = self.step(name, "gh", args, None, "graph", false);
        if code != 0 {
            return Err(format!("gh {name} failed"));
        }
        serde_json::from_str(&out).map_err(|e| format!("gh {name}: {e}"))
    }

    fn linear(&mut self, name: &str, query: &str, variables: Value) -> Result<Value, String> {
        let header = self.work.path().join("linear-header");
        std::fs::write(
            &header,
            format!("Authorization: {}\n", var("FNO_LIVE_LINEAR_API_KEY")),
        )
        .map_err(|e| e.to_string())?;
        set_mode(&header, 0o600);
        let body = json!({"query": query, "variables": variables}).to_string();
        let header_arg = format!("@{}", header.display());
        let mut child = Command::new("curl")
            .args([
                "-sS",
                "--max-time",
                "30",
                "-H",
                "Content-Type: application/json",
                "-H",
                header_arg.as_str(),
            ])
            .args(["--data-binary", "@-", "https://api.linear.app/graphql"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body.as_bytes())
            .map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let code = out.status.code().unwrap_or(-1);
        self.steps
            .push(json!({"step": name, "argv": ["curl", "linear graphql"], "exit": code}));
        let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("{name}: {e}"))?;
        if v.get("errors").is_some() || code != 0 {
            return Err(format!("{name}: {}", v["errors"]));
        }
        Ok(v["data"].clone())
    }

    fn create(&mut self, title: &str) -> Result<(String, String), String> {
        match self.backend {
            Backend::GitHub => {
                let repo = self.repo.clone();
                let (code, out) = self.step(
                    "create",
                    "gh",
                    &[
                        "issue",
                        "create",
                        "-R",
                        &repo,
                        "--label",
                        LABEL,
                        "--title",
                        title,
                        "--body",
                        "fno live gate fixture",
                    ],
                    None,
                    "graph",
                    false,
                );
                let url = out
                    .lines()
                    .rev()
                    .find(|l| l.contains("/issues/"))
                    .map(str::trim);
                match (code, url) {
                    (0, Some(url)) => {
                        let n = url.rsplit('/').next().unwrap_or("");
                        Ok((format!("{repo}#{n}"), url.to_string()))
                    }
                    _ => Err("gh issue create failed".into()),
                }
            }
            Backend::Linear => {
                let team = self.linear_team()?;
                let data = self.linear(
                    "create",
                    "mutation($t:String!,$team:String!){issueCreate(input:{teamId:$team,title:$t}){issue{identifier url}}}",
                    json!({"t": title, "team": team["id"]}),
                )?;
                let issue = &data["issueCreate"]["issue"];
                Ok((
                    issue["identifier"].as_str().unwrap_or("").to_string(),
                    issue["url"].as_str().unwrap_or("").to_string(),
                ))
            }
        }
    }

    fn linear_team(&mut self) -> Result<Value, String> {
        let data = self.linear(
            "team",
            "query($k:String!){viewer{name} organization{name} teams(filter:{key:{eq:$k}}){nodes{id states{nodes{id type}}}}}",
            json!({"k": var("FNO_LIVE_LINEAR_TEAM")}),
        )?;
        self.identity =
            json!({"viewer": data["viewer"]["name"], "organization": data["organization"]["name"]});
        data["teams"]["nodes"]
            .get(0)
            .cloned()
            .ok_or_else(|| "scratch Linear team not found".to_string())
    }

    fn remote_state(&mut self, id: &str) -> Option<&'static str> {
        let closed = match self.backend {
            Backend::GitHub => {
                let n = id.rsplit('#').next()?.to_string();
                let repo = self.repo.clone();
                let v = self
                    .gh_json(
                        "remote-state",
                        &["issue", "view", &n, "-R", &repo, "--json", "state"],
                    )
                    .ok()?;
                v["state"].as_str()? == "CLOSED"
            }
            Backend::Linear => {
                let v = self
                    .linear(
                        "remote-state",
                        "query($id:String!){issue(id:$id){state{type}}}",
                        json!({"id": id}),
                    )
                    .ok()?;
                matches!(
                    v["issue"]["state"]["type"].as_str()?,
                    "completed" | "canceled"
                )
            }
        };
        Some(if closed { "closed" } else { "open" })
    }

    /// Open items as (id, title, created_at).
    fn list_open_remote(&mut self) -> Result<Vec<(String, String, String)>, String> {
        match self.backend {
            Backend::GitHub => {
                let repo = self.repo.clone();
                let v = self.gh_json(
                    "preflight",
                    &[
                        "issue",
                        "list",
                        "-R",
                        &repo,
                        "--state",
                        "open",
                        "--limit",
                        "100",
                        "--json",
                        "number,title,createdAt,labels",
                    ],
                )?;
                // An issue without the gate label never counts as a gate
                // item, whatever its title says.
                let labeled = |i: &Value| {
                    i["labels"]
                        .as_array()
                        .is_some_and(|ls| ls.iter().any(|l| l["name"] == json!(LABEL)))
                };
                Ok(v.as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| {
                        (
                            format!("{repo}#{}", i["number"]),
                            if labeled(i) {
                                s(&i["title"])
                            } else {
                                String::new()
                            },
                            s(&i["createdAt"]),
                        )
                    })
                    .collect())
            }
            Backend::Linear => {
                let v = self.linear(
                    "preflight",
                    "query($k:String!){issues(first:100,filter:{team:{key:{eq:$k}},state:{type:{nin:[\"completed\",\"canceled\"]}}}){nodes{identifier title createdAt}}}",
                    json!({"k": var("FNO_LIVE_LINEAR_TEAM")}),
                )?;
                Ok(v["issues"]["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|i| (s(&i["identifier"]), s(&i["title"]), s(&i["createdAt"])))
                    .collect())
            }
        }
    }

    /// Close one gate-made item. Only ids this run created, or leftover items
    /// carrying the gate title prefix, ever reach here.
    fn close_remote(&mut self, id: &str, why: &str) {
        let ok = match self.backend {
            Backend::GitHub => {
                let n = id.rsplit('#').next().unwrap_or("").to_string();
                let repo = self.repo.clone();
                let name = format!("close-{why}");
                self.step(
                    &name,
                    "gh",
                    &["issue", "close", &n, "-R", &repo],
                    None,
                    "graph",
                    false,
                )
                .0 == 0
            }
            Backend::Linear => {
                let done = self.linear_team().ok().and_then(|t| {
                    t["states"]["nodes"]
                        .as_array()?
                        .iter()
                        .find(|st| st["type"] == json!("completed"))
                        .map(|st| s(&st["id"]))
                });
                match done {
                    Some(state) => self
                        .linear(
                            &format!("close-{why}"),
                            "mutation($id:String!,$s:String!){issueUpdate(id:$id,input:{stateId:$s}){success}}",
                            json!({"id": id, "s": state}),
                        )
                        .is_ok(),
                    None => false,
                }
            }
        };
        self.cleanup
            .push(json!({"issue": id, "why": why, "closed_now": ok}));
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

/// The token's kind from its public prefix, never its value.
fn token_kind(token: &str) -> &'static str {
    match token {
        "" => "absent",
        t if t.starts_with("github_pat_") => "github fine-grained PAT",
        t if t.starts_with("ghp_") => "github classic PAT",
        t if t.starts_with("lin_api_") => "linear personal API key",
        _ => "unknown",
    }
}

fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}
