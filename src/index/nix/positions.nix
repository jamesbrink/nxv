# Attribute position extraction for nxv indexer
# This file is included at compile time via include_str!()
# Returns an empty list on any evaluation errors for resilience
# against older nixpkgs commits that may have evaluation issues.
{ nixpkgsPath, system }:
let
  pkgs = import nixpkgsPath {
    system = system;
    config = {
      allowUnfree = true;
      allowBroken = true;
      allowInsecure = true;
      allowUnsupportedSystem = true;
    };
  };
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
in
  # Wrap entire lambda body in tryEval, returning list directly to avoid field access issues
  builtins.concatMap (name:
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
  ) attrNames
