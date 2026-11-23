{ ... }: {
  perSystem = { pkgs, self', ... }: {
    devShells.default = pkgs.mkShell {
      name = "swan-lake-common devshell";
      packages = [
        pkgs.python3
        self'.packages.common-workflow
        self'.packages.secretctl
        self'.packages.swl
      ];
    };
  };
}
