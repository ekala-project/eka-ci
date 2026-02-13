{
  pkgs ? import <nixpkgs> { },
}:

{
  hello = pkgs.hello;
}
