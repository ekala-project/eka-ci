fenix: final: prev: {
  eka-ci =
    let
      toolchain = fenix.packages.${final.stdenv.buildPlatform.system}.stable.minimalToolchain;
    in
    final.callPackage ./eka-ci.nix {
      rustPlatform = final.makeRustPlatform {
        cargo = toolchain;
        rustc = toolchain;
      };
    };
}
