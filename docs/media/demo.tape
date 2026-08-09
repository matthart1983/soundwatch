# SoundWatch demo — recorded with vhs (github.com/charmbracelet/vhs)
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
Type "alias soundwatch=./target/debug/soundwatch"  Enter
Type "clear"  Enter
Sleep 500ms
Show

Type "soundwatch --demo"
Sleep 700ms
Enter
Sleep 2500ms

# [2] Devices — what exists and how it is attached
Type "2"
Sleep 2600ms

# [3] Streams — who has the microphone
Type "3"
Sleep 2400ms

# [4] Meters — peak, RMS and the crest factor between them
Type "4"
Sleep 2600ms

# [5] Spectrum — what is in the signal
Type "5"
Sleep 3800ms

# [9] Timeline — what changed while you were not looking
Type "9"
Sleep 2600ms

# [0] Insights — all of it, in plain English
Type "0"
Sleep 3200ms

# the settings menu, and the btop theme
Type ","
Sleep 1600ms
Right
Sleep 1800ms
Escape
Sleep 800ms

# back to the spectrum, now in braille with a gradient
Type "5"
Sleep 4000ms

Type "q"
Sleep 600ms
