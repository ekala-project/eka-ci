{
  description = "Eka-ci flake";

  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
    }:
    utils.lib.eachDefaultSystem (system: rec {
      legacyPackages = import nixpkgs {
        inherit system;
        overlays = [
          (final: prev: {
            web-up = final.callPackage ./nix/web-up.nix { };
            dev-shell = final.callPackage ./nix/dev-shell.nix { };
          })
        ];
      };

      devShells.default = legacyPackages.dev-shell;
      formatter = legacyPackages.nixfmt-rfc-style;
      apps.web = {
        type = "app";
        program = "${legacyPackages.web-up}/bin/web-up";
      };
    });
}
