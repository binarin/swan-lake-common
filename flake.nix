{
  description = "Shared parts of my infra / personal configs";

  inputs = {
    flake-parts.url = "github:binarin/flake-parts/swan-lake-current";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";

    nixpkgs.url = "github:binarin/nixpkgs/swan-lake-current/nixos-26.05";
    nixpkgs-unstable.url = "github:binarin/nixpkgs/swan-lake-current/nixpkgs-unstable";

    import-tree.url = "github:binarin/import-tree/swan-lake-current";

    disko.url = "github:binarin/disko/swan-lake-current";
    disko.inputs.nixpkgs.follows = "nixpkgs";

    home-manager.url = "github:binarin/home-manager/swan-lake-current/release-26.05";
    home-manager.inputs.nixpkgs.follows = "nixpkgs";

    sops-nix.url = "github:binarin/sops-nix/swan-lake-current";
    sops-nix.inputs.nixpkgs.follows = "nixpkgs";

    emacs-niri-awareness.url = "github:binarin/emacs-niri-awareness";
    emacs-niri-awareness.flake = false;

    emacs-overlay.url = "github:binarin/emacs-overlay/swan-lake-current";
    emacs-overlay.inputs.nixpkgs.follows = "nixpkgs";
    emacs-overlay.inputs.nixpkgs-stable.follows = "nixpkgs";

    emacs-tramp-rpc.url = "github:binarin/emacs-tramp-rpc/swan-lake-current";
    emacs-tramp-rpc.flake = false;

    zwanenzang.url = "git+https://forgejo.lynx-lizard.ts.net/binarin/zwanenzang";
    zwanenzang.inputs.flake-parts.follows = "flake-parts";
    zwanenzang.inputs.nixpkgs.follows = "nixpkgs";
    zwanenzang.inputs.nixpkgs-unstable.follows = "nixpkgs-unstable";
    zwanenzang.inputs.import-tree.follows = "import-tree";
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } (
      { self, ... }: {
        imports = [
          (inputs.import-tree ./modules)
        ];
        systems = [ "x86_64-linux" ];
      }
    );
}
