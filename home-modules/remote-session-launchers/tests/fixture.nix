# Golden matrix; reserved example hosts only.
{
  programs.remoteSessionLaunchers = {
    enable = true;
    tags = {
      dev = {
        background = "001800";
        titleSuffix = " [dev]";
      };
      prod = {
        background = "180000";
        titlePrefix = "[PROD] ";
      };
    };
    items = {
      dev-shell = {
        name = "dev shell";
        host = "dev.example.com";
        protocol = "ssh";
        remote.shell = { };
        tag = "dev";
      };
      dev-tmux = {
        name = "dev tmux";
        host = "dev.example.com";
        protocol = "ssh";
        remote.tmux.session = "work";
        tag = "dev";
      };
      dev-herdr = {
        name = "dev herdr";
        host = "dev.example.com";
        protocol = "ssh";
        remote.herdr = { };
      };
      prod-btop = {
        name = "prod btop";
        host = "admin@prod.example.net";
        protocol = "ssh";
        protocolArgs = [
          "-p"
          "2222"
        ];
        remote.exec = [ "btop" ];
        tag = "prod";
      };
      prod-hello = {
        name = ''prod "hello" \ nix run'';
        host = "prod.example.net";
        protocol = "ssh";
        remote.nixRun = {
          package = "hello";
          args = [
            "--greeting"
            "hi there"
          ];
        };
        tag = "prod";
        background = "101010";
        titlePrefix = "[P] ";
      };
      box-shell = {
        name = "box mosh";
        host = "box.example.org";
        protocol = "mosh";
        remote.shell = { };
        title = "box";
      };
      box-tmux = {
        name = "box mosh tmux";
        host = "box.example.org";
        protocol = "mosh";
        protocolArgs = [ "--predict=always" ];
        remote.tmux = {
          session = "main";
          detachOthers = false;
        };
        tag = "dev";
        titleSuffix = "";
      };
      box-hello = {
        name = "box nix run";
        host = "box.example.org";
        protocol = "mosh";
        remote.nixRun = {
          package = "hello";
          args = [
            "it's"
            ""
          ];
          flake = "github:example/flake/main";
        };
      };
    };
  };
}
