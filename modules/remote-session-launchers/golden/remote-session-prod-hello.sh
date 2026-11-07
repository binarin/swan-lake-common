#!/nix/store/HASH-bash/bin/bash
exec /nix/store/HASH-remote-session-focus/bin/remote-session-focus '[P] prod-hello' remote-session-prod-hello -- \
  -o 'main.locked-title=yes' \
  -o 'colors-dark.background=101010' \
  -e /nix/store/HASH-stub-openssh/bin/ssh -t -- prod.example.net 'nix run '\''github:binarin/swan-lake-common/swan-lake-current#hello'\'' -- --greeting '\''hi there'\'''

