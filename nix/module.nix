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
#               # Logging configuration
#               logging.level = "nxv=info,tower_http=info,warn";
#               logging.format = "json";  # or "text" (default)
#               # Database concurrency limits (prevents resource exhaustion)
#               database.maxConnections = 32;  # Max concurrent DB operations
#               database.timeoutSeconds = 30;  # Timeout for DB operations
#             };
#           }
#         ];
#       };
#     };
#   }

# The module takes an optional flakePackages argument that is passed from
# the flake. This is a function from system to package, allowing the module
# to work without requiring the overlay.
{ flakePackages ? null }:

{ config, lib, pkgs, ... }:

let
  cfg = config.services.nxv;
  inherit (lib) mkEnableOption mkOption mkIf types;

  # Use the package from the flake if provided, otherwise fall back to pkgs.nxv
  defaultPkg =
    if flakePackages != null && flakePackages ? ${pkgs.system}
    then flakePackages.${pkgs.system}.nxv
    else pkgs.nxv or (throw "nxv package not found. Either use the nxv overlay or set services.nxv.package.");
in
{
  options.services.nxv = {
    enable = mkEnableOption "nxv API server for querying Nix package versions";

    package = mkOption {
      type = types.package;
      default = defaultPkg;
      defaultText = lib.literalExpression "pkgs.nxv";
      description = "The nxv package to use.";
    };

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

    manifestUrl = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "https://example.com/nxv/manifest.json";
      description = ''
        Custom manifest URL for index downloads. If null, uses the default
        GitHub releases URL. Useful for self-hosted index mirrors.
      '';
    };

    publicKey = mkOption {
      type = types.nullOr (types.either types.str types.path);
      default = null;
      example = "/etc/nxv/signing-key.pub";
      description = ''
        Custom public key for manifest signature verification. Required when
        using a self-hosted index signed with your own key. Can be a path to
        a .pub file or the raw key string (RW...).
      '';
    };

    skipVerify = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Skip manifest signature verification. INSECURE - only use for
        development or testing with unsigned manifests.
      '';
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

    logging = {
      level = mkOption {
        type = types.str;
        default = "nxv=info,tower_http=info,warn";
        example = "nxv=debug,tower_http=debug,info";
        description = ''
          Log level configuration using RUST_LOG syntax.
          Common patterns:
          - "nxv=info,tower_http=info,warn" (default)
          - "nxv=debug,tower_http=debug,info" (verbose)
          - "nxv=trace,tower_http=trace,debug" (very verbose)
        '';
      };

      format = mkOption {
        type = types.enum [ "text" "json" ];
        default = "text";
        description = ''
          Log output format.
          - "text": Human-readable format for development and debugging.
          - "json": Structured JSON format for log aggregation systems.
        '';
      };
    };

    database = {
      maxConnections = mkOption {
        type = types.int;
        default = 32;
        description = ''
          Maximum number of concurrent database operations.
          This limits file descriptor usage and prevents spawn_blocking pool
          exhaustion under heavy load. Increase for high-traffic deployments
          with sufficient system resources.
        '';
      };

      timeoutSeconds = mkOption {
        type = types.int;
        default = 30;
        description = ''
          Timeout for database operations in seconds.
          Operations exceeding this timeout will return HTTP 504 Gateway Timeout.
          Increase if you have a very large database or slow storage.
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
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];

      environment = {
        RUST_LOG = cfg.logging.level;
        NXV_MAX_DB_CONNECTIONS = toString cfg.database.maxConnections;
        NXV_DB_TIMEOUT_SECS = toString cfg.database.timeoutSeconds;
      } // lib.optionalAttrs (cfg.logging.format == "json") {
        NXV_LOG_FORMAT = "json";
      };

      serviceConfig = let
        manifestArgs = lib.optionalString (cfg.manifestUrl != null)
          "--manifest-url ${cfg.manifestUrl}";
        publicKeyArgs = lib.optionalString (cfg.publicKey != null)
          "--public-key ${toString cfg.publicKey}";
        skipVerifyArgs = lib.optionalString cfg.skipVerify
          "--skip-verify";
        updateArgs = lib.concatStringsSep " " (lib.filter (s: s != "") [
          manifestArgs publicKeyArgs skipVerifyArgs
        ]);
        corsArgs =
          if cfg.cors.origins != null then
            "--cors-origins ${lib.concatStringsSep "," cfg.cors.origins}"
          else if cfg.cors.enable then
            "--cors"
          else
            "";
      in {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;
        ExecStartPre = pkgs.writeShellScript "nxv-bootstrap" ''
          if [ ! -f "${cfg.dataDir}/index.db" ]; then
            echo "Database not found, downloading index..."
            ${cfg.package}/bin/nxv --db-path ${cfg.dataDir}/index.db update ${updateArgs}
          fi
        '';
        ExecStart = ''
          ${cfg.package}/bin/nxv \
            --db-path ${cfg.dataDir}/index.db \
            serve \
            --host ${cfg.host} \
            --port ${toString cfg.port} \
            ${corsArgs}
        '';
        Restart = "on-failure";
        RestartSec = "5s";

        # Resource limits
        LimitNOFILE = 65536;  # Increase file descriptor limit for high concurrency

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
    };

    # Automatic update service and timer
    systemd.services.nxv-update = mkIf cfg.autoUpdate.enable {
      description = "Update nxv package index";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];

      serviceConfig = let
        manifestArgs = lib.optionalString (cfg.manifestUrl != null)
          "--manifest-url ${cfg.manifestUrl}";
        publicKeyArgs = lib.optionalString (cfg.publicKey != null)
          "--public-key ${toString cfg.publicKey}";
        skipVerifyArgs = lib.optionalString cfg.skipVerify
          "--skip-verify";
        updateArgs = lib.concatStringsSep " " (lib.filter (s: s != "") [
          manifestArgs publicKeyArgs skipVerifyArgs
        ]);
      in {
        Type = "oneshot";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = "${cfg.package}/bin/nxv --db-path ${cfg.dataDir}/index.db update ${updateArgs}";

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
