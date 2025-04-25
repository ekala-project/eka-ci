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
    let
      pkgsForSystem =
        system:
        import nixpkgs {
          inherit system;
        };
    in
    utils.lib.eachDefaultSystem (system: rec {
      legacyPackages = pkgsForSystem system;
      devShells.default = legacyPackages.callPackage ./nix/dev-shell.nix { };
      formatter = legacyPackages.nixfmt-rfc-style;
    });
}
