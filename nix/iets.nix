{
  lib,
  rustPlatform,
  installShellFiles,
  gitMinimal,
  src,
}:

rustPlatform.buildRustPackage {
  pname = "iets";
  version = lib.fileContents "${src}/VERSION";
  inherit src;

  cargoHash = "sha256-EM2mV/cs9Isr8bgbe6QKgkDiYYmBLY9MyI8NWza/B2I=";

  nativeBuildInputs = [ installShellFiles ];

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
