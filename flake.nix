{
  description = "EkaCI flake";

  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
    treefmt-nix.url = "github:numtide/treefmt-nix";
  };

  outputs =
    {
      self,
      nixpkgs,
      utils,
      treefmt-nix,
    }:
    let
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
      checks.module = import ./nix/module-test.nix self legacyPackages;
      formatter =
        let
          fmt = treefmt-nix.lib.evalModule legacyPackages {
            programs.rustfmt.enable = true;
            programs.elm-format.enable = true;
            programs.nixfmt.enable = true;
          };
        in
        fmt.config.build.wrapper;
    })
    // {
      overlays.default = localOverlay;
      nixosModules.daemon = import ./nix/module.nix self;
    };
}
