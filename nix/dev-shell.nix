{
  lib,
  stdenv,
  cargo,
  clippy,
  elmPackages,
  nix-eval-jobs,
  mkShell,
  openssl,
  pkg-config,
  rustc,
  rustfmt,
  rust-analyzer,
  rustPlatform,
  sqlite,
  dev-server,
  fenix,
}:

mkShell {
  RUST_SRC_PATH = "${rustPlatform.rustcSrc}/library";
  nativeBuildInputs = [
    (fenix.default.withComponents [
      "cargo"
      "clippy"
      "rust-std"
      "rustc"
      "rustfmt-preview"
    ])
    pkg-config
    nix-eval-jobs
    rust-analyzer
    elmPackages.elm
    dev-server
    sqlite
  ]
  ++ lib.optionals stdenv.isLinux [
    # Broken in nixpkgs for darwin?
    elmPackages.elm-format
  ];

  buildInputs = [
    openssl
  ];
}
