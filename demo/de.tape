# Rebuild the binary, then replay with: termctrl play demo/de.tape
Session de-readme-demo
Viewport 92 10
Cell 9 18
Cwd ".."
Env TERM "xterm-256color"
Record "de.termctrl"
Color always
Setup "/bin/bash" "demo/fixture.sh" "setup"
Cleanup "/bin/bash" "demo/fixture.sh" "cleanup"
Launch "/bin/bash" "demo/shell.sh"

Wait "de-demo ~/workspace $"
Mark ready
Sleep 1600ms
Type "ls" Pace 140ms
Key enter
Wait "frontend"
Sleep 1800ms
Type "de" Pace 140ms
Key enter
Wait "workspace"
Mark picker
Sleep 2400ms
Key down
Wait "guides/"
Sleep 1400ms
Key up
Sleep 1000ms
Key right
Wait "api"
Sleep 2200ms
Key down
Wait "health.rs"
Sleep 1400ms
Key up
Sleep 900ms
Key right
Wait "src"
Sleep 1800ms
Key left
Wait "api"
Sleep 1500ms
Type "/test" Pace 180ms
Wait "filter: test"
Sleep 2000ms
Mark filtered
Key escape
Sleep 900ms
Key enter
Wait "de-demo ~/workspace/api $"
Mark done
Stop
