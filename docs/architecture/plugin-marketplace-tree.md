# Marketplace plugin trees

Claude and Codex consume the same generated plugin tree. The release workflow publishes it to `stable` after release approval and to `plugin-nightly` for nightly builds. Both branches advance through ordinary fast-forward pushes. The separate `nightly` release tag remains force-moved by design for binary downloads.

The CI-only Rust module at `crates/fno-agents/ci/plugin_tree.rs` builds from a Git archive of the selected source commit. It copies skills, agents, commands, hooks, assets, harness manifests and the transitive set of scripts named by those files. It excludes repository source and test trees. `SOURCE_COMMIT` names the source commit; the generated README identifies main as the contribution branch. Both plugin versions receive the release version.

The generated tree requires installed `fno` and `fno-agents` binaries on PATH. Install them with `https://fno.sh`. Hook helpers use standard-library Python or the installed Python interpreter. They never create an environment inside the plugin. Hook commands disable bytecode writes. Session start warns when `fno version --json` names a commit that differs from the plugin source; versions with no commit do not trigger a comparison.

`scripts/release/publish-plugin.sh` constructs an exact Git tree with a private index. Its commit has the current remote channel tip as its parent, including the first migration from a full repository tree. An ordinary push refuses concurrent conflicting updates. Publication runs only in CI. Local developer staging retains its separate all-tracked-files contract and rewrites both marketplace manifests to install from that stage.

The `plugin-tree` guards job runs `scripts/ci/check-plugin-tree.sh`. It builds the real archive, checks the 5000-file maximum, and proves rejection of 5001 files, `cli/`, `crates/`, symlinks and hard links. It exercises the join guard in a read-only tree, then publishes twice to a disposable bare remote that rejects non-fast-forward pushes. An existing clone must update with `git pull --ff-only`, and the published archive must pass the same tree validator. These checks run in CI; local file counts do not establish release acceptance.

The main-branch Codex marketplace uses its URL source form with `ref: stable`; the generated tree serves its own local plugin. Claude uses its GitHub source form with the same ref. Claude nightly uses `plugin-nightly`. Buddy remains a separate plugin sourced from main through an HTTPS Git URL.
