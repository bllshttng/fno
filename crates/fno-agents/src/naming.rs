//! The x-aaaa worker-name vocabulary owner, ported from Python `naming.py`
//! (team ruling on laws d-4b39ad4c / d-52ae01cb: the tree budget ports the
//! touched command to crates/ rather than raising the allowance).
//!
//! One data file stays the contract: the tables are read from the crate-local
//! `naming-codes.yaml` at compile time (the Python copy is deleted; the
//! Python side reads the tables back through `name-codes`), so Python and
//! Rust cannot drift on the codes - only on behavior, which the ported tests pin.

use regex::Regex;
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::OnceLock;

/// The daemon's public agent-name contract.
pub const MAX_LEN: usize = 64;
/// Per-component cap for human-readable text.
pub const SLUG_CAP: usize = 30;
/// Dispatch-form slug cap: a worker name must stay readable in a
/// narrow terminal, so the human slug gives up more room to the model tag.
pub const DISPATCH_SLUG_CAP: usize = 12;

const CODES_YAML: &str = include_str!("naming-codes.yaml");

#[derive(Deserialize)]
struct RawCodes {
    sources: Vec<String>,
    commands: Vec<String>,
    word_codes: std::collections::HashMap<String, String>,
    model_codes: std::collections::HashMap<String, String>,
    provenance: Vec<ProvenanceRow>,
}

#[derive(Deserialize)]
struct ProvenanceRow {
    site: String,
    source: String,
    command: String,
}

pub struct Codes {
    pub sources: HashSet<String>,
    pub commands: HashSet<String>,
    pub word_codes: std::collections::HashMap<String, String>,
    pub model_codes: std::collections::HashMap<String, String>,
    pub provenance: Vec<(String, String, String)>,
}

fn codes() -> &'static Codes {
    static CODES: OnceLock<Codes> = OnceLock::new();
    CODES.get_or_init(|| {
        let raw: RawCodes = serde_yaml_ng::from_str(CODES_YAML).expect("naming-codes.yaml parses");
        Codes {
            sources: raw.sources.into_iter().collect(),
            commands: raw.commands.into_iter().collect(),
            word_codes: raw.word_codes,
            model_codes: raw.model_codes,
            provenance: raw
                .provenance
                .into_iter()
                .map(|r| (r.site, r.source, r.command))
                .collect(),
        }
    })
}

pub fn dispatch_sources() -> &'static HashSet<String> {
    &codes().sources
}

pub fn dispatch_commands() -> &'static HashSet<String> {
    &codes().commands
}

pub fn provenance_rows() -> &'static [(String, String, String)] {
    &codes().provenance
}

/// The vocabulary error: `exit` is 3 for a naming refusal, 2 for bridge usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameError {
    pub message: String,
    pub exit: i32,
}

impl NameError {
    fn refused(message: impl Into<String>) -> Self {
        NameError {
            message: message.into(),
            exit: 3,
        }
    }
    fn usage(message: impl Into<String>) -> Self {
        NameError {
            message: message.into(),
            exit: 2,
        }
    }
}

impl std::fmt::Display for NameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Normalize free text to a name-safe tail, byte-for-byte with the shell.
pub fn slug_component(raw: Option<&str>, cap: usize) -> String {
    let Some(raw) = raw else { return String::new() };
    if raw.is_empty() {
        return String::new();
    }
    let lowered: String = raw
        .chars()
        .map(|c| {
            if c == '_' {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let mut s = String::with_capacity(lowered.len());
    let mut prev_dash = false;
    for c in lowered.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            let dash = c == '-';
            if dash && (s.is_empty() || prev_dash) {
                continue;
            }
            s.push(c);
            prev_dash = dash;
        } else {
            if !s.is_empty() && !prev_dash {
                s.push('-');
                prev_dash = true;
            }
        }
    }
    while s.ends_with('-') {
        s.pop();
    }
    s.truncate(cap);
    while s.ends_with('-') {
        s.pop();
    }
    s
}

/// The short per-model name tag: a known key in `model_codes`
/// matched longest-first as a substring of the lowered model, else the first
/// 8 alphanumeric characters. `None` for a blank model.
pub fn model_code_for(model: Option<&str>) -> Option<String> {
    let m = model?.trim();
    if m.is_empty() {
        return None;
    }
    let lowered = m.to_lowercase();
    let mut keys: Vec<(&String, &String)> = codes().model_codes.iter().collect();
    keys.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.0.cmp(b.0)));
    for (k, v) in keys {
        if lowered.contains(k.as_str()) {
            return Some(v.clone());
        }
    }
    let squeezed: String = lowered
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect();
    let mut tag: String = squeezed.chars().take(8).collect();
    if tag.is_empty() {
        tag.push('m');
    }
    Some(tag)
}

/// A node-shaped identity (``) emits its bare hex: every row in a
/// dispatch shares the prefix, so the name spends those bytes on the model
/// tag instead.
fn node_hex(identity: &str) -> Option<&str> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9]*-([0-9a-f]+)$").unwrap());
    re.captures(identity)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str())
}

/// Truncate a sanitized slug at the last hyphen at or under `cap`; a
/// hyphenless slug truncates hard.
fn cut_slug(raw: Option<&str>, cap: usize) -> String {
    let full = slug_component(raw, usize::MAX);
    if full.chars().count() <= cap {
        return full;
    }
    let taken: String = full.chars().take(cap).collect();
    match taken.rfind('-') {
        Some(i) if i > 0 => taken[..i].trim_end_matches('-').to_string(),
        _ => taken.trim_end_matches('-').to_string(),
    }
}

