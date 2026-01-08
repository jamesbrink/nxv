# Attribute position extraction for nxv indexer
# This file is included at compile time via include_str!()
# Returns an empty list on any evaluation errors for resilience
# against older nixpkgs commits that may have evaluation issues.
{ nixpkgsPath, system }:
let
  pkgsResult = builtins.tryEval (import nixpkgsPath {
    system = system;
    config = {
      allowUnfree = true;
      allowBroken = true;
      allowInsecure = true;
      allowUnsupportedSystem = true;
    };
  });
  pkgs = if pkgsResult.success && builtins.isAttrs pkgsResult.value
         then pkgsResult.value
         else {};

  # Known package sets that contain nested derivations
  nestedPackageSets = [
    "qt5" "qt6" "libsForQt5" "kdePackages"
    "python3Packages" "python311Packages" "python312Packages" "python313Packages"
    "perlPackages" "rubyPackages" "rubyPackages_3_1" "rubyPackages_3_2" "rubyPackages_3_3"
    "luaPackages" "lua51Packages" "lua52Packages" "lua53Packages" "luajitPackages"
    "nodePackages" "nodePackages_latest"
    "haskellPackages"
    "ocamlPackages"
    "elmPackages"
    "rPackages"
    "emacsPackages"
    "vimPlugins"
    "gnome" "pantheon" "mate" "cinnamon" "xfce"
    "phpPackages" "php81Packages" "php82Packages" "php83Packages"
    "rustPackages"
    "goPackages"
    "texlive"
  ];

  # Get attr names - this should always succeed if import succeeded
  attrNamesRes = builtins.tryEval (
    if builtins.isAttrs pkgs then builtins.attrNames pkgs else []
  );
  attrNames = if attrNamesRes.success then attrNamesRes.value else [];

  # Get position for each attr - force full evaluation with seq
  getPos = name:
    let
      result = builtins.tryEval (
        let
          pos = builtins.unsafeGetAttrPos name pkgs;
          file = if pos == null then null else if pos ? file then pos.file else null;
          # Force evaluation of file (if it's a string, seq will evaluate it)
          forcedFile = builtins.seq file file;
        in { attrPath = name; file = forcedFile; }
      );
      # Also force the result record itself
      forced = if result.success
        then builtins.tryEval (builtins.seq result.value.attrPath (builtins.seq result.value.file result.value))
        else { success = false; };
    in if forced.success then forced.value else null;

  # Get positions for nested package set attributes
  getNestedPos = prefix: attrSet:
    let
      result = builtins.tryEval (
        if builtins.isAttrs attrSet then
          let
            nestedNames = builtins.tryEval (builtins.attrNames attrSet);
          in
            if nestedNames.success then
              builtins.concatMap (nestedName:
                let
                  fullPath = "${prefix}.${nestedName}";
                  posResult = builtins.tryEval (
                    let
                      pos = builtins.unsafeGetAttrPos nestedName attrSet;
                      file = if pos == null then null else if pos ? file then pos.file else null;
                      forcedFile = builtins.seq file file;
                    in { attrPath = fullPath; file = forcedFile; }
                  );
                  forced = if posResult.success
                    then builtins.tryEval (builtins.seq posResult.value.attrPath (builtins.seq posResult.value.file posResult.value))
                    else { success = false; };
                in
                  if forced.success && forced.value != null then [forced.value] else []
              ) nestedNames.value
            else []
        else []
      );
    in if result.success then result.value else [];

  # Top-level positions
  topLevelPositions = builtins.concatMap (name:
    let
      safeResult = builtins.tryEval (
        let
          pos = getPos name;
          forced = builtins.deepSeq pos pos;
        in if forced != null then [forced] else []
      );
      extracted = builtins.tryEval (
        builtins.seq safeResult.success (
          if safeResult.success then safeResult.value else []
        )
      );
    in if extracted.success then extracted.value else []
  ) attrNames;

  # Nested package set positions
  nestedPositions = builtins.concatMap (setName:
    let
      exists = builtins.hasAttr setName pkgs;
      valueResult = if exists then builtins.tryEval pkgs.${setName} else { success = false; };
      value = if valueResult.success then valueResult.value else null;
      nested = if value != null && builtins.isAttrs value
               then getNestedPos setName value
               else [];
      forcedNested = builtins.tryEval (builtins.deepSeq nested nested);
    in
      if forcedNested.success then forcedNested.value else []
  ) nestedPackageSets;

in
  topLevelPositions ++ nestedPositions
