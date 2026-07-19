# riz — terminal demo tape for charmbracelet/vhs.
#
# Renders a REAL recording against the actual riz binary: scaffold a project,
# run it, hit it over HTTP, and see the function already exposed as a typed MCP
# tool — the exact story the hero animation tells, but captured from real output
# for the README / social cards.
#
#   Prereqs : vhs  (https://github.com/charmbracelet/vhs), plus riz + bun on PATH
#   Render  : vhs assets/demo.tape        # writes assets/demo.gif + assets/demo.mp4
#
# vhs runs every command for real in a headless terminal and records the screen,
# so the GIF can never drift from what the binary actually does. (CI alternative:
# the charmbracelet/vhs-action GitHub Action renders the same tape on demand.)

Output assets/demo.gif
Output assets/demo.mp4

Require riz
Require bun

Set Shell "bash"
Set FontSize 18
Set Width 1300
Set Height 760
Set Padding 30
Set TypingSpeed 55ms
# riz "terminal acid" palette — green is the acid accent (#CFFF04).
Set Theme { "name": "riz", "background": "#0A0A0B", "foreground": "#FAFAF6", "black": "#101013", "red": "#FF5F57", "green": "#CFFF04", "yellow": "#FEBC2E", "blue": "#9A9AA2", "magenta": "#CFFF04", "cyan": "#28C840", "white": "#FAFAF6", "brightBlack": "#6E6E78", "brightRed": "#FF5F57", "brightGreen": "#CFFF04", "brightYellow": "#FEBC2E", "brightBlue": "#9A9AA2", "brightMagenta": "#CFFF04", "brightCyan": "#28C840", "brightWhite": "#FAFAF6", "cursor": "#CFFF04", "selection": "#34343C" }

# Scratch dir, kept off-camera.
Hide
Type "cd $(mktemp -d)" Enter
Show

Type "riz new typescript-bun orders && cd orders" Sleep 600ms Enter
Sleep 2.5s

Type "cat index.ts" Enter
Sleep 3s

# Start it (logs redirected so the recording stays clean).
Type "riz run > /tmp/riz-demo.log 2>&1 &" Enter
Sleep 3.5s

Type "curl -s 'localhost:3000/hello?name=alice'; echo" Enter
Sleep 2.6s

# Every function is already a typed MCP tool — no SDK, no glue.
Type "riz mcp inspect" Enter
Sleep 4.5s

Hide
Type "kill %1 2>/dev/null; cd - >/dev/null" Enter
Show
Sleep 800ms
