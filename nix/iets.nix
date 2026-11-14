{
  lib,
  rustPlatform,
  installShellFiles,
  pkg-config,
  zstd,
  gitMinimal,
  # Defaults to the iets revision pinned in flake.lock, so the overlay works
  # without flakes (e.g. when eka-ci is a `flake = false` input).
  iets-src ? builtins.fetchTree (builtins.fromJSON (builtins.readFile ../flake.lock))
    .nodes.iets-src.locked,
}:

rustPlatform.buildRustPackage {
  pname = "iets";
  version = lib.fileContents "${iets-src}/VERSION";
  src = iets-src;

  cargoHash = "sha256-EM2mV/cs9Isr8bgbe6QKgkDiYYmBLY9MyI8NWza/B2I=";

  nativeBuildInputs = [
    installShellFiles
    pkg-config
  ];

  buildInputs = [ zstd ];

  env.IETS_FALLBACK_PATH = lib.makeBinPath [ gitMinimal ];

  postInstall = ''
    installShellCompletion \
      --bash src/cli/complete/iets.bash \
      --zsh src/cli/complete/iets.zsh \
      --fish src/cli/complete/iets.fish
  '';

  doCheck = false;

  meta = {
    description = "Fast Nix evaluator and remote-build client";
    license = lib.licenses.lgpl21Plus;
    mainProgram = "iets";
  };
}
