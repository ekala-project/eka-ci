{
  git,
  mktemp,
  mprocs,
  caddy,
  cargo,
  watchexec,
  elmPackages,
  live-server,
  writeShellApplication,
  writeText,
}:

let
  caddyfile = writeText "config.caddyfile" ''
    http://localhost:8080

    handle_path /api/* {
      reverse_proxy :3030
    }

    handle {
      reverse_proxy :3031
    }
  '';

in
writeShellApplication {
  name = "dev-server";

  runtimeInputs = [
    git
    mktemp
    mprocs
    caddy
    cargo
    watchexec
    elmPackages.elm
    live-server
  ];

  text = ''
    BASE_DIR=$(git rev-parse --show-toplevel)

    TEMP=$(mktemp -d)

    mprocs \
      "XDG_DATA_HOME=$TEMP XDG_CONFIG_HOME=$TEMP caddy run --config ${caddyfile}" \
      "XDG_RUNTIME_DIR=$TEMP cargo run --manifest-path $BASE_DIR/backend/server/Cargo.toml" \
      "watchexec --watch $BASE_DIR/frontend/src --workdir $BASE_DIR/frontend --timings elm make src/Main.elm --output static/main.js" \
      "live-server --port 3031 $BASE_DIR/frontend/static"
  '';
}
