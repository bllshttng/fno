# Render the README GIF from a redacted events file.
# Usage: EVENTS=path/to/events.jsonl vhs scripts/readme-gif/run.tape
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
Type "export PS1=''; clear; scripts/readme-gif/replay.sh $EVENTS 24"
Enter
Sleep 300ms
Show
Sleep 26s
