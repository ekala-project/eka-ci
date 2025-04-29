{
  description = "EkaCI flake";

  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
    }: let
      localOverlay = import ./nix/overlay.nix;
    in
    utils.lib.eachDefaultSystem (system: rec {
      legacyPackages = import nixpkgs {
        inherit system;
        overlays = [
          localOverlay
          (final: prev: {
            dev-server = final.callPackage ./nix/dev-server.nix { };
            dev-shell = final.callPackage ./nix/dev-shell.nix { };
          })
        ];
      };

      packages.default = legacyPackages.eka-ci;
      devShells.default = legacyPackages.dev-shell;
      formatter = legacyPackages.nixfmt-rfc-style;
    }) // {
      overlays.default = localOverlay;
    };
}
