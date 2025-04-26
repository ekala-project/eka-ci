{
  cargo,
  clippy,
  elmPackages,
  mkShell,
  openssl,
  pkg-config,
  rustc,
  rustfmt,
  rust-analyzer,
  dev-server,
}:

mkShell {
  nativeBuildInputs = [
    cargo
    clippy
    pkg-config
    rustc
    rustfmt
    rust-analyzer
    elmPackages.elm
    elmPackages.elm-format
    dev-server
  ];

  buildInputs = [
    openssl
  ];
}
