{
  lib,
  rustPlatform,
  pkg-config,
  openssl,
}:

rustPlatform.buildRustPackage {
  pname = "eka-ci";
  version =
    let
      server_toml = builtins.readFile ../backend/server/Cargo.toml;
      server_info = builtins.fromTOML server_toml;
    in
    server_info.package.version;

  cargoLock.lockFile = ../backend/Cargo.lock;
  src = ../backend;

  nativeBuildInputs = [
    pkg-config
  ];

  buildInputs = [
    openssl
  ];

  # This causes the build to occur again, but in debug mode
  doCheck = false;
}