/// Build `<prefix>-<node_hex>[-<qualifier>][-<slug>][-<model>][-<discriminator>]`.
/// Required identity never shaves; only the human slug gives way.
pub fn agent_name(
    prefix: &str,
    node_id: &str,
    slug: Option<&str>,
    qualifier: Option<&str>,
    discriminator: Option<&str>,
    model: Option<&str>,
) -> Result<String, NameError> {
    let prefix = prefix.trim();
    let node_id = node_id.trim();
    let qualifier = qualifier.unwrap_or("").trim();
    let model_code = model_code_for(model).unwrap_or_default();
    let disc = slug_component(discriminator, SLUG_CAP);

    let node_disp = node_hex(node_id).unwrap_or(node_id);
    let required_parts: Vec<&str> = [
        prefix,
        node_disp,
        qualifier,
        model_code.as_str(),
        disc.as_str(),
    ]
    .iter()
    .filter(|p| !p.is_empty())
    .copied()
    .collect();
    if required_parts.is_empty() {
        return Err(NameError::refused(
            "agent name needs at least a prefix or a node id",
        ));
    }
    let required = required_parts.join("-");
    if required.len() > MAX_LEN {
        // Single-quote the components, never Debug's double quotes: the shell
        // dispatchers relay this message into a `reason="..."` field whose
        // grammar a double quote would break.
        let mut detail = format!(
            "required agent-name identity is {} chars, over the {}-char runtime limit: prefix='{}' node='{}'",
            required.len(), MAX_LEN, prefix, node_id
        );
        if !qualifier.is_empty() {
            detail.push_str(&format!(" qualifier='{}'", qualifier));
        }
        if !model_code.is_empty() {
            detail.push_str(&format!(" model='{}'", model_code));
        }
        if !disc.is_empty() {
            detail.push_str(&format!(" discriminator='{}'", disc));
        }
        return Err(NameError::refused(detail));
    }

    let mut human = cut_slug(slug, SLUG_CAP);
    if !human.is_empty() {
        let avail = MAX_LEN.saturating_sub(required.len() + 1);
        human = human.chars().take(avail).collect();
        while human.ends_with('-') {
            human.pop();
        }
    }

    let name = [
        prefix,
        node_disp,
        qualifier,
        human.as_str(),
        model_code.as_str(),
        disc.as_str(),
    ]
    .iter()
    .filter(|p| !p.is_empty())
    .copied()
    .collect::<Vec<&str>>()
    .join("-");
    let ok = name.len() <= MAX_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok {
        return Err(NameError::refused(format!(
            "generated agent name '{}' violates the runtime contract [A-Za-z0-9_-]{{1,{}}} (node='{}')",
            name, MAX_LEN, node_id
        )));
    }
    Ok(name)
}

/// Whether `command` names a footnote work command (a `word_codes` key): the same
/// vocabulary the name mint resolves.
pub fn is_word_code_command(command: &str) -> bool {
    codes().word_codes.contains_key(command)
}

/// The word-code keys, sorted: the accepted-command vocabulary the write gate
/// names in its refusals.
pub fn word_code_words() -> Vec<String> {
    let mut words: Vec<String> = codes().word_codes.keys().cloned().collect();
    words.sort();
    words
}

/// The command code for a work-command word (`/target`, `$fno:blueprint`, ...).
/// Unknown words raise: nothing defaults to `t`.
pub fn command_code_for(word: Option<&str>) -> Result<String, NameError> {
    let w = word.unwrap_or("").trim();
    let v = match crate::provider::parse_command_token(w) {
        Some((command, _)) => command,
        None if w.is_empty() => "target",
        None => w,
    };
    codes().word_codes.get(v).cloned().ok_or_else(|| {
        NameError::refused(format!("unknown dispatch command '{}'", word.unwrap_or("")))
    })
}

/// Build `[<source>-]<command>-<hex>[-<qualifier>][-<slug>][-<model>][-<disc>]`.
/// `source` None is the attended manual form; unknown codes raise rather than
/// fabricating provenance. The dispatch form caps the human slug at
/// [`DISPATCH_SLUG_CAP`] and carries the model tag.
pub fn dispatch_agent_name(
    source: Option<&str>,
    command: &str,
    identity: &str,
    slug: Option<&str>,
    qualifier: Option<&str>,
    discriminator: Option<&str>,
    model: Option<&str>,
) -> Result<String, NameError> {
    let v = command.trim();
    if !dispatch_commands().contains(v) {
        return Err(NameError::refused(format!(
            "unknown dispatch command '{}'",
            command
        )));
    }
    if let Some(s) = source {
        let s = s.trim();
        if !dispatch_sources().contains(s) {
            return Err(NameError::refused(format!(
                "unknown dispatch source '{}'",
                source.unwrap_or("")
            )));
        }
    }
    let prefix = match source {
        None => v.to_string(),
        Some(s) => format!("{}-{}", s.trim(), v),
    };
    let slug = cut_slug(slug, DISPATCH_SLUG_CAP);
    agent_name(
        &prefix,
        identity,
        Some(&slug),
        qualifier,
        discriminator,
        model,
    )
}

