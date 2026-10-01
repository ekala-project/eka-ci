{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  protobuf,
  openssl,
}:

let
  backendDir = ../backend;
in
rustPlatform.buildRustPackage {
  pname = "eka-ci";
  version = (lib.importTOML (backendDir + "/server/Cargo.toml")).package.version;

  src = lib.fileset.toSource {
    root = backendDir;
    fileset = lib.fileset.difference backendDir (lib.fileset.maybeMissing (backendDir + "/target"));
  };

  cargoLock = {
    lockFile = backendDir + "/Cargo.lock";
    outputHashes = {
      "harmonia-file-core-3.3.0" = "sha256-HhtISw8rZs1aScJiMGWZrr2tuaDQea/sKskb/kH2k1E=";
    };
  };

  nativeBuildInputs = [
    pkg-config
    protobuf
  ];

  buildInputs = [
    openssl
  ];

  env = {
    OPENSSL_NO_VENDOR = "1";
  }
  // lib.optionalAttrs (stdenv.hostPlatform.isLinux && stdenv.hostPlatform.isx86_64) {
    RUSTFLAGS = "-C linker-features=-lld";
  };

  # This causes the build to occur again, but in debug mode
  doCheck = false;
}
