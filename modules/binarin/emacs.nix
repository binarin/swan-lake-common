{ ... }:
let
  emacsPackageFn =
    {
      emacsPackagesFor,
      basePackage,
    }:
    (emacsPackagesFor basePackage).emacsWithPackages (
      e: with e; [
        consult
        corfu
        vertico
        orderless
        marginalia
        embark
        embark-consult
        magit
        ws-buttler
        zenburn-theme
        treesit-grammars.with-all-grammars
      ]
    );
in
{
  perSystem = { system, pkgs, ... }: {
    packages.emacs-nox = pkgs.callPackage emacsPackageFn { basePackage = pkgs.bleeding.emacs-nox; };
  };
}
