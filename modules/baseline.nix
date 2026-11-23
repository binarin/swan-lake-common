{ self, inputs, ... }:
let
  commonSelf = self;
in
{
  imports = [
    inputs.flake-parts.flakeModules.flakeModules
  ];

  flake.flakeModules.default = { ... }: {
    key = "swan-lake-common.modules.flake.default";
    imports = [
      commonSelf.flakeModules.configured-pkgs
      commonSelf.flakeModules.common-workflow
      commonSelf.flakeModules.swl
      inputs.flake-parts.flakeModules.flakeModules
    ];
  };
}
