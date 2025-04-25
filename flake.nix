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
      legacyPackages = nixpkgs.legacyPackages.${system}.extend (
        final: prev: {
          dev-server = prev.callPackage ./nix/dev-server.nix { };
          dev-shell = prev.callPackage ./nix/dev-shell.nix { };
        }
      );

      devShells.default = legacyPackages.dev-shell;
      formatter = legacyPackages.nixfmt-rfc-style;
    });
}
