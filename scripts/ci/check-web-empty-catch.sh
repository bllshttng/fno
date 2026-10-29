#!/usr/bin/env bash
# check-web-empty-catch.sh - refuse an empty catch in the web pages. A page
# must never swallow an error: a catch shows the failure to the user, or it
# says why silence is right with a `/* quiet-ok: <reason> */` comment as its
# whole body.
#
# Scans every tracked .html file under crates/. Refused shapes, when the
# body holds only whitespace and comments and no comment says quiet-ok:
#   catch {}   catch (e) {}   .catch(() => {})   .catch((e) => {})
#   .catch(function () {})
#
# Run:  bash scripts/ci/check-web-empty-catch.sh [--self-test]
# Exit: 0 clean, 1 a refused catch.

set -euo pipefail

scan() {
  perl -0777 -ne '
    my $body = qr{\{((?:\s|/\*.*?\*/|//[^\n]*)*)\}}s;
    while (/(catch\s*(?:\(\s*[\w\$]*\s*\))?\s*$body|\.catch\(\s*(?:\(\s*[\w\$]*\s*\)\s*=>|[\w\$]+\s*=>|function\s*\(\s*[\w\$]*\s*\))\s*$body\s*\))/g) {
      my ($hit, $inner) = ($1, defined $2 ? $2 : $3);
      next if $inner =~ /quiet-ok/;
      my $line = 1 + (substr($_, 0, $-[0]) =~ tr/\n//);
      print "$ARGV:$line: empty catch: $hit\n";
    }
  ' "$@"
}

if [ "${1:-}" = "--self-test" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  printf '%s\n' 'try { a(); } catch (e) {}' > "$tmp/bad1.html"
  printf '%s\n' 'try { a(); } catch {' '  }' > "$tmp/bad2.html"
  printf '%s\n' 'fetch(u).catch(() => {});' > "$tmp/bad3.html"
  printf '%s\n' 'fetch(u).catch(function (e) { });' > "$tmp/bad4.html"
  printf '%s\n' 'try { a(); } catch (e) { /* ignore */ }' > "$tmp/bad5.html"
  printf '%s\n' 'fetch(u).catch(() => {' '  // nothing to do' '});' > "$tmp/bad6.html"
  printf '%s\n' 'try { a(); } catch (e) { /* quiet-ok: no storage */ }' \
    'fetch(u).catch((e) => show(e));' 'try { a(); } catch (e) { ok = false; }' \
    'r.json().catch(() => ({}));' 'p.catch(() => { /* quiet-ok: best effort */ });' > "$tmp/good.html"
  for f in bad1 bad2 bad3 bad4 bad5 bad6; do
    if [ -z "$(scan "$tmp/$f.html")" ]; then
      echo "self-test: $f.html passed but must be refused" >&2
      exit 1
    fi
  done
  if [ -n "$(scan "$tmp/good.html")" ]; then
    echo "self-test: good.html was refused:" >&2
    scan "$tmp/good.html" >&2
    exit 1
  fi
  echo "self-test: ok (6 refused, 5 passed)"
  exit 0
fi

cd "$(git rev-parse --show-toplevel)"
files=()
while IFS= read -r f; do files+=("$f"); done < <(git ls-files 'crates/*.html')
if [ "${#files[@]}" -eq 0 ]; then
  echo "check-web-empty-catch: no web pages found under crates/" >&2
  exit 1
fi
hits=$(scan "${files[@]}")
if [ -n "$hits" ]; then
  printf '%s\n' "$hits"
  echo "check-web-empty-catch: show the error to the user, or give the catch a /* quiet-ok: <reason> */ body" >&2
  exit 1
fi
echo "check-web-empty-catch: ok (${#files[@]} pages)"
