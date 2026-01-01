# NixOS module for running the nxv API server as a systemd service.
#
# Example usage in a NixOS configuration:
#
#   {
#     inputs.nxv.url = "github:jamesbrink/nxv";
#
#     outputs = { self, nixpkgs, nxv }: {
#       nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
#         modules = [
#           nxv.nixosModules.default
#           {
#             services.nxv = {
#               enable = true;
#               host = "0.0.0.0";
#               port = 8080;
#               cors.enable = true;
#             };
#           }
#         ];
#       };
#     };
#   }

{ config, lib, pkgs, ... }:

let
  cfg = config.services.nxv;
  inherit (lib) mkEnableOption mkOption mkIf mkPackageOption types;
in
{
  options.services.nxv = {
    enable = mkEnableOption "nxv API server for querying Nix package versions";

    package = mkPackageOption pkgs "nxv" { };

    host = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = ''
        The host address to bind the API server to.
        Use "0.0.0.0" to listen on all interfaces.
      '';
    };

    port = mkOption {
      type = types.port;
      default = 8080;
      description = "The port to listen on.";
    };

    dataDir = mkOption {
      type = types.path;
      default = "/var/lib/nxv";
      description = ''
        Directory to store the nxv database and bloom filter.
        The service will look for index.db in this directory.
      '';
    };

    cors = {
      enable = mkEnableOption "CORS support for all origins";

      origins = mkOption {
        type = types.nullOr (types.listOf types.str);
        default = null;
        example = [ "https://example.com" "https://app.example.com" ];
        description = ''
          Specific CORS origins to allow. If set, only these origins
          will be permitted. If null and cors.enable is true, all
          origins are allowed.
        '';
      };
    };

    openFirewall = mkOption {
      type = types.bool;
      default = false;
      description = "Whether to open the firewall port for the nxv API server.";
    };

    user = mkOption {
      type = types.str;
      default = "nxv";
      description = "User account under which nxv runs.";
    };

    group = mkOption {
      type = types.str;
      default = "nxv";
      description = "Group under which nxv runs.";
    };

    autoUpdate = {
      enable = mkEnableOption "automatic index updates via systemd timer";

      interval = mkOption {
        type = types.str;
        default = "daily";
        example = "hourly";
        description = ''
          How often to update the index. This uses systemd calendar event syntax.
          Common values: "hourly", "daily", "weekly", or specific times like "Mon *-*-* 02:00:00".
        '';
      };
    };
  };

  config = mkIf cfg.enable {
    # Create user and group
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      home = cfg.dataDir;
      description = "nxv API server user";
    };

    users.groups.${cfg.group} = { };

    # Create data directory
    systemd.tmpfiles.rules = [
      "d ${cfg.dataDir} 0750 ${cfg.user} ${cfg.group} -"
    ];

    # Main API server service
    systemd.services.nxv = {
      description = "nxv API Server - Nix Package Version Search";
      after = [ "network.target" ];
      wantedBy = [ "multi-user.target" ];

      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = let
          corsArgs =
            if cfg.cors.origins != null then
              "--cors-origins ${lib.concatStringsSep "," cfg.cors.origins}"
            else if cfg.cors.enable then
              "--cors"
            else
              "";
        in ''
          ${cfg.package}/bin/nxv \
            --db-path ${cfg.dataDir}/index.db \
            serve \
            --host ${cfg.host} \
            --port ${toString cfg.port} \
            ${corsArgs}
        '';
        Restart = "on-failure";
        RestartSec = "5s";

        # Hardening options
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        MemoryDenyWriteExecute = true;
        LockPersonality = true;
        ReadWritePaths = [ cfg.dataDir ];
        CapabilityBoundingSet = "";
        SystemCallFilter = [ "@system-service" "~@privileged" ];
        SystemCallArchitectures = "native";
      };

      # Wait for the database to exist before starting
      preStart = ''
        if [ ! -f "${cfg.dataDir}/index.db" ]; then
          echo "Warning: Database not found at ${cfg.dataDir}/index.db"
          echo "Run 'nxv update' or copy an existing index.db to start the service."
        fi
      '';
    };

    # Automatic update service and timer
    systemd.services.nxv-update = mkIf cfg.autoUpdate.enable {
      description = "Update nxv package index";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      serviceConfig = {
        Type = "oneshot";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = "${cfg.package}/bin/nxv --db-path ${cfg.dataDir}/index.db update";

        # Hardening options
        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ReadWritePaths = [ cfg.dataDir ];
      };
    };

    systemd.timers.nxv-update = mkIf cfg.autoUpdate.enable {
      description = "Timer for nxv index updates";
      wantedBy = [ "timers.target" ];

      timerConfig = {
        OnCalendar = cfg.autoUpdate.interval;
        Persistent = true;
        RandomizedDelaySec = "5m";
      };
    };

    # Open firewall port if requested
    networking.firewall.allowedTCPPorts = mkIf cfg.openFirewall [ cfg.port ];
  };
}
