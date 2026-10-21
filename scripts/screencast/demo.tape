# Interactions replayed by the Playwright recorder. ttyd launches the TUI.
Wait+Screen /A working notebook/
Sleep 2s

# Browse the guide, then search across the canvas.
Type "j"
Sleep 2s
Type "/Monthly sales"
Sleep 1s
Escape
Sleep 500ms

# Run the report. The region default needs no input.
Type "r"
Wait+Screen /Report ready for North/
Sleep 2s

# Configure the region and repeat the same block.
Type "c"
Wait+Screen /REGION/
Sleep 1s
Backspace 5
Type "South"
Sleep 500ms
Enter
Sleep 500ms
Type "r"
Wait+Screen /Report ready for South/
Sleep 2s

# Show the local chart, then add a node from the command palette.
Type "j"
Wait+Screen /Images render right inside/
Sleep 3s
Ctrl+P
Wait+Screen /Commands/
Sleep 2s
Type "new child"
Sleep 1s
Enter
Wait+Screen /Title:/
Type "Next steps"
Sleep 500ms
Enter
Wait+Screen /Next steps/
Sleep 1s

# Write its text in the source editor and save.
Type "e"
Sleep 1s
Type "Go"
Type "***Ready to share.***"
Escape
Ctrl+S
Sleep 1s
Escape
Wait+Screen /Ready to share/
Sleep 2s

# Cover the live terminal before disconnecting, so reconnect UI is never recorded.
EndCard 3s
