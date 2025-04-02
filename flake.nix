{
  description = "Eka-ci flake";

  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
  };

  outputs = { self, nixpkgs, utils }:
    let
      pkgsForSystem = system: import nixpkgs {
        inherit system;
      };
    in utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ] (system: rec {
      legacyPackages = pkgsForSystem system;
      devShells.default = with legacyPackages; mkShell {
        nativeBuildInputs = [
          cargo
          clippy
          rustc
          rustfmt
        ];
      };
  });
}
