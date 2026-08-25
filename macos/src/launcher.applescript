-- Droplet entry point for MeshfoxCanvas.app. A bare Unix executable set as
-- CFBundleExecutable never receives the files Finder hands over on
-- double-click/"Open With"/drag-onto-icon (verified empirically: Finder
-- launches it, but always with an empty argv) — only a real app with its
-- own run loop does, via the standard "open documents" Apple Event.
-- `osacompile` compiles this into exactly that: a small droplet app whose
-- `on open` handler actually receives it.
--
-- `meshfox`'s own path is resolved fresh from PATH on every launch (see
-- resolveMeshfox below), not baked in at build time — that way a
-- `meshfox` installed (or reinstalled/upgraded) after this app was last
-- built is still found.

on open theFiles
	set meshfoxBin to my resolveMeshfox()
	if meshfoxBin is missing value then
		tell application "Terminal"
			activate
			do script "curl -fsSL https://raw.githubusercontent.com/orofarne/meshfox/main/scripts/install.sh | sh"
		end tell
		return
	end if
	repeat with f in theFiles
		set p to POSIX path of f
		do shell script "nohup " & quoted form of meshfoxBin & " view " & quoted form of p & ¬
			" >> \"${TMPDIR:-/tmp}meshfox-canvas-opener.log\" 2>&1 &"
	end repeat
end open

on run
	-- Launched with no documents — e.g. a plain double-click on the app
	-- itself, or the `build` block's own "warm up LaunchServices trust"
	-- step. Nothing to do.
end run

on resolveMeshfox()
	-- meshfox's own install script (see the `on open` handler above)
	-- defaults to ~/.local/bin, which isn't on PATH out of the box on a
	-- fresh Mac — mixed in here explicitly so a from-Terminal-just-now
	-- install is found immediately, without the user having to add it to
	-- their shell rc file first.
	try
		return do shell script "PATH=\"$HOME/.local/bin:$PATH\" command -v meshfox"
	on error
		return missing value
	end try
end resolveMeshfox
