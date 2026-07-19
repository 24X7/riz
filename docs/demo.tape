# VHS tape — recorded terminal demo for the riz landing page.
#
# Generates a GIF showing the riz dev loop in motion:
#   1. `riz new typescript-bun my-app` (scaffold)
#   2. `cd my-app && riz run` (boots TUI)
#   3. live latency percentiles update as traffic hits
#   4. `riz mcp inspect` in a second pane shows the auto-registered tool
#
# Run: `vhs docs/demo.tape` → produces docs/riz-demo.gif
# Install vhs: brew install vhs
#
# Visual settings tuned for the dark-navy/amber landing page.

Output docs/riz-demo.gif

Set FontSize 14
Set Width 1200
Set Height 720
Set Theme {
  "name": "riz-dark",
  "black": "#0E1220",
  "red": "#E84A6F",
  "green": "#4F9758",
  "yellow": "#F0A500",
  "blue": "#6E7D99",
  "magenta": "#C8D8F0",
  "cyan": "#F0A500",
  "white": "#C8D8F0",
  "brightBlack": "#4A5775",
  "brightRed": "#E84A6F",
  "brightGreen": "#4F9758",
  "brightYellow": "#FFB833",
  "brightBlue": "#C8D8F0",
  "brightMagenta": "#C8D8F0",
  "brightCyan": "#FFB833",
  "brightWhite": "#FFFFFF",
  "background": "#0E1220",
  "foreground": "#C8D8F0",
  "cursor": "#F0A500",
  "selection": "#1E2540"
}

# Frame 1 — scaffold
Type "riz new typescript-bun my-app"
Sleep 600ms
Enter
Sleep 1500ms

# Frame 2 — cd + run
Type "cd my-app && riz run"
Sleep 600ms
Enter
Sleep 3000ms

# Frame 3 — generate some traffic in the background, then inspect
# (vhs records what's visible — the user sees the boot logs, then we type more)
Sleep 1500ms

Type "# In real demos a second pane runs:"
Sleep 400ms
Enter
Type "#   wrk -t2 -c10 -d10s http://localhost:3000/hello?name=alice"
Sleep 400ms
Enter
Type "#   riz mcp inspect"
Sleep 400ms
Enter
Sleep 1500ms

# Frame 4 — riz mcp inspect (shows the auto-registered tool)
Ctrl+C
Sleep 800ms
Type "riz mcp inspect"
Sleep 400ms
Enter
Sleep 4000ms
