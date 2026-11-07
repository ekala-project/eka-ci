{ iets-src }:
final: prev: {
  eka-ci = final.callPackage ./eka-ci.nix { };
  iets = final.callPackage ./iets.nix { src = iets-src; };
}
