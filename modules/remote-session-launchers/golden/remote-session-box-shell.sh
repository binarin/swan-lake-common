#!/nix/store/HASH-bash/bin/bash
exec /nix/store/HASH-remote-session-focus/bin/remote-session-focus box remote-session-box-shell -- \
  -o 'main.locked-title=yes' \
  -e /nix/store/HASH-stub-mosh/bin/mosh '--ssh=/nix/store/HASH-stub-openssh/bin/ssh' -- box.example.org

