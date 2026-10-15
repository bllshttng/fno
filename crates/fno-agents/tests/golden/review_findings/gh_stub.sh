#!/bin/sh
# Fake gh for the review_findings goldens (frozen by
# review_findings_golden_driver.py, read by review_findings_parity.rs).
# Every call logs one space-joined argv line to $FAKE_GH_LOG, with the -f
# body argument's value written to $FAKE_GH_BODY instead (newlines must
# survive). Answers come from $FAKE_GH_CASE fixture files.
log_line=""
prev=""
for a in "$@"; do
  if [ "$prev" = "-f" ]; then
    printf '%s' "${a#body=}" > "${FAKE_GH_BODY:?FAKE_GH_BODY must be set}"
    prev="$a"
    continue
  fi
  if [ -z "$log_line" ]; then log_line="$a"; else log_line="$log_line $a"; fi
  prev="$a"
done
printf '%s\n' "$log_line" >> "${FAKE_GH_LOG:?FAKE_GH_LOG must be set}"
case "$log_line" in
  *rate_limit*)
    printf '{"resources":{"core":{"remaining":4990,"reset":1}}}\n'
    exit 0
    ;;
esac
if [ "$1" != "api" ]; then
  echo "fake-gh: unexpected: $log_line" >&2
  exit 1
fi
post=0
for a in "$@"; do
  if [ "$a" = "POST" ]; then post=1; fi
done
if [ "$post" = 1 ]; then
  if [ -f "$FAKE_GH_CASE/post_fail" ]; then echo boom >&2; exit 1; fi
  printf '{}\n'
  exit 0
fi
path=""
for a in "$@"; do
  case "$a" in
    *repos/*) path="$a" ;;
  esac
done
if [ -z "$path" ]; then
  echo "fake-gh: no repos path: $log_line" >&2
  exit 1
fi
case "$path" in
  *"pulls?state=all"*)
    if [ -f "$FAKE_GH_CASE/pulls_fail" ]; then echo boom >&2; exit 1; fi
    if [ -f "$FAKE_GH_CASE/pulls_list.json" ]; then
      cat "$FAKE_GH_CASE/pulls_list.json"
    else
      printf '[]\n'
    fi
    exit 0
    ;;
  *"comments"*)
    if [ -f "$FAKE_GH_CASE/comments_fail" ]; then echo boom >&2; exit 1; fi
    for a in "$@"; do
      if [ "$a" = "--paginate" ] && [ -f "$FAKE_GH_CASE/comments_both.json" ]; then
        cat "$FAKE_GH_CASE/comments_both.json"
        exit 0
      fi
    done
    cat "$FAKE_GH_CASE/comments_p1.json"
    exit 0
    ;;
  *"pulls/"*)
    if [ -f "$FAKE_GH_CASE/pr_info_fail" ]; then echo boom >&2; exit 1; fi
    cat "$FAKE_GH_CASE/pr_info.json"
    exit 0
    ;;
esac
echo "fake-gh: unmatched path: $path" >&2
exit 1
