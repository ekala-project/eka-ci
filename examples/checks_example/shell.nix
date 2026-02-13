{
  pkgs ? import <nixpkgs> { },
}:

{
  default = pkgs.mkShell {
    packages = with pkgs; [
      coreutils
    ];
  };

  formatting = pkgs.mkShell {
    packages = with pkgs; [
      nixfmt-rfc-style
    ];
  };

  networking = pkgs.mkShell {
    packages = with pkgs; [
      curl
    ];
  };
}
