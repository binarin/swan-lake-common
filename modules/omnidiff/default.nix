{
  inputs,
  ...
}:
{
  perSystem = { pkgs, ... }: {
    # `src` cannot be left to the derivation as a fetch of its own: package.nix
    # gets it from the input, and callPackage would silently fill the argument
    # from pkgs (`src` is an alias of simple-revision-control there) if the
    # module ever stopped passing it.
    packages.omnidiff = pkgs.callPackage ./_package {
      src = inputs.omnidiff;
    };
  };
}