/// The `fno agents name` assembly: `--command`/`--source` select the dispatch
/// form; a positional prefix alone is the legacy form. `prefix`/`node_id` are
/// the CLI positionals AFTER the one-positional-binds-the-node rule.
pub fn bridge_name(
    prefix: Option<&str>,
    node_id: &str,
    slug: Option<&str>,
    qualifier: Option<&str>,
    discriminator: Option<&str>,
    source: Option<&str>,
    command: Option<&str>,
    model: Option<&str>,
) -> Result<String, NameError> {
    if command.is_some() || source.is_some() {
        let p = prefix.unwrap_or("");
        if !p.is_empty() {
            return Err(NameError::usage(
                "pass the legacy prefix form or --source/--command, not both",
            ));
        }
        let Some(v) = command.filter(|v| !v.trim().is_empty()) else {
            return Err(NameError::usage("--source requires --command"));
        };
        let source = source.filter(|s| !s.trim().is_empty());
        let code = if dispatch_commands().contains(v.trim()) {
            v.trim().to_string()
        } else {
            command_code_for(Some(v))?
        };
        return dispatch_agent_name(
            source,
            &code,
            node_id,
            slug,
            qualifier,
            discriminator,
            model,
        );
    }
    match prefix.filter(|p| !p.trim().is_empty()) {
        Some(p) => agent_name(p, node_id, slug, qualifier, discriminator, model),
        None => Err(NameError::usage("a prefix or --command is required")),
    }
}

/// A parsed canonical name. `source` is None for the manual form; `node` is
/// the graph node id when the identity is node-shaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub name: String,
    pub source: Option<String>,
    pub command: String,
    pub node: Option<String>,
    pub tail: String,
}

fn node_shape() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(?:([a-z][a-z0-9]*-)?([0-9a-f]+))(?:-(.*))?$").unwrap())
}

/// Parse `[<source>-]<command>-<identity>`, else None. Positional grammar: the
/// first token is a source only when the second is a command, so a node prefix
/// colliding with a code cannot misread. Pre-cutover `target-`/`think-`
/// names resolve through the legacy read window.
pub fn parse_dispatch_agent_name(name: Option<&str>) -> Option<Parsed> {
    let name = name?;
    if name.is_empty() {
        return None;
    }
    let tokens: Vec<&str> = name.split('-').collect();
    let (source, command, rest): (Option<&str>, &str, &[&str]);
    if tokens.len() >= 2
        && dispatch_sources().contains(tokens[0])
        && dispatch_commands().contains(tokens[1])
    {
        source = Some(tokens[0]);
        command = tokens[1];
        rest = &tokens[2..];
    } else if dispatch_commands().contains(tokens[0]) {
        source = None;
        command = tokens[0];
        rest = &tokens[1..];
    } else if tokens[0] == "target" || tokens[0] == "think" {
        source = None;
        command = if tokens[0] == "target" { "t" } else { "th" };
        rest = &tokens[1..];
    } else {
        return None;
    }
    if rest.is_empty() {
        return None;
    }
    let joined = rest.join("-");
    if rest[0] == "backlog" || rest[0] == "evals" || rest[0] == "session" {
        return Some(Parsed {
            name: name.to_string(),
            source: source.map(str::to_string),
            command: command.to_string(),
            node: None,
            tail: joined,
        });
    }
    match node_shape().captures(&joined) {
        Some(c) => Some(Parsed {
            name: name.to_string(),
            source: source.map(str::to_string),
            command: command.to_string(),
            node: Some(match (c.get(1), c.get(2)) {
                // Full form keeps the prefix; bare hex stays bare - the
                // Python bridge re-attaches the id via the graph.
                (Some(p), Some(hex)) => format!("{}{}", p.as_str(), hex.as_str()),
                (_, Some(hex)) => hex.as_str().to_string(),
                _ => joined.clone(),
            }),
            tail: c.get(3).map(|m| m.as_str().to_string()).unwrap_or_default(),
        }),
        None => Some(Parsed {
            name: name.to_string(),
            source: source.map(str::to_string),
            command: command.to_string(),
            node: None,
            tail: joined,
        }),
    }
}

/// Is this a blueprint dispatch name? The grammar answers it, never a
/// prefix test: `bp-x-1-slug` and `ac-bp-x-2-slug` both
/// parse to command `bp`; `t-x-4-slug` does not (AC4-HP).
pub fn is_blueprint_name(name: &str) -> bool {
    parse_dispatch_agent_name(Some(name)).is_some_and(|p| p.command == "bp")
}

// ── internal machine commands (binary-direct, not `fno agents` surface) ─────────
//
// The delegation flip: Python's naming.py keeps its public signatures and
// shells THESE commands, so the tables, mint, and parse own exactly one
// implementation. Matched with `matches!` in client.rs like `reentry-plan`,
// they stay out of the routable-command parity sets by design.

fn value_of(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .or_else(|| {
            let prefix = format!("{}=", flag);
            args.iter()
                .find_map(|a| a.strip_prefix(&prefix).map(str::to_string))
        })
}

