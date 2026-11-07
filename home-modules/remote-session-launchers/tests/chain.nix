{ pkgs, home }:
let
  inherit (pkgs) lib;

  fakeScript = name: ''
    { echo '#!${pkgs.python3}/bin/python3 -I'; cat ${./fakes.py}; } > $out/bin/${name}
    chmod +x $out/bin/${name}
  '';
  fake =
    names:
    pkgs.runCommand "fake-${lib.head names}" { } ''
      mkdir -p $out/bin
      ${lib.concatMapStrings fakeScript names}
    '';
  fakes = {
    openssh = fake [ "ssh" ];
    foot = fake [ "foot" ];
    niri = fake [ "niri" ];
    uwsm = fake [ "uwsm" ];
    # The real pinned jq and mosh client; mosh fails after the fake
    # mosh-server records its argv, as no server answers the handshake.
    inherit (pkgs) jq mosh;
  };
  remote = fake [
    "tmux"
    "herdr"
    "nix"
    "prog"
    "mosh-server"
  ];
  ssh = "${fakes.openssh}/bin/ssh";
  foot = "${fakes.foot}/bin/foot";
  mosh = "${pkgs.mosh}/bin/mosh";

  nasty = [
    "a b"
    "it's"
    ''say "hi"''
    "$HOME"
    "$(echo injected)"
    "`echo injected`"
    "a;b"
    "*"
    "back\\slash"
    "two\\\\back"
    "\\'"
    "'"
    ""
    "-leading"
  ];
  defaultFlake = "github:binarin/swan-lake-common/swan-lake-current";

  launchers =
    settings:
    let
      config = home [
        {
          programs.remoteSessionLaunchers = {
            enable = true;
            packages = fakes;
          }
          // settings;
        }
      ];
    in
    id: "${config.home.activationPackage}/home-files/.local/share/applications/remote-session-${id}.desktop";

  item = protocol: host: remote: { inherit protocol host remote; name = host; };

  chain = launchers {
    tags.t = {
      background = "102030";
      titlePrefix = "[T] ";
    };
    items = {
      ssh-exec = item "ssh" "user@exec.example.com" { exec = [ "prog" ] ++ nasty; } // {
        protocolArgs = [ "-p" "2222" "-o" "SendEnv=X" ];
        tag = "t";
      };
      ssh-nix = item "ssh" "nix.example.com" {
        nixRun = {
          package = "pkg";
          args = nasty;
        };
      };
      ssh-nix-flake = item "ssh" "nix.example.com" {
        nixRun = {
          package = "pkg";
          flake = "github:example/f/b";
        };
      };
      ssh-tmux = item "ssh" "tmux.example.com" { tmux.session = "my session"; };
      ssh-herdr = item "ssh" "herdr.example.com" { herdr = { }; };
      ssh-shell = item "ssh" "shell.example.com" { shell = { }; };
      mosh-exec = item "mosh" "exec.example.net" { exec = [ "prog" ] ++ nasty; } // {
        protocolArgs = [ "--predict=never" ];
      };
      mosh-nix = item "mosh" "nix.example.net" {
        nixRun = {
          package = "pkg";
          args = nasty;
        };
      };
      mosh-tmux = item "mosh" "tmux.example.net" {
        tmux = {
          session = "s'1";
          detachOthers = false;
        };
      };
      mosh-shell = item "mosh" "shell.example.net" { shell = { }; };
    };
  };

  allShells = [ "sh" "dash" "zsh" ];
  footPrefix = title: [ "foot" "--title" title "-o" "main.locked-title=yes" ];
  sshCase = id: host: remoteArgv: {
    name = id;
    desktop = chain id;
    shells = allShells;
    expect = [
      { prefix = footPrefix id ++ [ "-e" ssh "-t" "--" host ]; }
      { prefix = [ "ssh" host ]; }
      { exact = remoteArgv; }
    ];
  };
  moshCase = id: host: server: {
    name = id;
    desktop = chain id;
    shells = allShells;
    ignoreStatus = true;
    expect = [
      { exact = footPrefix id ++ [ "-e" mosh "--ssh=${ssh}" "--" host ] ++ server.argv or [ ]; }
      { prefix = [ "ssh" host ]; }
      ({ prefix = [ "mosh-server" "new" ]; } // removeAttrs server [ "argv" ])
    ];
  };

  chainCases = [
    {
      name = "ssh-exec";
      desktop = chain "ssh-exec";
      shells = allShells;
      expect = [
        {
          prefix = footPrefix "[T] ssh-exec" ++ [
            "-o"
            "colors-dark.background=102030"
            "-e"
            ssh
            "-t"
            "-p"
            "2222"
            "-o"
            "SendEnv=X"
            "--"
            "user@exec.example.com"
          ];
        }
        { prefix = [ "ssh" "user@exec.example.com" ]; }
        { exact = [ "prog" ] ++ nasty; }
      ];
    }
    (sshCase "ssh-nix" "nix.example.com" ([ "nix" "run" "${defaultFlake}#pkg" "--" ] ++ nasty))
    (sshCase "ssh-nix-flake" "nix.example.com" [ "nix" "run" "github:example/f/b#pkg" "--" ])
    (sshCase "ssh-tmux" "tmux.example.com" [ "tmux" "-u" "new-session" "-A" "-D" "-s" "my session" ])
    (sshCase "ssh-herdr" "herdr.example.com" [ "herdr" ])
    {
      name = "ssh-shell";
      desktop = chain "ssh-shell";
      expect = [
        { exact = footPrefix "ssh-shell" ++ [ "-e" ssh "-t" "--" "shell.example.com" ]; }
        { exact = [ "ssh" "shell.example.com" ]; }
      ];
    }
    {
      name = "mosh-exec";
      desktop = chain "mosh-exec";
      shells = allShells;
      ignoreStatus = true;
      expect = [
        {
          exact = footPrefix "mosh-exec" ++ [
            "-e"
            mosh
            "--ssh=${ssh}"
            "--predict=never"
            "--"
            "exec.example.net"
            "prog"
          ]
          ++ nasty;
        }
        { prefix = [ "ssh" "exec.example.net" ]; }
        {
          prefix = [ "mosh-server" "new" ];
          suffix = [ "--" "prog" ] ++ nasty;
        }
      ];
    }
    (
      let
        argv = [ "nix" "run" "${defaultFlake}#pkg" "--" ] ++ nasty;
      in
      moshCase "mosh-nix" "nix.example.net" {
        inherit argv;
        suffix = [ "--" ] ++ argv;
      }
    )
    (
      let
        argv = [ "tmux" "-u" "new-session" "-A" "-s" "s'1" ];
      in
      moshCase "mosh-tmux" "tmux.example.net" {
        inherit argv;
        suffix = [ "--" ] ++ argv;
      }
    )
    (moshCase "mosh-shell" "shell.example.net" { absent = "--"; })
  ];

  # Focus helper and uwsm handling, with niri windows titled near-misses.
  focusItems = {
    t = item "ssh" "t.example.com" { shell = { }; } // {
      title = "T";
    };
    q = item "ssh" "q.example.com" { shell = { }; } // {
      title = ''say "hi"'';
    };
    sub = item "ssh" "sub.example.com" { shell = { }; } // {
      title = "T-oth";
    };
  };
  windows = builtins.toJSON [
    {
      id = 1;
      title = "T-other";
    }
    {
      id = 2;
      title = "T";
    }
    {
      id = 3;
      title = ''say "hi"'';
    }
    {
      id = 4;
      title = "xT";
    }
  ];
  plain = launchers { items = focusItems; };
  uwsm = launchers {
    items = focusItems;
    uwsm.enable = true;
  };
  customMarker = launchers {
    items = focusItems;
    uwsm.enable = true;
    uwsm.wrappedMarker = "MY_MARK";
  };
  focused = id: { exact = [ "niri" "msg" "action" "focus-window" "--id" id ]; };
  started = title: host: [
    { exact = footPrefix title ++ [ "-e" ssh "-t" "--" host ]; }
    { exact = [ "ssh" host ]; }
  ];
  viaUwsm = {
    exact = [ "uwsm" "app" "-a" "remote-session-t" "--" foot ] ++ lib.drop 1 (lib.head (started "T" "t.example.com")).exact;
  };
  focusCase = name: desktop: env: expect: { inherit name desktop env expect; };

  focusCases = [
    (focusCase "focus-exact" (plain "t") { FAKE_NIRI_WINDOWS = windows; } [ (focused "2") ])
    (focusCase "focus-quote" (plain "q") { FAKE_NIRI_WINDOWS = windows; } [ (focused "3") ])
    (focusCase "focus-no-substring" (plain "sub") { FAKE_NIRI_WINDOWS = windows; } (started "T-oth" "sub.example.com"))
    (focusCase "niri-not-running" (plain "t") { } (started "T" "t.example.com"))
    (focusCase "focus-fails" (plain "t") {
      FAKE_NIRI_WINDOWS = windows;
      FAKE_NIRI_FOCUS_STATUS = "1";
    } ([ (focused "2") ] ++ started "T" "t.example.com"))
    (focusCase "uwsm-off-never-uwsm" (plain "t") { } (started "T" "t.example.com"))
    (focusCase "uwsm-focus-first" (uwsm "t") { FAKE_NIRI_WINDOWS = windows; } [ (focused "2") ])
    (focusCase "uwsm-marker-unset" (uwsm "t") { } ([ viaUwsm ] ++ started "T" "t.example.com"))
    (focusCase "uwsm-marker-set" (uwsm "t") { _UWSM_WRAPPED = "1"; } (started "T" "t.example.com"))
    (focusCase "uwsm-marker-empty" (uwsm "t") { _UWSM_WRAPPED = ""; } (started "T" "t.example.com"))
    (focusCase "uwsm-custom-marker-unset" (customMarker "t") { _UWSM_WRAPPED = "1"; } (
      [ viaUwsm ] ++ started "T" "t.example.com"
    ))
    (focusCase "uwsm-custom-marker-set" (customMarker "t") { MY_MARK = "1"; } (started "T" "t.example.com"))
  ];

  spec = {
    cases = chainCases ++ focusCases;
    path = lib.makeBinPath [ pkgs.coreutils ];
    remotePath = lib.makeBinPath [
      remote
      pkgs.coreutils
      pkgs.bash
    ];
    shells = {
      sh = "${pkgs.bash}/bin/sh";
      dash = "${pkgs.dash}/bin/dash";
      zsh = "${pkgs.zsh}/bin/zsh";
    };
  };
in
pkgs.runCommand "remote-session-launchers-chain"
  {
    nativeBuildInputs = [ pkgs.python3 ];
    spec = builtins.toJSON spec;
    passAsFile = [ "spec" ];
  }
  ''
    python3 -I ${./chain.py} "$specPath"
    touch $out
  ''
