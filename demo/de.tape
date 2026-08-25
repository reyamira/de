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

Wait "de-demo workspace $"
Sleep 800ms
Mark ready
Type "de" Pace 100ms
Key enter
Wait "workspace"
Mark picker
Sleep 1200ms
Key down
Sleep 650ms
Key up
Key right
Wait "api"
Sleep 900ms
Key down
Sleep 500ms
Key up
Sleep 350ms
Key right
Wait "src"
Sleep 900ms
Key left
Wait "api"
Sleep 700ms
Type "/test" Pace 120ms
Sleep 900ms
Mark filtered
Key escape
Sleep 500ms
Key enter
Wait "de-demo api $"
Mark done
Sleep 1400ms
Stop
