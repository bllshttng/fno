#!/usr/bin/env bash
# check-no-verb-wording.sh - refuse the word "verb" on lines a change adds to
# user-facing text. Not every fno command is a verb (mux, whoami, doctor
# footprint), so the user-facing word is "command": "slash command" or "CLI
# command" when the two must be told apart.
#
# The gate reads the diff, never the whole tree: thousands of old uses stay
# until the lines that carry them are touched, and a touched line is fixed in
# the change that touches it.
#
# Scanned, on added lines only:
#   prose  AGENTS.md, README.md, and *.md under docs/, skills/, commands/,
#          agents/. The whole line counts.
#   code   *.rs, *.py, *.sh under cli/src/, crates/*/src/, hooks/, skills/.
#          Only double-quoted string literals count: that is where help
#          text, refusals and receipts live. Identifiers stay for now.
# Never scanned: test paths (/tests/, /test/, test_*, *_tests.rs) and golden
# fixtures. A word joined to a name (harness-verbs, dispatch_verb, verb=)
# is a name, not the word, and passes. So does a `backticked` span in prose:
# it quotes a literal, such as a command whose own name is not renamed yet.
#
# Escape: a line carrying `verb-wording: allow` passes, for text that must
# quote the old word (a migration note, this file).
#
# Run:  bash scripts/ci/check-no-verb-wording.sh [--self-test]
# Env:  VERB_WORDING_BASE  the ref to diff against (default origin/main).
# Exit: 0 clean, 1 a refused line, 2 an unresolvable base.

set -euo pipefail

# The word alone: not joined to a name by - _ / . : = or a letter or digit.
WORD_RE='(?<![-_/.:=A-Za-z0-9])verbs?(?![-_/=A-Za-z0-9]|\.[A-Za-z0-9])'

scan() {
    # stdin: unified diff with zero context. stdout: file:line: text per hit.
    perl -ne '
        BEGIN { $re = qr{'"$WORD_RE"'}i; }
        if (/^\+\+\+ b\/(.*)$/) { $f = $1; next; }
        if (/^@@ -\S+ \+(\d+)/) { $n = $1; next; }
        next unless defined $f;
        if (/^\+/) {
            my $t = substr($_, 1); chomp $t;
            my $line = $n++;
            next if $t =~ /verb-wording: allow/;
            next if $f =~ m{(^|/)tests?/|(^|/)test_|_tests?\.(rs|py)$|/golden/};
            my $prose = $f =~ m{^(AGENTS|README)\.md$|^(docs|skills|commands|agents)/.*\.md$};
            my $code = $f =~ m{^(cli/src|crates/[^/]+/src|hooks|skills)/.*\.(rs|py|sh)$};
            my $hit = 0;
            if ($prose) { (my $p = $t) =~ s/`[^`]*`//g; $hit = $p =~ $re; }
            elsif ($code) {
                while ($t =~ /"((?:[^"\\]|\\.)*)"/g) { $hit = 1 if $1 =~ $re; }
            }
            print "$f:$line: $t\n" if $hit;
        } elsif (!/^-/) { $n++; }
    '
}

if [[ "${1:-}" == "--self-test" ]]; then
    diff=$'+++ b/docs/a.md\n@@ -0,0 +1,4 @@\n+run the verb.\n+see harness-verbs and `fno-agents verbs`\n+quote it: verb (verb-wording: allow)\n+run the command\n+++ b/crates/x/src/a.rs\n@@ -0,0 +1,3 @@\n+let v = dispatch_verb("ok");\n+eprintln!("unknown verb {v}");\n+// a verb in a comment\n+++ b/crates/x/tests/a.rs\n@@ -0,0 +1 @@\n+assert!("a verb");\n'
    got="$(printf '%s' "$diff" | scan)"
    want=$'docs/a.md:1: run the verb.\ncrates/x/src/a.rs:2: eprintln!("unknown verb {v}");'
    if [[ "$got" != "$want" ]]; then
        printf 'check-no-verb-wording self-test FAILED\nwant:\n%s\ngot:\n%s\n' "$want" "$got" >&2
        exit 1
    fi
    echo "check-no-verb-wording self-test: ok"
    exit 0
fi

cd "$(git rev-parse --show-toplevel)"
base_ref="${VERB_WORDING_BASE:-origin/main}"
if ! base="$(git merge-base "$base_ref" HEAD 2>/dev/null)"; then
    echo "check-no-verb-wording: cannot resolve base $base_ref; fetch it or set VERB_WORDING_BASE" >&2
    exit 2
fi
hits="$(git diff --unified=0 --no-color --no-renames "$base" HEAD | scan)"
if [[ -n "$hits" ]]; then
    printf '%s\n' "$hits"
    echo "check-no-verb-wording: write \"command\" for user-facing text (\"slash command\" or \"CLI command\" to tell them apart). Quoting the old word on purpose: add verb-wording: allow to the line." >&2
    exit 1
fi
echo "check-no-verb-wording: ok"
