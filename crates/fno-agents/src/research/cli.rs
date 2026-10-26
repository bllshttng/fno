//! `fno do research "X"`: retrieve + store, then ship the doc deliverable.
//!
//! Exit codes: 0 shipped (or stored with `--no-deliver`), 1 a local
//! filesystem failure before anything was stored, 2 usage or a too-thin
//! topic, 3 the ddgs backbone is unavailable, 4 the topic is claimed by
//! another writer, 5 `config.research.output_dir` is unset, 6 the
//! deliverable write failed.

use std::path::PathBuf;

use super::deliverable::{self, DeliverError, OUTPUT_DIR_UNSET};
use super::engine::{self, RoundError, DEFAULT_MAX_RESULTS, NOTE_CLAIMED, NOTE_NO_SOURCES};
use super::pyfmt;

const USAGE: &str = "Usage: fno do research [OPTIONS] TOPIC\n\
                     Try 'fno do research --help' for help.";

const HELP: &str = "\
Usage: fno do research [OPTIONS] TOPIC

  Retrieve sources for TOPIC, store them, and ship a cited brief.

  Searches via the ddgs backbone, self-fetches each result, writes one row per
  source to ~/.fno/notes/research/<slug>.sources.jsonl, then (unless
  --no-deliver) ships <slug>.md + <slug>.sources.jsonl to
  config.research.output_dir and reports DoneAdvisory.

Arguments:
  TOPIC  Topic to research, e.g. \"CA CCLD financials\".  [required]

Options:
  -m, --max-results INTEGER  Max sources to retrieve this round.  [default: 10]
  --no-claim                 Skip the per-topic single-writer claim
                             (testing/CI).
  --deliver / --no-deliver   Ship the doc deliverable to
                             config.research.output_dir (default on).
                             [default: deliver]
  --stopped TEXT             Why the investigation stopped, stamped in the
                             brief (\"declared\" or \"cap N\").  [default:
                             declared]
  --node TEXT                Graph node this deliverable finishes; binds the
                             shipped brief as its plan_path (best-effort).
                             Falls back to $FNO_NODE when omitted.
  --help                     Show this message and exit.
";

#[derive(Debug)]
struct Args {
    topic: String,
    max_results: i64,
    no_claim: bool,
    deliver: bool,
    stopped: String,
    node: Option<String>,
    help: bool,
}

/// `int(text)` as click's INTEGER type reads it.
fn py_int(text: &str) -> Option<i64> {
    let t = pyfmt::strip(text);
    let (neg, digits) = match t.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    let n = clean.parse::<i64>().ok()?;
    Some(if neg { -n } else { n })
}

fn parse(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        topic: String::new(),
        max_results: DEFAULT_MAX_RESULTS,
        no_claim: false,
        deliver: true,
        stopped: "declared".to_string(),
        node: None,
        help: false,
    };
    let mut positional: Vec<String> = Vec::new();
    let mut it = argv.iter();
    let mut options_done = false;
    while let Some(arg) = it.next() {
        if options_done || arg == "-" || !arg.starts_with('-') {
            positional.push(arg.clone());
            continue;
        }
        if arg == "--" {
            options_done = true;
            continue;
        }
        let (name, inline): (String, Option<String>) = if let Some(rest) = arg.strip_prefix("--") {
            match rest.split_once('=') {
                Some((n, v)) => (format!("--{n}"), Some(v.to_string())),
                None => (arg.clone(), None),
            }
        } else if arg.len() > 2 && arg.starts_with("-m") {
            ("-m".to_string(), Some(arg[2..].to_string()))
        } else {
            (arg.clone(), None)
        };
        let takes_value = matches!(
            name.as_str(),
            "--max-results" | "-m" | "--stopped" | "--node"
        );
        let is_flag = matches!(
            name.as_str(),
            "--no-claim" | "--deliver" | "--no-deliver" | "--help"
        );
        if !takes_value && !is_flag {
            return Err(format!("No such option: {name}"));
        }
        if is_flag {
            if inline.is_some() {
                return Err(format!("Option '{name}' does not take a value."));
            }
            match name.as_str() {
                "--no-claim" => a.no_claim = true,
                "--deliver" => a.deliver = true,
                "--no-deliver" => a.deliver = false,
                _ => a.help = true,
            }
            continue;
        }
        let value = match inline {
            Some(v) => v,
            None => match it.next() {
                Some(v) => v.clone(),
                None => return Err(format!("Option '{name}' requires an argument.")),
            },
        };
        match name.as_str() {
            "--max-results" | "-m" => match py_int(&value) {
                Some(n) => a.max_results = n,
                None => {
                    return Err(format!(
                        "Invalid value for '--max-results' / '-m': {} is not a valid integer.",
                        pyfmt::repr(&value)
                    ));
                }
            },
            "--stopped" => a.stopped = value,
            _ => a.node = Some(value),
        }
    }
    if a.help {
        return Ok(a);
    }
    let mut positional = positional.into_iter();
    match positional.next() {
        Some(topic) => a.topic = topic,
        None => return Err("Missing argument 'TOPIC'.".to_string()),
    }
    let extra: Vec<String> = positional.collect();
    if !extra.is_empty() {
        let noun = if extra.len() == 1 {
            "argument"
        } else {
            "arguments"
        };
        return Err(format!("Got unexpected extra {noun} ({})", extra.join(" ")));
    }
    Ok(a)
}

