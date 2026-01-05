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

  # Known package sets that contain nested derivations
  # These will be recursively explored to find nested packages
  nestedPackageSets = [
    # Qt packages (contains qtwebengine, etc.)
    "qt5" "qt6" "libsForQt5" "kdePackages"
    # Python packages
    "python3Packages" "python311Packages" "python312Packages" "python313Packages"
    # Scripting language packages
    "perlPackages" "rubyPackages" "rubyPackages_3_1" "rubyPackages_3_2" "rubyPackages_3_3"
    "luaPackages" "lua51Packages" "lua52Packages" "lua53Packages" "luajitPackages"
    # Node/JS packages
    "nodePackages" "nodePackages_latest"
    # Haskell packages
    "haskellPackages" "haskell.packages.ghc94" "haskell.packages.ghc96"
    # OCaml packages
    "ocamlPackages" "ocaml-ng.ocamlPackages_4_14"
    # Elm packages
    "elmPackages"
    # R packages
    "rPackages"
    # Emacs packages
    "emacsPackages"
    # Vim plugins
    "vimPlugins"
    # Desktop environments
    "gnome" "pantheon" "mate" "cinnamon" "xfce"
    # PHP packages
    "phpPackages" "php81Packages" "php82Packages" "php83Packages"
    # Rust packages (crates)
    "rustPackages"
    # Go packages
    "goPackages"
    # Texlive packages
    "texlive"
  ];

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

  # Safely extract the output path (store path) from a derivation
  # This gives us the /nix/store/hash-name-version path without building
  # Note: We use a different name for the return value because "outPath" is a special
  # Nix attribute that causes coercion issues when serializing to JSON.
  #
  # Store path format: /nix/store/<32-char-hash>-<name>
  # The hash is base32 encoded (chars: 0-9, a-z excluding e,o,t,u)
  # We validate both the prefix and the hash format to catch invalid paths early.
  getStorePath = pkg:
    let
      storePrefix = "/nix/store/";
      storePrefixLen = 11;
      hashLen = 32;
      # Base32 alphabet used by Nix (lowercase, no e/o/t/u)
      base32Chars = "0123456789abcdfghijklmnpqrsvwxyz";

      result = builtins.tryEval (
        if pkg ? outPath then builtins.toString pkg.outPath else null
      );
      path = if result.success then result.value else null;

      # Extract the hash portion (32 chars after /nix/store/)
      hash = if path != null && builtins.stringLength path > (storePrefixLen + hashLen)
             then builtins.substring storePrefixLen hashLen path
             else null;

      # Validate hash contains only valid base32 characters
      isValidHash = hash != null &&
        builtins.stringLength hash == hashLen &&
        builtins.all (c: builtins.elem c (builtins.stringToCharacters base32Chars))
                     (builtins.stringToCharacters hash);

      # Check that hash is followed by a hyphen (separator before name)
      hasHyphenAfterHash = path != null &&
        builtins.stringLength path > (storePrefixLen + hashLen) &&
        builtins.substring (storePrefixLen + hashLen) 1 path == "-";

    in if path != null
          && builtins.substring 0 storePrefixLen path == storePrefix
          && isValidHash
          && hasHyphenAfterHash
       then path
       else null;

  # Safely extract package info - each field is independently evaluated
  getPackageInfo = attrPath: pkg:
    let
      meta = pkg.meta or {};
      name = tryDeep (toString' (pkg.pname or pkg.name or attrPath));
      version = tryDeep (toString' (pkg.version or "unknown"));
      sourcePath = getSourcePath meta;
      storePath = getStorePath pkg;
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
      storePath = storePath;
    };

  # Check if an attribute name matches a nested package set pattern
  isNestedPackageSet = name:
    builtins.elem name nestedPackageSets ||
    # Also check for patterns like pythonXXPackages, rubyPackages_X_X, etc.
    (builtins.match "python[0-9]+Packages" name != null) ||
    (builtins.match "rubyPackages_[0-9_]+" name != null) ||
    (builtins.match "php[0-9]+Packages" name != null) ||
    (builtins.match "lua[0-9]+Packages" name != null);

  # Process a nested package set, extracting all derivations from it
  processNestedSet = prefix: attrSet:
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
                  valueResult = builtins.tryEval attrSet.${nestedName};
                  value = if valueResult.success then valueResult.value else null;
                  isDeriv = if value != null then isDerivation value else false;
                  info = if isDeriv then getPackageInfo fullPath value else null;
                  forcedResult = if info != null then builtins.tryEval (builtins.deepSeq info info) else { success = false; };
                in
                  if forcedResult.success then [forcedResult.value] else []
              ) nestedNames.value
            else []
        else []
      );
    in if result.success then result.value else [];

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

  # Process a dotted attribute path like "qt6.qtwebengine"
  processDottedAttr = attrPath:
    let
      parts = builtins.filter (x: builtins.isString x) (builtins.split "\\." attrPath);
      # Navigate to the value through the path
      getValue = currentSet: remainingParts:
        if builtins.length remainingParts == 0 then currentSet
        else
          let
            head = builtins.elemAt remainingParts 0;
            tail = builtins.genList (i: builtins.elemAt remainingParts (i + 1)) (builtins.length remainingParts - 1);
            hasAttrResult = builtins.tryEval (builtins.hasAttr head currentSet);
          in
            if hasAttrResult.success && hasAttrResult.value then
              let
                nextResult = builtins.tryEval currentSet.${head};
              in
                if nextResult.success then getValue nextResult.value tail
                else null
            else null;
      valueResult = builtins.tryEval (getValue pkgs parts);
      value = if valueResult.success then valueResult.value else null;
      isDeriv = if value != null then isDerivation value else false;
      info = if isDeriv then getPackageInfo attrPath value else null;
      forcedResult = if info != null then builtins.tryEval (builtins.deepSeq info info) else { success = false; };
    in if forcedResult.success then forcedResult.value else null;

  # Check if a name is a dotted path (nested attribute)
  isDottedPath = name: builtins.match ".*\\..*" name != null;

  # Get list of attribute names and process them
  names = if attrNames != null then attrNames else builtins.attrNames pkgs;

  # Separate top-level names from dotted paths
  topLevelNames = builtins.filter (n: !isDottedPath n) names;
  dottedNames = builtins.filter isDottedPath names;

  # Process top-level packages
  topLevelResults = builtins.concatMap (name:
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
  ) topLevelNames;

  # Process dotted attribute paths (nested packages)
  dottedResults = builtins.concatMap (attrPath:
    let
      safeResult = builtins.tryEval (
        let
          pkg = processDottedAttr attrPath;
          forced = builtins.deepSeq pkg pkg;
        in if forced != null then [forced] else []
      );
      extracted = builtins.tryEval (
        builtins.seq safeResult.success (
          if safeResult.success then safeResult.value else []
        )
      );
    in if extracted.success then extracted.value else []
  ) dottedNames;

  # Process nested package sets (only when not filtering by specific attrs)
  nestedResults = if attrNames != null then [] else
    builtins.concatMap (setName:
      let
        exists = builtins.hasAttr setName pkgs;
        valueResult = if exists then builtins.tryEval pkgs.${setName} else { success = false; };
        value = if valueResult.success then valueResult.value else null;
        nested = if value != null && builtins.isAttrs value
                 then processNestedSet setName value
                 else [];
        forcedNested = builtins.tryEval (builtins.deepSeq nested nested);
      in
        if forcedNested.success then forcedNested.value else []
    ) nestedPackageSets;

  results = topLevelResults ++ dottedResults ++ nestedResults;

  # Final safety filter: remove any null entries that might have slipped through
  filteredResults = builtins.filter (x: x != null) results;
in
  filteredResults
