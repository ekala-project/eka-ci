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
  dev-server,
}:

mkShell {
  nativeBuildInputs =
    [
      cargo
      clippy
      pkg-config
      nix-eval-jobs
      rustc
      rustfmt
      rust-analyzer
      elmPackages.elm
      dev-server
    ]
    ++ lib.optionals stdenv.isLinux [
      # Broken in nixpkgs for darwin?
      elmPackages.elm-format
    ];

  buildInputs = [
    openssl
  ];
}
