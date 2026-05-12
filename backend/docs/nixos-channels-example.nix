{ config, pkgs, ... }:

{
  # Example NixOS configuration for EkaCI release channels
  services.eka-ci = {
    enable = true;

    settings = {
      # ... other settings ...

      # Release channels configuration
      channels = [
        {
          # Stable release channel
          forge = "github";
          owner = "myorg";
          repo = "nixpkgs";
          name = "stable";
          tracking_branch = "master";
          target_branch = "ekapkgs-stable";
          required = [
            "coreutils"
            "bash"
            "gcc"
          ];
          packages = [
            "firefox"
            "chromium"
          ];
          dry_run = false;
        }

        {
          # Beta release channel with dry-run enabled
          forge = "github";
          owner = "myorg";
          repo = "nixpkgs";
          name = "beta";
          tracking_branch = "staging";
          target_branch = "ekapkgs-beta";
          required = [
            "coreutils"
            "bash"
          ];
          packages = [ ];
          dry_run = true;  # Test mode - evaluation only, no push
        }

        {
          # GitLab release channel example
          forge = "gitlab";
          owner = "mygroup";
          repo = "myproject";
          name = "production";
          tracking_branch = "main";
          target_branch = "production-packages";
          required = [
            "unit-tests"
            "integration-tests"
          ];
          packages = [
            "backend"
            "frontend"
            "database-migrations"
          ];
          dry_run = false;
        }
      ];

      # Required: GitHub Apps for authentication
      github_apps = [
        {
          app_id = 123456;
          owner = "myorg";
          private_key_file = "/run/secrets/github-app-key.pem";
        }
      ];
    };
  };
}
