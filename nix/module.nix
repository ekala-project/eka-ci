flake:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    mkEnableOption
    mkPackageOption
    mkOption
    ;

  cfg = config.services.anduril.eka-ci;
  settingsFormat = pkgs.formats.toml { };
in
{
  options.services.anduril.eka-ci = {
    enable = mkEnableOption "Eka CI";

    package = mkPackageOption pkgs "eka-ci" { };

    settings = mkOption {
      type = settingsFormat.type;
      default = { };
      # TODO: uncomment when docs are available online
      # description = ''
      #   Specify the configuration for Eka CI in Nix.

      #   See <https://doesntexistyet.com/eka-ci/docs> for available options.
      # '';
    };
  };
  config = {
    # avoid having to hardcode the package from this flake and its associated nixpkgs
    nixpkgs.overlays = [ flake.overlays.default ];

    systemd.services.eka-ci = {
      description = "Eka CI backend Service Daemon";
      wantedBy = [ "multi-user.target" ];

      serviceConfig =
        let
          conf = settingsFormat.generate "ekaci.toml" cfg.settings;
        in
        {
          ExecStart = "${cfg.package}/bin/eka_ci_server --config-file ${conf}";
          DynamicUser = true;
          Restart = "always";
          ProtectSystem = "full";
          DevicePolicy = "closed";
          NoNewPrivileges = true;
          WorkingDirectory = "%S/%p";
          StateDirectory = "%p";
          RuntimeDirectory = "%p";
          RuntimeDirectoryMode = "0700";
          Environment = [
            "XDG_RUNTIME_DIR=/run/%p"
            "XDG_DATA_HOME=/var/lib/%p"
          ];
        };
    };
  };
}