fn positionals(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with("--") {
            if a.contains('=') {
                i += 1;
            } else {
                i += 2;
            }
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

/// Slug words the `{slug}` token keeps.
const WORKER_SLUG_WORDS: usize = 3;
/// A three-word head longer than this keeps two words, so a sideline row
/// stays readable.
const WORKER_SLUG_CAP: usize = 24;
/// A rendered worker name is a CLI handle no longer than this.
const WORKER_NAME_CAP: usize = 32;

/// `agents.naming.worker` when the config sets none.
pub const DEFAULT_WORKER_TEMPLATE: &str = "t-{slug}";

/// What a worker name template draws on. A token with no fact leaves the
/// template unrendered: the caller falls to its next name source.
#[derive(Debug, Default, Clone, Copy)]
pub struct WorkerFacts<'a> {
    pub node: Option<&'a str>,
    pub slug: Option<&'a str>,
    pub skill: Option<&'a str>,
    /// Seeds `{random}`, so a row renamed on a later beat keeps one name.
    pub session: Option<&'a str>,
}

/// The `agents.naming.worker` template the project at `cwd` resolves.
pub fn worker_template(cwd: &std::path::Path) -> String {
    crate::agents_config::config_lookup(cwd, &["agents", "naming", "worker"])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_WORKER_TEMPLATE.to_string())
}

/// A seed whose first word is node-shaped binds that node (`x-2604` as the
/// whole prompt names the worker for x-2604 the way a `/target` seed would).
fn bare_seed_node(seed: &str) -> Option<String> {
    let word = seed
        .split_whitespace()
        .next()?
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    crate::law_match::matches_node_id_shape(&word).then_some(word)
}

/// The first words of a prompt (`fix the login bug` -> `fix-the-login-bug`),
/// the naming fallback when no node slug exists: up to four words, cut at a
/// hyphen boundary like every other worker slug.
fn prompt_slug_head(seed: &str) -> Option<String> {
    let words: Vec<&str> = seed.split_whitespace().take(4).collect();
    if words.is_empty() {
        return None;
    }
    let head = cut_slug(Some(&words.join("-")), WORKER_SLUG_CAP);
    (!head.is_empty()).then_some(head)
}

/// The first words of a node slug (`visual-approval-gate-two-paint` ->
/// `visual-approval-gate`).
fn slug_head(slug: &str) -> Option<String> {
    let clean = slug_component(Some(slug), usize::MAX);
    let words: Vec<&str> = clean.split('-').filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return None;
    }
    let mut head = words[..words.len().min(WORKER_SLUG_WORDS)].join("-");
    if head.len() > WORKER_SLUG_CAP && words.len() > 2 {
        head = words[..2].join("-");
    }
    let head = cut_slug(Some(head.as_str()), WORKER_SLUG_CAP);
    (!head.is_empty()).then_some(head)
}

/// Four hex digits: the second group of the session's UUID, else of a fresh
/// one. Never the first group: that is the short id the name replaces.
fn random_token(session: Option<&str>) -> Option<String> {
    let id = session.map_or_else(crate::identity::mint_fno_id, |s| Ok(s.to_string()));
    id.ok()?.get(9..13).map(str::to_ascii_lowercase)
}

/// A worker's name from an `agents.naming.worker` template: `{slug}` (the
/// node slug's first words), `{node}`, `{skill}` (the seed's command) and
/// `{random}`. No model token: the lane picks the model after the name is
/// set. `None` for a token with no fact, an unknown token, or a result that
/// does not start with a letter.
pub fn render_worker_name(template: &str, facts: &WorkerFacts) -> Option<String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = open + rest[open..].find('}')?;
        let value = match &rest[open + 1..close] {
            "slug" => slug_head(facts.slug?)?,
            "node" => slug_component(facts.node, usize::MAX),
            "skill" => slug_component(facts.skill, usize::MAX),
            "random" => random_token(facts.session)?,
            _ => return None,
        };
        if value.is_empty() {
            return None;
        }
        out.push_str(&value);
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    let name = cut_slug(Some(out.as_str()), WORKER_NAME_CAP);
    name.starts_with(|c: char| c.is_ascii_lowercase())
        .then_some(name)
}

/// The node a `/target <node>` seed names, else None. Only a target seed
/// binds: another command's worker keeps its own name form.
fn target_seed_node(seed: &str) -> Option<String> {
    let first = seed.split_whitespace().next()?;
    match crate::provider::parse_command_token(first) {
        Some(("target", _)) => crate::node_seed::scan_seed_node(seed),
        _ => None,
    }
}

/// The command a seed opens with (`/fno:target x-aaaa` -> `target`).
fn seed_skill(seed: &str) -> Option<String> {
    let first = seed.split_whitespace().next()?;
    crate::provider::parse_command_token(first).map(|(command, _)| command.to_string())
}

/// The `agents.naming.worker` name of the node `token` names. A row's own
/// cwd picks its project, so its graph and its template.
pub fn node_worker_name(token: &str, cwd: &std::path::Path, session: &str) -> Option<String> {
    let slug = node_slug(token, cwd);
    let facts = WorkerFacts {
        node: Some(token),
        slug: slug.as_deref(),
        skill: None,
        session: Some(session),
    };
    render_worker_name(&worker_template(cwd), &facts)
}

