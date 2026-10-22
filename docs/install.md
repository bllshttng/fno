# Install fno

## Is this page for you?

You decided to try fno. This page installs it on your harness, proves the install worked, and sets up your first project. When `fno --version` prints a version and `/fno:` autocompletes, go to the [quickstart](quickstart.md).

Not for: a first PR. That is the [quickstart](quickstart.md).

## Install

### Claude Code

In any Claude Code session:

```
/plugin marketplace add bllshttng/fno#stable
/plugin install fno@footnote
```

`#stable` matters. Without it, the add clones `main` (7,949 files, 80s+) instead of `stable` (781 files, 4s). A slow connection can then hit Claude Desktop's 120s "Add marketplace" timeout.

If the add refuses because the name `footnote` is already in use: `fno config plugin install claude` already staged a local directory marketplace under that same name, at `~/.fno/plugin-stage/fno`. Remove that entry, or the whole stage, before you add the GitHub source under the same name. Or skip the GitHub add and keep using the staged copy.

On the first session after install, footnote prints the install plan and starts nothing: a person's yes is what runs it. On a yes, run `bash <plugin root>/.claude-plugin/postinstall.sh`. Its output ends with `installer exit 0` on success. Then open a new session, and `fno` is on your PATH.

### Codex, opencode, agy and other harnesses

Codex CLI:

```
codex plugin marketplace add bllshttng/fno@stable
codex plugin add fno@footnote
```

Antigravity (`agy`): run `bash scripts/install/agy-plugin.sh` from a clone. It installs a staged payload and checks that `agy plugin list` names Footnote. For a slow install, set `AGY_PLUGIN_INSTALL_TIMEOUT_S=120`.

Every other harness has its own page under [harnesses/](harnesses/). [HARNESSES.md](HARNESSES.md) says what works on each one.

### The CLI alone

`curl -fsSL fno.sh | sh`, `uv tool install fno`, `pip install fno`, or `brew install bllshttng/fno/fno` each install the published PyPI wheel. The wheel bundles the complete set: the Rust `fno` front door, the three `fno-agents` binaries, and the Python CLI (`fno-py`). `cargo install fno` is the source route. It builds the Rust front door with your Rust toolchain. The front door bootstraps the Python CLI on first use.

### Local clone

Clone, run setup, get the CLI:

```bash
git clone https://github.com/bllshttng/fno.git footnote
cd footnote
bash scripts/setup.sh
```

The setup checks the prerequisites (`bash`, `git`, `gh`, `jq`, and `python3` 3.11+ or `uv`). It registers the global `~/.fno` state dir and scaffolds `.fno/config.toml` in the clone. When no `fno` answers, it installs the CLI through the same `fno.sh` the curl one-liner uses. Afterwards `fno`, `fno-agents`, and the mux all answer. Per-path overrides live in the `config.paths.*` keys: see [path configuration](path-config.md).

### Windows (WSL2)

