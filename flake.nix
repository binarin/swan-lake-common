{
  description = "Shared parts of my infra / personal configs";

  inputs = {
    flake-parts.url = "github:binarin/flake-parts/swan-lake-current";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";

    nixpkgs.url = "github:binarin/nixpkgs/swan-lake-current/nixos-26.05";
    nixpkgs-unstable.url = "github:binarin/nixpkgs/swan-lake-current/nixpkgs-unstable";

    import-tree.url = "github:binarin/import-tree/swan-lake-current";
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } (inputs.import-tree ./modules);
}
