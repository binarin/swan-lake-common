{ ... }: {
  perSystem = { pkgs, ... }: {
    devShells.default = pkgs.mkShell {
      name = "swan-lake-common devshell";
    };
  };
}
