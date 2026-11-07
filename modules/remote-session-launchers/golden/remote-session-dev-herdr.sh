#!/nix/store/HASH-bash/bin/bash
exec /nix/store/HASH-remote-session-focus/bin/remote-session-focus dev-herdr remote-session-dev-herdr -- \
  -o 'main.locked-title=yes' \
  -e /nix/store/HASH-stub-openssh/bin/ssh -t -- dev.example.com herdr

