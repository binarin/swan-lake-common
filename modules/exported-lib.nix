{
  inputs,
  self,
  lib,
  ...
}:
{
  options = {
    flake.lib = lib.mkOption {
      type = lib.types.lazyAttrsOf lib.types.raw;
      default = { };
      description = "Exported library helpers";
    };
  };
  config = {
    flake.lib.import-tree = inputs.import-tree;
  };
}
