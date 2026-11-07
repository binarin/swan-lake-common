#!/nix/store/HASH-bash/bin/bash
exec /nix/store/HASH-remote-session-focus/bin/remote-session-focus '[PROD] prod-btop' remote-session-prod-btop -- \
  -o 'main.locked-title=yes' \
  -o 'colors-dark.background=180000' \
  -e /nix/store/HASH-stub-openssh/bin/ssh -t -p 2222 -- admin@prod.example.net btop

