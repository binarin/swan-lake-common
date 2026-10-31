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

  emacsPackageFn =
    {
      emacsPackagesFor,
      basePackage,
    }:
    (emacsPackagesFor basePackage).emacsWithPackages (
      e: with e; [
        (tramp-rpc-builder e)
        consult
        corfu
        vertico
        orderless
        marginalia
        embark
        embark-consult
        magit
        ws-butler
        zenburn-theme
        treesit-grammars.with-all-grammars
      ]
    );

  emacsPackageEnv =
    {
      buildEnv,
      fd,
      ripgrep,
      finalEmacsBinary,
    }:
    buildEnv {
      name = "emacs-from-swan-lake-common";
      paths = [
        fd
        ripgrep
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
