{
  cargo,
  clippy,
  elmPackages,
  mkShell,
  openssl,
  pkg-config,
  rustc,
  rustfmt,
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
  ];

  buildInputs = [
    openssl
  ];
}
