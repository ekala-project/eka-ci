final: prev: {
  eka-ci = final.callPackage ./eka-ci.nix { };
  iets = final.callPackage ./iets.nix { };
}
