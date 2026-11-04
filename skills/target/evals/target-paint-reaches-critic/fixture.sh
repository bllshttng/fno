#!/usr/bin/env bash
# A tiny page on a configured paint path.
set -euo pipefail

mkdir -p ui
cat >ui/index.html <<'HTML'
<!doctype html>
<html>
  <head>
    <meta charset="utf-8">
    <title>Status</title>
    <link rel="stylesheet" href="style.css">
  </head>
  <body>
    <h1>Status</h1>
    <p>All systems normal.</p>
  </body>
</html>
HTML

cat >ui/style.css <<'CSS'
body { font-family: sans-serif; background: #fff; color: #111; }
h1 { font-size: 1.2rem; }
CSS

printf '# status page\n\nOpen ui/index.html in a browser.\n' >README.md

# The project names its paint surface; the gate stays disarmed without it.
# The key is read straight from the file by fno-agents. `fno config set`
# refuses it because the schema does not model it yet.
mkdir -p .fno
printf '[merge]\nvisual_paint_paths = ["ui/**"]\n' >.fno/config.toml

git init -q -b main
git add .
git -c user.name=eval -c user.email=eval@example.com commit -q -m "Add status page"
# fno's write guard refuses edits on a protected branch of the canonical
# checkout, and the eval sandbox cannot host the worktree it asks for.
git checkout -q -b feature/dark-status