/// The project journal: the `FNO_EVENTS_PATH` pin, else the space journal
/// of `$FNO_REPO_ROOT`, else of the cwd's repository.
fn events_path() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_EVENTS_PATH").filter(|v| !v.is_empty()) {
        return PathBuf::from(pin);
    }
    let anchor = std::env::var_os("FNO_REPO_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    crate::paths::events_path(&anchor)
}

fn resolved_node(flag: Option<&str>) -> Option<String> {
    let flag = pyfmt::strip(flag.unwrap_or(""));
    if !flag.is_empty() {
        return Some(flag.to_string());
    }
    let env = std::env::var("FNO_NODE").unwrap_or_default();
    let env = pyfmt::strip(&env);
    (!env.is_empty()).then(|| env.to_string())
}

pub fn run(argv: &[String]) -> i32 {
    let args = match parse(argv) {
        Ok(a) => a,
        Err(message) => {
            eprintln!("{USAGE}\n\nError: {message}");
            return 2;
        }
    };
    if args.help {
        print!("{HELP}");
        return 0;
    }

    let result = match engine::run_round(&args.topic, args.max_results, !args.no_claim) {
        Ok(r) => r,
        Err(RoundError::EmptyQuery(m)) => {
            eprintln!("{m}");
            return 2;
        }
        Err(RoundError::DdgsUnavailable(m)) => {
            eprintln!("{m}");
            return 3;
        }
        Err(RoundError::Io(m)) => {
            eprintln!("research: {m}");
            return 1;
        }
    };

    if result.note == NOTE_CLAIMED {
        eprintln!(
            "research: {} ({}); nothing written.",
            result.note, result.slug
        );
        return 4;
    }
    let sources_path = result.sources_path.display().to_string();
    if result.note == NOTE_NO_SOURCES {
        println!(
            "research: no sources found for \"{}\" -> {sources_path}",
            result.topic
        );
    } else {
        println!(
            "research: \"{}\" -> {sources_path} ({} found, {} verified, {} failed)",
            result.topic, result.found, result.verified, result.failed
        );
    }

    if !args.deliver {
        return 0;
    }

    let node = resolved_node(args.node.as_deref());
    let output_dir = deliverable::configured_output_dir();
    let delivered = match deliverable::deliver(
        &result.topic,
        &result.sources_path,
        &args.stopped,
        output_dir.as_deref(),
        node.as_deref(),
    ) {
        Ok(d) => d,
        Err(DeliverError::OutputDirUnset) => {
            eprintln!("{OUTPUT_DIR_UNSET}");
            return 5;
        }
        Err(DeliverError::Io(m)) => {
            eprintln!("research: failed to write deliverable: {m}");
            return 6;
        }
    };

    deliverable::emit_done_advisory(&events_path(), &delivered.slug);
    println!(
        "research: shipped {} (+{}.sources.jsonl, {} cited) -> DoneAdvisory",
        delivered.brief_path.display(),
        delivered.slug,
        delivered.verified
    );
    if let Some(warning) = &delivered.bind_warning {
        eprintln!("research: {warning}");
    }
    0
}
