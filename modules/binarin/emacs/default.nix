{ inputs, lib, ... }:
let
  tramp-rpc-builder =
    epkgs:
    epkgs.melpaBuild {
      pname = "tramp-rpc";
      version = "0.0.0";
      src = inputs.emacs-tramp-rpc;
      packageRequires = [ epkgs.msgpack ];
    };

  # zwanenzang-builder = epkgs:
  #   epkgs.trivialBuild  {
  #     pname = "zwanenzang";
  #     version = "0.0.0";
  #     src = "${inputs.zwanenzang}/emacs";
  #   };

  # zwanenzang-builder = import "${inputs.zwanenzang}/nix/_packages/zwanenzang-el.nix";
  
  emacsPackageFn =
    {
      emacsPackagesFor,
      basePackage,
    }:
    (emacsPackagesFor basePackage).emacsWithPackages (
      e: with e; [
        (tramp-rpc-builder e)
        # (zwanenzang-builder e)
        zwanenzang
        batppuccin
        zenburn-theme
        consult
        corfu
        vertico
        orderless
        marginalia
        embark
        embark-consult
        magit
        ws-butler
        nix-mode
        direnv
        ghostel


        treesit-grammars.with-all-grammars
      ]
    );

  emacsPackageEnv =
    {
      buildEnv,
      fd,
      ripgrep,
      zwanenzang,
      finalEmacsBinary,
    }:
    buildEnv {
      name = "emacs-from-swan-lake-common";
      paths = [
        fd
        ripgrep
        zwanenzang
      ];
      postBuild = ''
        ln -sf "${lib.getExe finalEmacsBinary}" "$out/bin/emacs"
      '';
    };
in
{
  perSystem = { system, pkgs, ... }: {
    packages.emacs-nox = pkgs.callPackage emacsPackageFn { basePackage = pkgs.bleeding.emacs-nox; };
    packages.emacs = pkgs.callPackage emacsPackageFn { basePackage = pkgs.bleeding.emacs-pgtk; };
  };
}