/// The slug of the node `token` names (id first, then slug), else None.
fn node_slug(token: &str, cwd: &std::path::Path) -> Option<String> {
    let path = crate::graph_get::graph_path_for(cwd);
    let by_id = crate::backlog::RowQuery {
        filter: crate::backlog::api::NodeFilter {
            id_in: Some(vec![token.to_string()]),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut rows = crate::graph_store::read_rows_where(&path, &by_id).ok()?;
    if rows.is_empty() {
        rows = crate::graph_store::read_rows_where(&path, &Default::default()).ok()?;
    }
    crate::graph_get::find_entry(&rows, token)?
        .get("slug")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `name-mint --worker <node> [--seed TEXT] [--slug S]`: a node worker's
/// name, rendered from `agents.naming.worker`. An empty `--worker` reads the
/// node from a `/target` seed, and the seed's command fills `{skill}`.
/// Prints the name; exit 3 when a template token has no fact.
fn run_worker_mint(args: &[String]) -> i32 {
    let cwd = std::path::Path::new(".");
    let (template, name) = worker_mint(
        value_of(args, "--worker").as_deref(),
        value_of(args, "--seed").as_deref(),
        value_of(args, "--slug").as_deref(),
        cwd,
    );
    match name {
        Some(name) => {
            println!("{name}");
            0
        }
        None => {
            eprintln!("error: agents.naming.worker {template:?} has a token with no value");
            3
        }
    }
}

/// A node worker's name and the template it rendered from. An empty node
/// reads the node from a `/target` seed; the seed's command fills `{skill}`.
pub(crate) fn worker_mint(
    node: Option<&str>,
    seed: Option<&str>,
    slug: Option<&str>,
    cwd: &std::path::Path,
) -> (String, Option<String>) {
    let nonblank = |v: &&str| !v.trim().is_empty();
    let seed = seed.filter(nonblank);
    let node = node
        .filter(nonblank)
        .map(str::to_string)
        .or_else(|| seed.and_then(target_seed_node))
        .or_else(|| seed.and_then(bare_seed_node));
    let slug = slug
        .filter(nonblank)
        .map(str::to_string)
        .or_else(|| node.as_deref().and_then(|n| node_slug(n, cwd)))
        .or_else(|| seed.and_then(prompt_slug_head));
    let skill = seed.and_then(seed_skill);
    let template = worker_template(cwd);
    let facts = WorkerFacts {
        node: node.as_deref(),
        slug: slug.as_deref(),
        skill: skill.as_deref(),
        session: None,
    };
    let name = render_worker_name(&template, &facts);
    (template, name)
}

/// `fno-agents name-mint [prefix] <node> [--slug S] [--qualifier Q]
/// [--discriminator D] [--source S] [--command V]`: the bridge, byte-compatible
/// with `fno agents name`. Prints the name; exit 2 usage, 3 refusal.
pub fn run_name_mint(args: &[String]) -> i32 {
    if args
        .iter()
        .any(|a| a == "--worker" || a.starts_with("--worker="))
    {
        return run_worker_mint(args);
    }
    let pos = positionals(args);
    // One positional binds the node (the CLI contract: Click would bind it to
    // the legacy prefix slot; the bridge reads it as the node instead).
    let (prefix, node) = match pos.len() {
        0 => (None, String::new()),
        1 => (None, pos[0].clone()),
        _ => (Some(pos[0].as_str()), pos[1].clone()),
    };
    if node.is_empty() {
        eprintln!("error: a node id is required: fno-agents name-mint <node-id>");
        return 2;
    }
    match bridge_name(
        prefix,
        &node,
        value_of(args, "--slug").as_deref(),
        value_of(args, "--qualifier").as_deref(),
        value_of(args, "--discriminator").as_deref(),
        value_of(args, "--source").as_deref(),
        value_of(args, "--command")
            .or_else(|| value_of(args, "--verb")) // verb-wording: allow (old flag, one release)
            .as_deref(),
        value_of(args, "--model").as_deref(),
    ) {
        Ok(name) => {
            println!("{name}");
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            e.exit
        }
    }
}

/// The shell dispatchers' bridge model: `$FNO_AGENTS_NAME_MODEL`, never a
/// flag, so a dispatcher carries it as cheaply as any env var.
pub fn name_model_from_env() -> Option<String> {
    std::env::var("FNO_AGENTS_NAME_MODEL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `fno agents name [prefix] <node>`: the routed bridge, the same mint as
/// `name-mint` with the model read from the env. Exit 3 (never 2) is the
/// naming refusal; 2 is a usage error, which a stale install also returns.
pub fn run_agents_name(args: &[String]) -> i32 {
    const USAGE: &str = "usage: fno agents name [prefix] <node-id> [--slug S] [--qualifier Q] \
                         [--discriminator D] [--source S] [--command C]";
    const VALUE_FLAGS: [&str; 6] = [
        "--slug",
        "--qualifier",
        "--discriminator",
        "--source",
        "--command",
        "--verb", // verb-wording: allow (old flag, one release)
    ];
    // The shared mint parser is lenient by design; this public door keeps
    // the strict grammar the retired Click front enforced.
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--help" || a == "-h" {
            println!("{USAGE}");
            return 0;
        }
        if let Some(flag) = a.strip_prefix("--") {
            let (name, inline) = match flag.split_once('=') {
                Some((n, _)) => (format!("--{n}"), true),
                None => (a.to_string(), false),
            };
            if !VALUE_FLAGS.contains(&name.as_str()) {
                eprintln!("error: no such option: {name}\n{USAGE}");
                return 2;
            }
            if !inline && args.get(i + 1).is_none_or(|v| v.starts_with("--")) {
                eprintln!("error: option {name} requires a value\n{USAGE}");
                return 2;
            }
            i += if inline { 1 } else { 2 };
            continue;
        }
        i += 1;
    }
    let pos = positionals(args);
    if pos.is_empty() {
        eprintln!("error: a node id is required: fno agents name [prefix] <node-id>");
        return 2;
    }
    if pos.len() > 2 {
        eprintln!("error: got unexpected extra argument ({})\n{USAGE}", pos[2]);
        return 2;
    }
    let mut argv = args.to_vec();
    if value_of(args, "--model").is_none() {
        if let Some(model) = name_model_from_env() {
            argv.extend(["--model".to_string(), model]);
        }
    }
    run_name_mint(&argv)
}

/// `fno-agents name-parse` reads one name per line on stdin and prints one
/// JSON object per line: {name, source, verb, node, tail} or {name, null} (verb-wording: allow, wire key).
pub fn run_name_parse() -> i32 {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let parsed = parse_dispatch_agent_name(Some(line.trim_end_matches('\n')));
        let json = match parsed {
            Some(p) => format!(
                "{{\"name\":{},\"source\":{},\"verb\":{},\"node\":{},\"tail\":{}}}", // verb-wording: allow (wire key)
                serde_json::to_string(&p.name).unwrap_or_else(|_| "null".into()),
                p.source
                    .as_ref()
                    .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "null".into()))
                    .unwrap_or_else(|| "null".into()),
                serde_json::to_string(&p.command).unwrap_or_else(|_| "null".into()),
                p.node
                    .as_ref()
                    .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "null".into()))
                    .unwrap_or_else(|| "null".into()),
                serde_json::to_string(&p.tail).unwrap_or_else(|_| "null".into()),
            ),
            None => format!(
                "{{\"name\":{},\"parsed\":null}}",
                serde_json::to_string(&line).unwrap_or_else(|_| "\"\"".into())
            ),
        };
        let _ = writeln!(out, "{json}");
    }
    0
}

