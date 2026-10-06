{
  lib,
  stdenv,
  rustPlatform,
  pkg-config,
  protobuf,
  openssl,
  elmPackages,
}:

let
  backendDir = ../backend;
  frontendDir = ../frontend;

  frontend = stdenv.mkDerivation {
    pname = "eka-ci-frontend";
    version = "0.1.0";

    src = lib.fileset.toSource {
      root = frontendDir;
      fileset = lib.fileset.unions [
        (frontendDir + "/elm.json")
        (frontendDir + "/src")
        (frontendDir + "/static")
      ];
    };

    nativeBuildInputs = [ elmPackages.elm ];

    configurePhase = elmPackages.fetchElmDeps {
      elmPackages = import (frontendDir + "/elm-srcs.nix");
      elmVersion = elmPackages.elm.version;
      registryDat = frontendDir + "/registry.dat";
    };

    buildPhase = ''
      elm make src/Main.elm --optimize --output=static/main.js
    '';

    installPhase = ''
      cp -r static $out
    '';
  };
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

  postInstall = ''
    mkdir -p $out/share/eka-ci
    ln -s ${frontend} $out/share/eka-ci/static
  '';

  env = {
    OPENSSL_NO_VENDOR = "1";
  }
  // lib.optionalAttrs (stdenv.hostPlatform.isLinux && stdenv.hostPlatform.isx86_64) {
    RUSTFLAGS = "-C linker-features=-lld";
  };

  # This causes the build to occur again, but in debug mode
  doCheck = false;
}
