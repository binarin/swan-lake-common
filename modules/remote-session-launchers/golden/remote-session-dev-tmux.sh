#!/nix/store/HASH-bash/bin/bash
exec /nix/store/HASH-remote-session-focus/bin/remote-session-focus 'dev-tmux [dev]' remote-session-dev-tmux -- \
  -o 'main.locked-title=yes' \
  -o 'colors-dark.background=001800' \
  -e /nix/store/HASH-stub-openssh/bin/ssh -t -- dev.example.com 'tmux -u new-session -A -D -s work'