fno runs under [WSL2](https://learn.microsoft.com/windows/wsl/install), not native Windows. The loop leans on POSIX file locking, Unix sockets, and signals that Windows handles differently. WSL2 is real Linux, so everything here works unchanged inside it.

One-time setup, from PowerShell as Administrator:

```powershell
wsl --install        # installs WSL2 + Ubuntu; reboot if prompted
```

Then open the Ubuntu shell and work from there. Install `gh`, Python 3.11+, and `jq`. Run Claude Code inside WSL2, and follow the install steps above. Keep your repos on the Linux filesystem (under `~/`), not `/mnt/c/...`. On the Windows mount, file locking and file watches are slow and unreliable.

## Verify it worked

```bash
fno --version          # prints a version
```

If `fno` is "command not found", your install predates the complete payload or is a source/editable dev build (those are development artifacts and carry no binaries). Every supported channel ships the Rust **`fno` front door**: the wheel, the plugin's first-session installer, `fno.sh`, and Homebrew. The front door owns the mux and bootstraps the Python CLI. Repair: upgrade to a current release through your channel, or install the front door directly with `cargo install fno` (needs a **Rust toolchain**, `rustup`). When the front door is missing, a Claude Code session also reminds you. Until it is installed, reach the CLI directly as `fno-py`. When even `fno-py` is missing, run the zero-binary doctor from the installed plugin tree: `bash ~/.claude/plugins/cache/footnote/fno/*/scripts/install/bootstrap.sh`. It prints every prerequisite's state and exact fix as JSON. `--repair` runs the plugin's own installer and never edits shell rc files. `--from-source <checkout>` builds and installs from a clone.

Inside Claude Code, type `/fno:` and skill autocomplete appears (`target`, `think`, `blueprint`, ...).

```bash
claude /status         # shows your Claude account; if not, run: claude login
gh auth status         # authenticated; if not, run: gh auth login
```

`gh auth status` is the most common cold-start failure. Credentials for each provider are in [the auth guide](auth.md).

## Configure your project

Configuration lives in `.fno/config.toml` (project-local) layered over `~/.fno/config.toml` (global). The global file holds shared defaults. The project file holds only the per-repo deltas. There are two ways to set it up.

**In a Claude Code session (agent-driven):**

```
/fno:setup
```

**In the terminal, no agent (CLI-native):**

```bash
fno config setup run --yes         # takes every recommended default, never prompts, prints the JSON report
fno config setup run --list --json # the step table (global, project, contributor layers)
fno config setup plan --advanced   # the advanced config tier, as JSON
```

Both walk the same schema-derived question plan and write through the validated config writer, so a typo or an out-of-range value is rejected, not silently stored.

The terminal wizard also offers, defaulting to No, to wire fno's SessionStart context into Codex and Gemini user config. You can do the same later with `fno config setup cli-hooks`.

### Reading and editing config directly

```bash
fno config get config.review.github_apps            # read one value
fno config set config.auto_merge.enabled true       # set one key (atomic, schema-checked)
fno config set a.b=1 c.d=2                           # set several keys in one atomic call
fno config unset config.auto_merge.enabled          # remove a key (reverts to its default)
fno config doctor                                    # what resolved, and any suspicious values
```

`fno config set` also takes a whole block as JSON: `fno config set config.review '{"github_apps":["chatgpt-codex-connector"]}'`.

### The settings you'll touch first

These are real keys in `config.toml`. Run `fno config get <key>` to read any of them, or `fno config schema --markdown` for the complete reference:

| Key | What it does | Default |
|-----|--------------|---------|
| `config.review.github_apps` | External review bots that must approve before `target` calls a PR done; none set means no external gate | none |
| `config.review.posture` | How much review a code PR needs before it can merge; unset floors at `self_review` (nine-rung ladder) | none |
| `config.review.max_rounds` | Review rounds per PR, counted across its whole life | `2` |
| `config.review.external_reviewers` | Which reviewer(s) `pr check` waits on (e.g. `gemini`, `codex`) | `[]` |
| `config.auto_merge.enabled` | Let `target` merge a PR itself once review passes | `false` |
| `config.target.defaults.max_iterations` | How many times `target` retries before stopping | `40` |
| `config.backlog.id_prefix` | The prefix for minted backlog node ids (e.g. `fno-a3f9`); unset falls back to `ab-` | none |
| `config.obsidian.enabled` + `.vault` | Store plans and design docs in an Obsidian vault | `false` |
| `config.project.vision` | One line: what this codebase is and why (project-scoped) | none |

Budget and skip behavior are not config keys. They are flags you pass to a run, for example `/fno:target --budget 25 "..."` or `/fno:target --no-external "..."`. See [the target guide](guides/target.md) for the full flag list.

## Set up a second machine

A second machine does not re-answer every question. Export the setup from a machine that already works, copy the profile over, and join it on the new one.

On the set-up machine:

```bash
fno config setup run --export --out fno-profile.toml
```

On the new machine, pick the join that fits who is present:

| Who runs it | Command |
|---|---|
| A person, in a terminal | `fno do wizard --join fno-profile.toml` |
| A person, in a session | `/fno:setup join fno-profile.toml` |
| A script, no person | `fno config setup run --yes --only join --join fno-profile.toml` |

The join prints the diff first, then applies. The previous global config is backed up to `~/.fno/sidecar/setup-backup/`. Later, `fno config setup run --check --json` shows how the machine has drifted, each row with its fix.

What travels is decided per key, once, by `classify` in `crates/fno/src/setup_profile.rs`:

| Class | Decided by | On a join |
|---|---|---|
| Secret | the leaf name carries `token`, `secret` or `password`, or ends `api_key` | never exported |
| Local | machine paths and names: the `paths.`, `work.`, `user.`, `build.`, `accounts.`, `agents.provider_limits.`, `store.` families; the exact keys `state_dir`, `plans_dir`, `obsidian.vault`, `research.output_dir`, `lead.enabled`, `agents.max_live`, `agents.max_live_per_territory`; or any string value starting `/` or `~` | stays behind, listed in the diff |
| Portable | everything else | travels |

The machine-local rows still need answering here. The wizard handles the big one: tick its projects screen and it appends this checkout to `work.workspaces.main.projects` through the validated writer.

When a person runs the wizard, a preset sets the pace. The preset screen opens with `balanced` highlighted. `--preset aggressive|balanced|user-reviewed` pre-answers it.

| Preset | Decides | Harnesses | Models | Services | Auto-merge |
|---|---|---|---|---|---|
| aggressive | everything with safe answers | every detected one | the first catalog lane per harness | all three | on after review and CI |
| balanced | the safe middle | the detected ones with credentials | each harness keeps its current model | the PR watcher and the groom | off |
| user-reviewed | nothing without you | none ticked | each harness keeps its current model | none | off |

## Keeping fno up to date

From a clone, the full refresh has three steps. `fno doctor update` refreshes the binaries. The long-running mux server and agents daemon keep old code until restarted:

```bash
git pull
fno doctor update          # reinstall the Python CLI + cargo binaries (fno mux + fno-agents) from source
fno agents session restart --mux   # restart the daemon AND the mux server onto the fresh binaries
```

`fno agents session restart` on its own restarts only the agents daemon (PTY workers survive). The `--mux` flag also restarts the mux server, which is **destructive**: it ends live mux sessions, so it is opt-in. Reattach afterward. `fno doctor` flags a running mux server that predates the installed binary and reminds you to run it. In a running Claude Code session, bump the plugin (or relaunch) to pick up new skills and hooks after a pull.

## Uninstall

Run it from a plain terminal, not from inside an fno mux pane:

```bash
fno uninstall --dry-run    # list every item found; change nothing
fno uninstall              # ask once, then remove them all
fno uninstall --yes        # no question; needed when stdin is not a terminal
```

It removes each item through its own uninstall verb, in this order:

1. The launchd agents (`~/Library/LaunchAgents/sh.fno.*.plist` and the autocorrect agents).
2. The Claude Code and Codex plugins and their `footnote` marketplaces, and the OpenCode plugin files.
3. The hooks fno added to `~/.claude/settings.json`, `~/.gemini/settings.json` and `~/.codex/config.toml`. Your own hooks stay.
4. The `uv` tool, the Homebrew formula and the bootstrap cache.
5. The daemon and the mux sessions, which it stops.
6. The `cargo` binaries, `fno` last.

`~/.fno` stays. `--purge` deletes it too, after you type `purge`. It holds your backlog and any worktrees under `~/.fno/worktrees`, so commit or push that work first. The run ends with a list of anything named fno that is still on `PATH`, in `LaunchAgents` or running. Exit 0 means that list is empty.

## Next

- [Quickstart](quickstart.md): your first PR with fno.
- [Troubleshooting](troubleshooting.md): the failures first runs hit.
