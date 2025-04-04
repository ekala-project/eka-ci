{
  description = "Eka-ci flake";

  inputs = {
    utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";
  };

  outputs = { self, nixpkgs, utils }:
    let
      pkgsForSystem = system: import nixpkgs {
        inherit system;
      };
    in utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ] (system: rec {
      legacyPackages = pkgsForSystem system;
      devShells.default = with legacyPackages; mkShell {
        nativeBuildInputs = [
          cargo
          clippy
          rustc
          rustfmt
          elmPackages.elm
          elmPackages.elm-format

          watchexec
          live-server
        ];

        shellHook = ''
          elm_up() {
            repo_root=$(git rev-parse --show-toplevel)
            pushd "$repo_root/frontend"

            watchexec --shell=none --watch=./src --exts=elm --log-file frontend.logs 2>&1 -- elm make ./src/Main.elm --output ./static/main.js &
            export WATCH_PID="$!"

            live-server --open ./static &
            export LIVE_PID="$!"

            popd
          }

          elm_down() {
            [ -n WATCH_PID ] && echo "Stopping watchexec" && kill "$WATCH_PID"
            [ -n LIVE_PID ] && echo "Stopping live-server" && kill "$LIVE_PID"
          }
        '';
      };
  });
}