/// The CI provenance audit's invariants on one row list: expected count, site
/// uniqueness, the two `ab` rows, and vocabulary membership. Empty means the
/// inventory is sound. Byte-matches the retired Python audit's problem texts.
fn provenance_problems(
    rows: &[(String, String, String)],
    sources: &HashSet<String>,
    commands: &HashSet<String>,
) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    if rows.len() != 17 {
        problems.push(format!("expected 17 coded paths, found {}", rows.len()));
    }
    let sites: HashSet<&String> = rows.iter().map(|(s, _, _)| s).collect();
    if sites.len() != rows.len() {
        problems.push("duplicate site labels in the inventory".to_string());
    }
    if rows.iter().filter(|(_, s, _)| s == "ab").count() != 2 {
        problems.push("both active-backlog rows must carry ab".to_string());
    }
    for (site, source, command) in rows {
        if !sources.contains(source) {
            problems.push(format!("{site}: unknown source {source:?}"));
        }
        if !commands.contains(command) && command != "resolved" {
            problems.push(format!("{site}: unknown command {command:?}"));
        }
    }
    problems
}

/// `fno-agents name-codes --json`: the vocabulary tables for thin readers.
/// `--check` runs the CI provenance audit: rows, problems, marker, exit 1 on
/// any defect.
pub fn run_name_codes(args: &[String]) -> i32 {
    let c = codes();
    if args.iter().any(|a| a == "--check") {
        let problems = provenance_problems(&c.provenance, &c.sources, &c.commands);
        for (site, source, command) in &c.provenance {
            println!("{site}\t{source}\t{command}");
        }
        for problem in &problems {
            eprintln!("error: {problem}");
        }
        if !problems.is_empty() {
            return 1;
        }
        // The marker CI greps; the retired Python audit printed the same bytes.
        println!("dispatch provenance: 17/17 coded");
        return 0;
    }
    let json = serde_json::json!({
        "sources": c.sources.iter().collect::<Vec<_>>(),
        "verbs": c.commands.iter().collect::<Vec<_>>(),
        "word_codes": c.word_codes,
        "model_codes": c.model_codes,
        "provenance": c
            .provenance
            .iter()
            .map(|(s, v, x)| serde_json::json!({"site": s, "source": v, "verb": x}))
            .collect::<Vec<_>>(),
    });
    println!("{json}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC4-HP: the grammar answers the blueprint question - every
    /// command-bp name matches whatever its source prefix, and a target row
    /// never does.
    #[test]
    fn blueprint_names_parse_by_command_not_prefix() {
        assert!(is_blueprint_name("bp-x-3-slug"));
        assert!(is_blueprint_name("ac-bp-x-1-slug"));
        assert!(!is_blueprint_name("t-x-4-slug"));
    }

    #[test]
    fn mint_dispatch_form_and_shaving() {
        let n = dispatch_agent_name(
            Some("ab"),
            "bp",
            "x-aaaa",
            Some("Ab Names"),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(n, "ab-bp-aaaa-ab-names");
        let manual = dispatch_agent_name(None, "t", "x-aaaa", None, None, None, None).unwrap();
        assert_eq!(manual, "t-aaaa");
    }

    #[test]
    fn model_segment_appends_and_refuses_unknown_only_when_blank_squeezes() {
        // Known model: longest-key-first substring match on the lowered model.
        let n = dispatch_agent_name(
            Some("ab"),
            "bp",
            "x-bbbb",
            Some("250ms Read Floor"),
            None,
            None,
            Some("glm-5.3-flash[1m]"),
        )
        .unwrap();
        assert_eq!(n, "ab-bp-bbbb-250ms-read-glm");
        // Unknown model squeezes to 8 alphanumeric characters.
        let n = dispatch_agent_name(
            Some("ab"),
            "t",
            "x-1",
            None,
            None,
            None,
            Some("Kimi K2 Thinking"),
        )
        .unwrap();
        assert_eq!(n, "ab-t-1-kimik2th");
        // Blank model is absent, never a segment of empty text.
        let n = dispatch_agent_name(Some("ab"), "t", "x-1", None, None, None, Some("  ")).unwrap();
        assert_eq!(n, "ab-t-1");
    }

    #[test]
    fn unknown_source_and_command_refuse() {
        assert!(dispatch_agent_name(Some("zz"), "t", "x-1", None, None, None, None).is_err());
        assert!(dispatch_agent_name(None, "zz", "x-1", None, None, None, None).is_err());
        assert!(command_code_for(Some("impeccable")).is_err());
        assert_eq!(command_code_for(Some("/fno:target")).unwrap(), "t");
        assert_eq!(command_code_for(Some("$fno:blueprint")).unwrap(), "bp");
        assert_eq!(command_code_for(Some("builtin")).unwrap(), "t");
    }

    #[test]
    fn required_identity_never_shaves() {
        let long = "n-".to_string() + &"z".repeat(70);
        let err = dispatch_agent_name(Some("ab"), "t", &long, None, None, None, None).unwrap_err();
        assert!(err.message.contains("64-char"), "{err}");
        assert_eq!(err.exit, 3);
    }

    /// A node of another project than the cwd still names its worker: the
    /// slug reads from the shared store the cwd resolves.
    #[test]
    fn node_worker_name_reads_the_slug_from_the_store() {
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let saved = std::env::var_os("FNO_STATE_DIR");
        std::env::set_var("FNO_STATE_DIR", tmp.path());
        let graph = crate::state_layout::graph_anchor(tmp.path());
        crate::graph_store::seed_rows(
            &graph,
            &[serde_json::json!({
                "id": "fb-bd7a",
                "slug": "books-index-sweep-run",
                "title": "Books index sweep",
                "status": "idea",
                "project": "fno-books"
            })],
        )
        .unwrap();
        let sid = "cccccccc-ab12-0000-0000-000000000003";
        let named = node_worker_name("fb-bd7a", tmp.path(), sid);
        let missing = node_worker_name("fb-0000", tmp.path(), sid);
        // A project's agents.naming.worker renders every token it names.
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(project.join(".fno")).unwrap();
        std::fs::write(
            project.join(".fno/config.toml"),
            "[agents.naming]\nworker = \"{node}-{slug}-{random}\"\n",
        )
        .unwrap();
        let templated = node_worker_name("fb-bd7a", &project, sid);
        match saved {
            Some(v) => std::env::set_var("FNO_STATE_DIR", v),
            None => std::env::remove_var("FNO_STATE_DIR"),
        }
        assert_eq!(named.as_deref(), Some("t-books-index-sweep"));
        assert_eq!(missing, None);
        assert_eq!(templated.as_deref(), Some("fb-bd7a-books-index-sweep-ab12"));
    }

    /// The default template: `t-` and the first slug words, two when three
    /// run long, and no model token. `{random}` is stable per session and is
    /// never the session's own short id. Only a target seed binds a node.
    #[test]
    fn worker_template_renders_its_tokens() {
        let slug = |s: &str| {
            render_worker_name(
                DEFAULT_WORKER_TEMPLATE,
                &WorkerFacts {
                    slug: Some(s),
                    ..Default::default()
                },
            )
        };
        assert_eq!(
            slug("visual-approval-gate-two-paint").as_deref(),
            Some("t-visual-approval-gate")
        );
        assert_eq!(
            slug("spawned-workers-name-them-t-short").as_deref(),
            Some("t-spawned-workers-name")
        );
        assert_eq!(
            slug("observability-instrumentation-dashboards").as_deref(),
            Some("t-observability")
        );
        assert_eq!(slug("fix").as_deref(), Some("t-fix"));
        assert_eq!(slug("--"), None);
        let facts = WorkerFacts {
            node: Some("X-AAAA"),
            session: Some("cccccccc-AB12-0000-0000-000000000003"),
            ..Default::default()
        };
        let random = render_worker_name("w-{node}-{random}", &facts);
        assert_eq!(random.as_deref(), Some("w-x-aaaa-ab12"));
        // No skill fact, an unknown token, an open brace, a leading digit.
        for bad in ["{skill}-{node}", "{bogus}", "t-{node", "7-{node}"] {
            assert_eq!(render_worker_name(bad, &facts), None, "{bad}");
        }
        assert_eq!(seed_skill("/fno:blueprint x").as_deref(), Some("blueprint"));
        assert_eq!(
            target_seed_node("/fno:target x-aaaa").as_deref(),
            Some("x-aaaa")
        );
        assert_eq!(
            target_seed_node("$fno:target L x-aaaa").as_deref(),
            Some("x-aaaa")
        );
        assert_eq!(target_seed_node("/fno:blueprint x-aaaa"), None);
        assert_eq!(target_seed_node("go"), None);
    }

    #[test]
    fn a_bare_node_seed_binds_its_slug() {
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let saved = std::env::var_os("FNO_STATE_DIR");
        std::env::set_var("FNO_STATE_DIR", tmp.path());
        let graph = crate::state_layout::graph_anchor(tmp.path());
        crate::graph_store::seed_rows(
            &graph,
            &[serde_json::json!({
                "id": "fb-2604",
                "slug": "spawn-flags-ride-verbatim",
                "title": "Spawn flags ride verbatim",
                "status": "idea",
                "project": "fno"
            })],
        )
        .unwrap();
        let (_, name) = worker_mint(None, Some("fb-2604"), None, tmp.path());
        match saved {
            Some(v) => std::env::set_var("FNO_STATE_DIR", v),
            None => std::env::remove_var("FNO_STATE_DIR"),
        }
        assert_eq!(name.as_deref(), Some("t-spawn-flags-ride"));
    }

    #[test]
    fn a_prompt_seed_names_the_worker_from_its_words() {
        let (_, name) = worker_mint(
            None,
            Some("fix the login bug today"),
            None,
            std::path::Path::new("."),
        );
        assert_eq!(name.as_deref(), Some("t-fix-the-login-bug"));
        // Four words fit the cap; a long word run cuts at a boundary.
        let (_, long) = worker_mint(
            None,
            Some("refactor the entire ingestion pipeline end to end"),
            None,
            std::path::Path::new("."),
        );
        let named = long.expect("a prompt names the worker");
        let words = named.trim_start_matches("t-").split('-').count();
        assert!((2..=4).contains(&words), "{named}");
    }

    #[test]
    fn parse_positional_grammar() {
        let p = parse_dispatch_agent_name(Some("ab-bp-x-aaaa-slug")).unwrap();
        assert_eq!(p.source.as_deref(), Some("ab"));
        assert_eq!(p.command, "bp");
        assert_eq!(p.node.as_deref(), Some("x-aaaa"));
        assert_eq!(p.tail, "slug");
        // Bare-hex node: parsed as the node, hex kept bare.
        let p = parse_dispatch_agent_name(Some("ab-bp-84b2-slug")).unwrap();
        assert_eq!(p.node.as_deref(), Some("84b2"));
        assert_eq!(p.tail, "slug");
        // A node prefix colliding with a code never misreads as a source.
        let p = parse_dispatch_agent_name(Some("t-x-aaaa")).unwrap();
        assert!(p.source.is_none());
        assert_eq!(p.command, "t");
        assert_eq!(p.node.as_deref(), Some("x-aaaa"));
        // Typed identities stay opaque.
        let p = parse_dispatch_agent_name(Some("ro-t-session-abcd1234")).unwrap();
        assert!(p.node.is_none());
        assert_eq!(p.tail, "session-abcd1234");
        // Pre-cutover names resolve through the legacy read window.
        let legacy = parse_dispatch_agent_name(Some("target-x-aaaa-1")).unwrap();
        assert_eq!(legacy.source.as_deref(), None);
        assert_eq!(
            (
                legacy.command.as_str(),
                legacy.node.as_deref(),
                legacy.tail.as_str()
            ),
            ("t", Some("x-aaaa"), "1")
        );
        let legacy = parse_dispatch_agent_name(Some("think-x-aaaa-retro")).unwrap();
        assert_eq!(
            (legacy.command.as_str(), legacy.node.as_deref()),
            ("th", Some("x-aaaa"))
        );
        assert!(parse_dispatch_agent_name(Some("j-x-cccc-2")).is_none());
    }

    #[test]
    fn bridge_forms() {
        let n = bridge_name(
            None,
            "x-aaaa",
            Some("Ab Names"),
            None,
            None,
            Some("ab"),
            Some("bp"),
            None,
        )
        .unwrap();
        assert_eq!(n, "ab-bp-aaaa-ab-names");
        assert_eq!(
            bridge_name(
                Some("legacy"),
                "x-1",
                None,
                None,
                None,
                Some("ab"),
                Some("t"),
                None,
            )
            .unwrap_err()
            .exit,
            2
        );
        assert_eq!(
            bridge_name(None, "x-1", None, None, None, Some("ab"), None, None)
                .unwrap_err()
                .exit,
            2
        );
        assert_eq!(
            bridge_name(None, "x-1", None, None, None, None, None, None)
                .unwrap_err()
                .exit,
            2
        );
        let n = bridge_name(Some("target"), "x-cccc", None, None, None, None, None, None).unwrap();
        assert_eq!(n, "target-cccc");
        // One positional (empty prefix) with a command is the node.
        let n = bridge_name(
            None,
            "x-aaaa",
            None,
            None,
            None,
            Some("kl"),
            Some("th"),
            None,
        )
        .unwrap();
        assert_eq!(n, "kl-th-aaaa");
    }

    #[test]
    fn name_codes_check_refuses_an_unknown_source() {
        // A doctored row list exercises the audit's negative path without
        // breaking the real inventory: an unknown source names the site.
        let rows = vec![("x-site".to_string(), "zz".to_string(), "t".to_string())];
        let problems = provenance_problems(&rows, dispatch_sources(), dispatch_commands());
        assert!(
            problems
                .iter()
                .any(|p| p.contains("x-site: unknown source \"zz\"")),
            "expected the unknown-source problem, got {problems:?}"
        );
    }
}
