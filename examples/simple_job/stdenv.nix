let
  nixpkgsSrc = builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/b2a3852bd078e68dd2b3dfa8c00c67af1f0a7d20.tar.gz";
    sha256 = "0lgg0bw6gnaa0sg45da65qzmhwrwjj7gsq2scfq8wcbv0bnc9xb9";
  };
  pkgs = import nixpkgsSrc { };
in
{
  inherit (pkgs) stdenv;
}
