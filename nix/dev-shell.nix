{
  cargo,
  clippy,
  elmPackages,
  mkShell,
  openssl,
  pkg-config,
  rustc,
  rustfmt,
  dev-server,
}:

mkShell {
  nativeBuildInputs = [
    cargo
    clippy
    pkg-config
    rustc
    rustfmt
    elmPackages.elm
    elmPackages.elm-format
    dev-server
  ];

  buildInputs = [
    openssl
  ];
}
