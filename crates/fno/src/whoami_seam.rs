//! `fno whoami` and `fno whoami status` with the session receipt appended.
//! Python owns both commands. The seam runs the front door again as a child
//! (the marker env keeps the child on the plain forward), captures its
//! stdout, then adds the `turns` block from `fno-agents intel --receipt`.
//! Any receipt failure prints Python's output unchanged plus an unmeasured
//! line; Python's exit code always passes through.

use serde_json::{Map, Value};
use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Set on the child so its own front door forwards instead of re-entering
/// the seam.
const CHILD_ENV: &str = "FNO_WHOAMI_SEAM_CHILD";
/// The receipt read's bound: whoami runs under the 2s SessionStart hook.
const RECEIPT_BOUND: Duration = Duration::from_secs(1);

/// The argv the seam claims: `whoami` with only its own flags, or
/// `whoami status`. Help and every other leaf keep the plain forward.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    if std::env::var_os(CHILD_ENV).is_some() || args.first()?.to_str()? != "whoami" {
        return None;
    }
    let mut leaf: Option<&str> = None;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].to_str()?;
        match a {
            "--help" | "-h" => return None,
            "--state-file" => i += 1,
            _ if a.starts_with('-') => {}
            _ if leaf.is_none() => leaf = Some(a),
            _ => return None,
        }
        i += 1;
    }
    matches!(leaf, None | Some("status")).then(|| args.to_vec())
}

fn read_receipt(json: bool) -> Result<String, String> {
    let bin = crate::digest_overlay::fno_agents_bin();
    let mut cmd = Command::new(&bin);
    cmd.args(["intel", "--receipt"]);
    if json {
        cmd.arg("--json");
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{} could not run: {e}", bin.display()))?;
    let deadline = Instant::now() + RECEIPT_BOUND;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => return Err(format!("the receipt read exited {status}")),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "the receipt read passed its {}s bound",
                    RECEIPT_BOUND.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("the receipt read failed: {e}")),
        }
    }
    let mut out = String::new();
    child
        .stdout
        .take()
        .ok_or("the receipt read had no stdout")?
        .read_to_string(&mut out)
        .map_err(|e| format!("the receipt output was unreadable: {e}"))?;
    Ok(out)
}

/// Python's JSON object with a `turns` key added, keys sorted as Python's
/// `sort_keys=True` prints them. `None` when Python's output is not an
/// object, so the caller prints it unchanged.
fn merge_json(python: &str, receipt: Result<String, String>) -> Option<String> {
    let Ok(Value::Object(mut obj)) = serde_json::from_str::<Value>(python) else {
        return None;
    };
    let turns = receipt
        .and_then(|r| serde_json::from_str::<Value>(&r).map_err(|e| format!("receipt JSON: {e}")))
        .unwrap_or_else(|why| serde_json::json!({ "basis": format!("unmeasured: {why}") }));
    obj.insert("turns".into(), turns);
    let mut keys: Vec<(String, Value)> = obj.into_iter().collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    serde_json::to_string_pretty(&Value::Object(keys.into_iter().collect::<Map<_, _>>())).ok()
}

/// Python's text with the receipt lines after it.
fn append_text(python: &str, receipt: Result<String, String>) -> String {
    let mut out = python.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    match receipt {
        Ok(lines) => out.push_str(&lines),
        Err(why) => out.push_str(&format!("turns: unmeasured ({why})\n")),
    }
    out
}

pub fn run(args: &[OsString]) -> i32 {
    let json = args.iter().any(|a| a == "--json" || a == "-J");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("fno whoami: the front door cannot find itself: {e}");
            return 2;
        }
    };
    let out = match Command::new(exe)
        .args(args)
        .env(CHILD_ENV, "1")
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            eprintln!("fno whoami: the forwarded command could not run: {e}");
            return 2;
        }
    };
    let code = out.status.code().unwrap_or(1);
    let python = String::from_utf8_lossy(&out.stdout);
    if code != 0 {
        print!("{python}");
        return code;
    }
    let receipt = read_receipt(json);
    if json {
        match merge_json(&python, receipt) {
            Some(merged) => println!("{merged}"),
            None => print!("{python}"),
        }
    } else {
        print!("{}", append_text(&python, receipt));
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_seam_claims_whoami_and_status_only() {
        for claimed in [
            &["whoami"][..],
            &["whoami", "--json"],
            &["whoami", "status", "-J"],
            &["whoami", "--state-file", "x.md", "status"],
        ] {
            assert!(classify(&os(claimed)).is_some(), "{claimed:?}");
        }
        for forwarded in [
            &["whoami", "cost"][..],
            &["whoami", "--help"],
            &["whoami", "status", "--help"],
            &["agents", "top"],
        ] {
            assert!(classify(&os(forwarded)).is_none(), "{forwarded:?}");
        }
    }

    #[test]
    fn the_merge_keeps_python_and_reads_unmeasured_on_failure() {
        // AC5-HP: every Python key survives, sorted, beside `turns`.
        let python = "{\n  \"fleet\": {\"a\": 1},\n  \"walker\": null\n}\n";
        let merged = merge_json(python, Ok(r#"{"last_turn": {}, "denied": {}}"#.into())).unwrap();
        let v: Value = serde_json::from_str(&merged).unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(keys, ["fleet", "turns", "walker"]);
        assert!(v["turns"]["last_turn"].is_object());
        // AC6-ERR: a missing sibling binary leaves Python's lines unchanged.
        let text = append_text("project: fno", Err("fno-agents could not run".into()));
        assert_eq!(
            text,
            "project: fno\nturns: unmeasured (fno-agents could not run)\n"
        );
        let v: Value =
            serde_json::from_str(&merge_json(python, Err("bound".into())).unwrap()).unwrap();
        assert_eq!(v["turns"]["basis"], "unmeasured: bound");
    }
}
