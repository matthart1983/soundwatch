# SoundWatch demo — recorded with vhs (github.com/charmbracelet/vhs)
#
# Recorded in the btop theme: charts in braille, two sub-columns per cell, and
# bars shaded by height. The palette is left at its default, so the colours are
# whatever the recording terminal defines — the tape switches it to `spec` near
# the end to show both.
Output docs/media/demo.gif

Set Shell zsh
Set FontSize 13
Set Width 1500
Set Height 800
Set Padding 8
Set Framerate 20
Set PlaybackSpeed 1.0
Set TypingSpeed 40ms

Hide
Type "cd ~/projects/soundwatch"  Enter
Type "alias soundwatch=./target/release/soundwatch"  Enter
Type "clear"  Enter
Sleep 500ms
Show

Type "soundwatch --demo --theme btop"
Sleep 700ms
Enter
Sleep 2800ms

# [2] Devices — what exists and how it is attached
Type "2"
Sleep 2600ms

# [3] Streams — who has the microphone
Type "3"
Sleep 2400ms

# [4] Meters — peak, RMS and the crest factor between them
Type "4"
Sleep 3200ms

# [5] Spectrum — what is in the signal, in braille
Type "5"
Sleep 3800ms

# [9] Timeline — what changed while you were not looking
Type "9"
Sleep 2600ms

# [0] Insights — all of it, in plain English
Type "0"
Sleep 3200ms

# Settings: down one row to `palette`, and off the terminal's colours onto
# the design handoff's own.
Type ","
Sleep 1600ms
Down
Sleep 1200ms
Right
Sleep 2000ms
Escape
Sleep 800ms

# Back to the spectrum, in the handoff palette
Type "5"
Sleep 4000ms

Type "q"
Sleep 600ms
