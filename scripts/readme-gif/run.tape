# Render the README GIF from a redacted events file. From the repo root:
#   scripts/readme-gif/extract.sh <transcript.jsonl> <name>=demo > /tmp/events.jsonl
#   grep -ci '<name>\|<host>' /tmp/events.jsonl   # must print 0
#   EVENTS=/tmp/events.jsonl vhs scripts/readme-gif/run.tape
# The GIF must stay under 30 seconds and 5 MB.
Output docs/assets/readme-run.gif
Set Shell "bash"
Set FontSize 15
Set Width 980
Set Height 560
Set Padding 18
Set Framerate 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 0

Hide
Type "export PS1=''; clear; scripts/readme-gif/replay.sh $EVENTS 22"
Enter
Sleep 300ms
Show
Sleep 28s
