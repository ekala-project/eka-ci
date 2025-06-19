flake: pkgs:
let
  nixosLib = import (pkgs.path + "/nixos/lib") { };
in
(nixosLib.runTest {
  hostPkgs = pkgs;
  defaults.documentation.enable = pkgs.lib.mkDefault false;
  imports = [
    {
      name = "eka-ci";
      nodes = {
        machine =
          { config, pkgs, ... }:
          {
            imports = [ flake.nixosModules.daemon ];
            services.anduril.eka-ci.enable = true;
          };
      };
      testScript = ''
        start_all()
        machine.wait_for_unit("eka-ci")
        try:
            machine.succeed("${pkgs.eka-ci}/bin/ekaci -s /run/eka-ci/ekaci/ekaci.socket info")
        except Exception:
            # If ping fails, dump service logs for debugging
            machine.execute("journalctl -u eka-ci -b > /tmp/service.log")
            machine.execute("cat /tmp/service.log")
            raise
      '';
    }
  ];
}).config.result
