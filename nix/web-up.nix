{
  writeShellApplication,
  writeText,
  bash,
  mktemp,
  mprocs,
  watchexec,
  live-server,
  caddy,
  stdenv,
  cargo,
  rustc,
  lib,
  elmPackages,
}:
let
  mkPathArray = paths: map (path: path + "/bin") paths;
  caddyConfig = writeText "local.caddyfile" ''
    {
      debug
      http_port 3030
      default_bind 127.0.0.1
      storage file_system {$XDG_RUNTIME_DIR}/caddy
      admin off
      persist_config off
      auto_https off
    }
    localhost:3030 {
      reverse_proxy /api/* 127.0.0.1:28164
      reverse_proxy /* 127.0.0.1:35249
    }
  '';
  mprocsConfig = writeText "mprocs.yaml" (
    lib.generators.toYAML { } {
      procs = {
        cargo = {
          cmd = [
            "${cargo}/bin/cargo"
            "run"
            "--manifest-path=./backend/Cargo.toml"
            "--package=eka_ci_server"
            "--"
            "--port=28164"
          ];
          env = {
            RUST_LOG = "debug";
          };
          add_path = mkPathArray [
            stdenv.cc
            rustc
          ];
        };
        watchexec = {
          cmd = [
            "${watchexec}/bin/watchexec"
            "--shell=none"
            "--workdir=./frontend"
            "--watch=./src"
            "--exts=elm"
            "--timings"
            "--"
            "${elmPackages.elm + "/bin/elm"}"
            "make"
            "./src/Main.elm"
            "--output=./static/main.js"
          ];
        };
        live-server = {
          cmd = [
            "${live-server}/bin/live-server"
            "--host=127.0.0.1"
            "--port=35249"
            "--hard"
            "./frontend/static"
          ];
          env = {
              RUST_LOG = "debug";
          };
        };
        caddy = {
          cmd = [
            "${caddy}/bin/caddy"
            "run"
            "--config=${caddyConfig}"
          ];
        };
      };
    }
  );
in
writeShellApplication {
  name = "web-up";
  runtimeInputs = [
    mktemp
    mprocs
  ];
  text = ''
    XDG_RUNTIME_DIR=$(mktemp -d)
    export XDG_RUNTIME_DIR
    mprocs --config ${mprocsConfig}
  '';
}
