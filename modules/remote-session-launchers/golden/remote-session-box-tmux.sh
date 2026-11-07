#!/nix/store/HASH-bash/bin/bash
exec /nix/store/HASH-remote-session-focus/bin/remote-session-focus box-tmux remote-session-box-tmux -- \
  -o 'main.locked-title=yes' \
  -o 'colors-dark.background=001800' \
  -e /nix/store/HASH-stub-mosh/bin/mosh '--ssh=/nix/store/HASH-stub-openssh/bin/ssh' '--predict=always' -- box.example.org tmux -u new-session -A -s main

