#!/nix/store/HASH-bash/bin/bash
exec /nix/store/HASH-remote-session-focus/bin/remote-session-focus box-hello remote-session-box-hello -- \
  -o 'main.locked-title=yes' \
  -e /nix/store/HASH-stub-mosh/bin/mosh '--ssh=/nix/store/HASH-stub-openssh/bin/ssh' -- box.example.org nix run 'github:example/flake/main#hello' -- 'it'\''s' ''

