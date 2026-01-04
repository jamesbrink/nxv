# Package metadata extraction for nxv indexer
# This file is included at compile time via include_str!()
{ nixpkgsPath, system, attrNames ? null }:
let
  # Import nixpkgs with current system and permissive config
  pkgs = import nixpkgsPath {
    system = system;
    config = {
      allowUnfree = true;
      allowBroken = true;
      allowInsecure = true;
      allowUnsupportedSystem = true;
    };
  };

  # Force full evaluation and catch any errors - this is critical for lazy evaluation
  tryDeep = expr:
    let result = builtins.tryEval (builtins.deepSeq expr expr);
    in if result.success then result.value else null;

  # Safely extract a string field - converts integers/floats to strings
  safeString = x: tryDeep (
    if x == null then null
    else if builtins.isString x then x
    else if builtins.isInt x || builtins.isFloat x then builtins.toString x
    else null
  );

  # Safely get licenses - force evaluation of each license
  # Each element access is wrapped in tryEval to handle thunks that throw
  getLicenses = l: tryDeep (
    let
      extractOne = x:
        let
          result = builtins.tryEval (
            if builtins.isAttrs x then (x.spdxId or x.shortName or "unknown")
            else if builtins.isString x then x
            else if builtins.isInt x || builtins.isFloat x then builtins.toString x
            else "unknown"
          );
        in if result.success then result.value else "unknown";
    in
      if builtins.isList l then map extractOne l
      else [ (extractOne l) ]
  );

  # Safely get maintainers - force evaluation of each maintainer
  # Handle both list of maintainers and single string/maintainer
  # Each element access is wrapped in tryEval to handle thunks that throw
  getMaintainers = m: tryDeep (
    if m == null then null
    else if builtins.isString m then [ m ]
    else if builtins.isList m then map (x:
      let
        result = builtins.tryEval (
          if builtins.isAttrs x then (x.github or x.name or "unknown")
          else if builtins.isString x then x
          else if builtins.isInt x || builtins.isFloat x then builtins.toString x
          else "unknown"
        );
      in if result.success then result.value else "unknown"
    ) m
    else null
  );

  # Safely get platforms - force evaluation of each platform
  # Handle both list of platforms and single string/platform
  # Each element access is wrapped in tryEval to handle thunks that throw
  getPlatforms = p: tryDeep (
    if p == null then null
    else if builtins.isString p then [ p ]
    else if builtins.isList p then map (x:
      let
        result = builtins.tryEval (
          if builtins.isString x then x
          else if builtins.isAttrs x then (x.system or "unknown")
          else if builtins.isInt x || builtins.isFloat x then builtins.toString x
          else "unknown"
        );
      in if result.success then result.value else "unknown"
    ) p
    else null
  );

  # Safely get knownVulnerabilities - list of strings describing security issues
  # meta.knownVulnerabilities is a list of strings when present
  getKnownVulnerabilities = v: tryDeep (
    if v == null then null
    else if builtins.isList v then
      let
        extracted = map (x:
          let
            result = builtins.tryEval (
              if builtins.isString x then x
              else if builtins.isInt x || builtins.isFloat x then builtins.toString x
              else null
            );
          in if result.success then result.value else null
        ) v;
        # Filter out nulls
        filtered = builtins.filter (x: x != null) extracted;
      in if builtins.length filtered > 0 then filtered else null
    else null
  );

  # Check if something is a derivation (with error handling)
  isDerivation = x:
    let result = builtins.tryEval (builtins.isAttrs x && x ? type && x.type == "derivation");
    in result.success && result.value;

  # Convert any value to string safely
  toString' = x:
    if x == null then null
    else if builtins.isString x then x
    else builtins.toString x;

  # Get the source file path for a package from meta.position
  # meta.position format is "/nix/store/.../pkgs/path/file.nix:42" or "/path/to/nixpkgs/pkgs/path/file.nix:42"
  # We extract the relative path starting from "pkgs/"
  getSourcePath = meta:
    let
      result = builtins.tryEval (
        let
          pos = meta.position or null;
          # Extract file path (remove line number after colon)
          file = if pos == null then null
                 else let parts = builtins.split ":" pos;
                      in if builtins.length parts > 0 then builtins.elemAt parts 0 else null;
          # Find "pkgs/" in the path and extract from there
          extractRelative = path:
            let
              # Match "pkgs/" and everything after it
              matches = builtins.match ".*(pkgs/.*)" path;
            in if matches != null && builtins.length matches > 0
               then builtins.elemAt matches 0
               else null;
        in if file != null then extractRelative file else null
      );
    in if result.success then result.value else null;

  # Safely extract package info - each field is independently evaluated
  getPackageInfo = attrPath: pkg:
    let
      meta = pkg.meta or {};
      name = tryDeep (toString' (pkg.pname or pkg.name or attrPath));
      version = tryDeep (toString' (pkg.version or "unknown"));
      sourcePath = getSourcePath meta;
    in {
      name = if name != null then name else attrPath;
      version = if version != null then version else "unknown";
      attrPath = attrPath;
      description = safeString (meta.description or null);
      homepage = safeString (meta.homepage or null);
      license = if meta ? license then getLicenses meta.license else null;
      maintainers = if meta ? maintainers then getMaintainers meta.maintainers else null;
      platforms = if meta ? platforms then getPlatforms meta.platforms else null;
      sourcePath = safeString sourcePath;
      knownVulnerabilities = if meta ? knownVulnerabilities then getKnownVulnerabilities meta.knownVulnerabilities else null;
    };

  # Process each package name with full error isolation
  # The entire result is forced to catch any remaining lazy errors
  # Use hasAttr first since tryEval doesn't catch missing attribute errors
  processAttr = name:
    let
      exists = builtins.hasAttr name pkgs;
      valueResult = if exists then builtins.tryEval pkgs.${name} else { success = false; };
      value = if valueResult.success then valueResult.value else null;
      isDeriv = if value != null then isDerivation value else false;
      info = if isDeriv then getPackageInfo name value else null;
      # Force the entire info record to catch lazy evaluation errors
      forcedResult = if info != null then builtins.tryEval (builtins.deepSeq info info) else { success = false; };
    in if forcedResult.success then forcedResult.value else null;

  # Get list of attribute names and process them
  names = if attrNames != null then attrNames else builtins.attrNames pkgs;
  # Wrap entire lambda body in tryEval, returning list directly to avoid field access issues
  results = builtins.concatMap (name:
    let
      # Do all computation inside a single tryEval that returns a list
      safeResult = builtins.tryEval (
        let
          pkg = processAttr name;
          forced = builtins.deepSeq pkg pkg;
        in if forced != null then [forced] else []
      );
      # Safely extract value with another tryEval
      extracted = builtins.tryEval (
        builtins.seq safeResult.success (
          if safeResult.success then safeResult.value else []
        )
      );
    in if extracted.success then extracted.value else []
  ) names;
in
  results
