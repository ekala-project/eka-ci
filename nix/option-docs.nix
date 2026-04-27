{
  flake,
  lib,
  pkgs,
  runCommand,
  nixosOptionsDoc,
}:
let
  eval = lib.evalModules {
    modules = [
      (import ./module.nix flake)
      # Stub module providing NixOS options that our module references
      {
        options = {
          networking.firewall.allowedTCPPorts = lib.mkOption {
            type = lib.types.listOf lib.types.port;
            default = [ ];
          };
          nixpkgs.overlays = lib.mkOption {
            type = lib.types.listOf lib.types.raw;
            default = [ ];
          };
          users.users = lib.mkOption {
            type = lib.types.attrsOf lib.types.anything;
            default = { };
          };
          users.groups = lib.mkOption {
            type = lib.types.attrsOf lib.types.anything;
            default = { };
          };
          systemd.services = lib.mkOption {
            type = lib.types.attrsOf lib.types.anything;
            default = { };
          };
          warnings = lib.mkOption {
            type = lib.types.listOf lib.types.str;
            default = [ ];
          };
        };
      }
    ];
    specialArgs = {
      inherit pkgs;
    };
  };

  optionsDoc = nixosOptionsDoc {
    options = eval.options.services.eka-ci;
    warningsAreErrors = false;
    transformOptions =
      opt:
      opt
      // {
        # Strip the leading path to show relative file locations
        declarations = map (
          decl:
          let
            declStr = toString decl;
          in
          if lib.hasPrefix (toString ./.) declStr then lib.removePrefix (toString ./. + "/") declStr else decl
        ) opt.declarations;
      };
  };
in
runCommand "eka-ci-module-docs" { } ''
  mkdir -p $out
  cat > $out/nixos-module-reference.md <<'HEADER'
  # NixOS Module Reference

  This page is auto-generated from the NixOS module options schema. For a
  user-friendly guide, see [NixOS Module](./nixos-module.md).

  HEADER
  cat ${optionsDoc.optionsCommonMark} >> $out/nixos-module-reference.md
''
