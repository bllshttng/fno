#!/usr/bin/env bash
# check-no-verb-wording.sh - refuse the word "verb" anywhere in the tracked
# tree. Not every fno command is a verb (mux, whoami, doctor footprint), so
# the word is "command": "slash command" or "CLI command" when the two must be
# told apart. Identifiers, paths, keys and prose all follow it.
#
# The gate scans the whole tree, not a diff: the rename is done, so any hit
# is new. A token counts when it holds "verb" in any case, alone or joined to
# a name (dispatch_verb, harness-verbs, VerbMeta).
#
# Passes:
#   - words that are not the fno concept: verbatim, verbose, verbosity,
#     verbal, adverb, proverb, verbiage, reverb.
#   - a line carrying `verb-wording: allow`, for text that must keep the old
#     word: a read-both migration key, an old flag accepted for one release.
#   - the wire keys the events schema and the harness capability table still
#     spell (both are cargo-generated, so they move after the build freeze):
#     "verb", "verbs", "verb_source", "verb_hint" as whole quoted literals,
#     and the table keys native_verbs, native_verb_meta, review_verbs.
#   - old flags: --dispatch-verb (retired; the retired-command table and a
#     recorded transcript keep spelling it) and --verb, --next-verb,
#     --ack-verb-risk (accepted for one release beside their new names).
#   - the node_dispatch.verb SQL column: older installed binaries share
#     graph.db and select it by name, so the stored name stays.
#   - role_migration's retired_verb, a frozen generated-copy source.
#   - the graph keeper's old store method name "effective_verb", served
#     beside "effective_command" so a client of an older build still reads.
#   - this gate's own name.
#   - files listed in ALLOW_FILES below.
#
# Run:  bash scripts/ci/check-no-verb-wording.sh [--self-test]
# Exit: 0 clean, 1 a refused line.

set -euo pipefail

# Path globs (perl regexes) whose content may keep the word.
ALLOW_FILES='^(LICENSE[^/]*|.*CHANGELOG[^/]*|scripts/ci/check-no-verb-wording\.sh|skills/harness-verbs/SKILL\.md)$'
# Cargo-generated copies and the sources the build freeze holds.
ALLOW_FILES+='|^(cli/src/fno/events/schema\.yaml|skills/target/cli/src/fno/events/schema\.yaml)$'
ALLOW_FILES+='|^(crates/fno-agents/src/harness_capabilities\.toml|crates/fno/src/harness_capabilities\.toml|cli/src/fno/agents/harness_capabilities\.toml)$'
ALLOW_FILES+='|^(crates/fno-agents/src/event_store/events_schema\.json|crates/fno/src/event_store/events_schema\.json)$'
ALLOW_FILES+='|^(crates/fno-agents/src/role_migration\.rs|crates/fno/src/role_migration\.rs)$'
ALLOW_FILES+='|^((crates/fno-agents/src|cli/src/fno/agents)/(spawn_phase|merge_posture|registry_schema)\.toml)$'
ALLOW_FILES+='|^(crates/(fno-agents|fno)/src/(event_store|effect_map|tick_ledger|help_router)\.rs|crates/(fno-agents|fno)/src/event_store/observation\.rs)$'
ALLOW_FILES+='|^(docs/harnesses/command-matrix\.md|docs/loops\.md|docs/architecture/help-routing\.md|skills/agent/references/launch-[a-z-]+\.md)$'
# A recorded transcript: what the agent typed, kept byte for byte.
ALLOW_FILES+='|^crates/fno-agents/tests/fixtures/lead/lead-2026-08-05\.json$'

scan() {
    # stdin: path:line:text rows (git grep -n). stdout: the refused rows.
    perl -ne '
        BEGIN {
            $files = qr{'"$ALLOW_FILES"'};
            $not = qr{verbatim|verbos|verbal|adverb|proverb|verbiage|reverb}i;
            $keys = qr{(["\x27])(?:verbs?|verb_source|verb_hint)\1|\b(?:native_verbs|native_verb_meta|review_verbs|retired_verb)\b|--(?:dispatch-verb|next-verb|ack-verb-risk|verb)\b|check-no-verb-wording|"effective_verb"};
            $sql = qr{\b(?:d|excluded|node_dispatch)\.verb\b|\bverb TEXT\b|\(node_id, verb\b|\bSELECT verb\b|\bSET verb\b};
        }
        chomp;
        my ($f, $n, $t) = split /:/, $_, 3;
        next unless defined $t;
        next if $f =~ $files;
        next if $t =~ /verb-wording: allow/;
        (my $s = $t) =~ s/$keys//g;
        $s =~ s/$sql//g if $f eq "crates/fno-agents/src/backlog/nodes.rs";
        my $hit = 0;
        while ($s =~ /([A-Za-z0-9_]*verb[A-Za-z0-9_]*)/gi) {
            $hit = 1 unless $1 =~ $not;
        }
        print "$_\n" if $hit;
    '
}

if [[ "${1:-}" == "--self-test" ]]; then
    rows=$'docs/a.md:1:run the verb.\n'
    rows+=$'docs/a.md:2:print it verbatim, verbosely\n'
    rows+=$'docs/a.md:3:quote it: verb (verb-wording: allow)\n'
    rows+=$'crates/x/src/a.rs:4:let v = dispatch_verb(x);\n'
    rows+=$'crates/x/src/a.rs:5:data.insert("verb".into(), v);\n'
    rows+=$'crates/x/src/a.rs:6:caps.native_verbs.len()\n'
    rows+=$'crates/x/src/a.rs:7:("--dispatch-verb", "dispatch_command=<command>"),\n'
    rows+=$'crates/x/src/a.rs:8:fn run(command: &str) {}\n'
    rows+=$'LICENSE:9:verbal communication\n'
    rows+=$'crates/x/src/a.rs:10:enum Kind { VerbMeta }\n'
    rows+=$'crates/fno-agents/src/backlog/nodes.rs:11:SELECT verb, brief FROM node_dispatch\n'
    rows+=$'crates/fno-agents/src/backlog/nodes.rs:12:let verb = 1;\n'
    rows+=$'crates/x/src/a.rs:13:"--ack-command-risk" | "--ack-verb-risk" => {\n'
    got="$(printf '%s' "$rows" | scan)"
    want=$'docs/a.md:1:run the verb.\ncrates/x/src/a.rs:4:let v = dispatch_verb(x);\ncrates/x/src/a.rs:10:enum Kind { VerbMeta }\ncrates/fno-agents/src/backlog/nodes.rs:12:let verb = 1;'
    if [[ "$got" != "$want" ]]; then
        printf 'check-no-verb-wording self-test FAILED\nwant:\n%s\ngot:\n%s\n' "$want" "$got" >&2
        exit 1
    fi
    echo "check-no-verb-wording self-test: ok"
    exit 0
fi

cd "$(git rev-parse --show-toplevel)"
paths="$(git ls-files | grep -i -n 'verb' | sed 's/^\([0-9]*\):\(.*\)$/\2:path:\2/' || true)"
hits="$( { git grep -n -I -i 'verb' || true; printf '%s\n' "$paths"; } | scan)"
if [[ -n "$hits" ]]; then
    printf '%s\n' "$hits"
    echo "check-no-verb-wording: write \"command\" (\"slash command\" or \"CLI command\" to tell them apart). Keeping the old word on purpose, as a migration key or an old flag: add verb-wording: allow to the line." >&2
    exit 1
fi
echo "check-no-verb-wording: ok"
