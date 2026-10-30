# Records docs/demo.gif. Run scripts/demo/record.sh, which builds the
# release binary and adds the current Omarchy theme's colors to this tape.
# Needs vhs (pacman -S vhs) and python3. The made-up files live in
# ~/work/pc-demo (see scripts/demo/setup.sh), with their own settings and
# trash, so your real ones are not touched.
#
# vhs has no F-keys: each one is typed as Escape plus the rest of its
# escape sequence, and scripts/demo/keyproxy.py glues the two together.
#   F3 = Escape "OR"     F5 = Escape "[15~"   F7 = Escape "[18~"
#   F8 = Escape "[19~"   F9 = Escape "[20~"

Output docs/demo.gif

Set Shell bash
Set FontFamily "JetBrainsMono Nerd Font"
Set FontSize 20
Set Width 1440
Set Height 760
Set Padding 20
Set Framerate 24
Set TypingSpeed 90ms

Hide
Type "D=$HOME/work/pc-demo && scripts/demo/setup.sh $D && export XDG_CONFIG_HOME=$D/config XDG_DATA_HOME=$D/data && clear"
Enter
Type "python3 scripts/demo/keyproxy.py target/release/peter-commander $D/peter/Projects/starfield $D/peter/Backup"
Enter
Sleep 1.5s
Show

Sleep 1.5s

# Quick view (F3) follows the cursor.
Down@300ms 6
Escape@0
Type@0 "OR"
Sleep 1.2s
Down@1.2s 2
Down@400ms 3
Sleep 1.5s
Escape@0
Type@0 "OR"
Sleep 600ms

# Mark two files with Insert and copy them (F5) to the other pane.
Up@300ms 3
Insert
Down@300ms 2
Insert
Sleep 600ms
Escape@0
Type@0 "[15~"
Sleep 1.5s
Enter
Sleep 1.5s

# Other pane: make a directory (F7).
Tab
Sleep 600ms
Escape@0
Type@0 "[18~"
Sleep 800ms
Type "old-builds"
Sleep 500ms
Enter
Sleep 1.2s

# Back left: delete build.log (F8) to the trash.
Tab
Up@250ms 6
Sleep 500ms
Escape@0
Type@0 "[19~"
Sleep 1.2s
Left
Sleep 500ms
Enter
Sleep 1.5s

# Walk up and into another folder.
PageUp
Sleep 400ms
Enter
Sleep 1s
Up 1
Sleep 400ms
Enter
Sleep 2.5s

# Options > Settings (F9): switch to the classic Norton Commander colors.
Escape@0
Type@0 "[20~"
Sleep 700ms
Right
Sleep 700ms
Enter
Sleep 1s
Down@350ms 4
Sleep 600ms
Right
Sleep 1s
Enter
Sleep 1.2s
Tab
Down@500ms 3
Sleep 2.5s
