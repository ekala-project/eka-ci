{
  description = "Example flake for eka-ci checks";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      devShells.${system} = {
        # Default dev shell with basic tools
        default = pkgs.mkShell {
          packages = with pkgs; [
            coreutils
          ];
        };

        # Dev shell for formatting checks
        formatting = pkgs.mkShell {
          packages = with pkgs; [
            nixfmt-rfc-style
          ];
        };

        # Dev shell for network checks
        networking = pkgs.mkShell {
          packages = with pkgs; [
            curl
          ];
        };
      };
    };
}
