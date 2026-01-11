# Package metadata extraction for nxv indexer
# This file is included at compile time via include_str!()
{ nixpkgsPath, system, attrNames ? null, extractStorePaths ? true }:
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
  # We validate the prefix, length, and hyphen separator.
  #
  # IMPORTANT: Accessing pkg.outPath triggers derivationStrict which evaluates ALL
  # build inputs. On old nixpkgs (2018) + modern Nix, darwin SDK packages fail to
  # evaluate and the error escapes tryEval. Use extractStorePaths=false to skip.
  getStorePath = pkg:
    # Skip if extractStorePaths is false (avoids derivationStrict for old commits)
    if !extractStorePaths then null
    else
    let
      storePrefix = "/nix/store/";
      storePrefixLen = 11;
      hashLen = 32;
      minLen = storePrefixLen + hashLen + 1; # prefix + hash + hyphen

      result = builtins.tryEval (
        if pkg ? outPath then builtins.toString pkg.outPath else null
      );
      path = if result.success then result.value else null;

      # Check that hash is followed by a hyphen (separator before name)
      hasHyphenAfterHash = path != null &&
        builtins.stringLength path > minLen &&
        builtins.substring (storePrefixLen + hashLen) 1 path == "-";

    in if path != null
          && builtins.stringLength path > minLen
          && builtins.substring 0 storePrefixLen path == storePrefix
          && hasHyphenAfterHash
       then path
       else null;

  # Extract version from package name using regex patterns
  # Handles: semver (1.2.3), dates (2021-07-29), compact dates (202202), etc.
  extractVersionFromName = name:
    let
      # Strip common file extensions before pattern matching
      # Handles: .tgz, .tar.gz, .tar.bz2, .tar.xz, .zip, .src, .orig
      stripExtension = s:
        let
          extensions = [
            "\\.tar\\.gz$" "\\.tar\\.bz2$" "\\.tar\\.xz$" "\\.tar\\.zst$"
            "\\.tgz$" "\\.tbz2$" "\\.txz$"
            "\\.zip$" "\\.gz$" "\\.bz2$" "\\.xz$"
            "\\.src$" "\\.orig$" "\\.source$"
          ];
          stripOne = str: ext:
            let m = builtins.match "(.*)${ext}" str;
            in if m != null then builtins.elemAt m 0 else str;
          stripAll = str: exts:
            if builtins.length exts == 0 then str
            else stripAll (stripOne str (builtins.elemAt exts 0)) (builtins.tail exts);
        in stripAll s extensions;

      cleanName = stripExtension name;

      # Try various patterns in order of specificity
      patterns = [
        # Version with internal hyphen and suffix: name-0.9.8.6-0.rc1, name-1.0-beta1
        ".*-([0-9]+\\.[0-9]+(\\.[0-9]+)*-[0-9a-z.]+)$"
        # Pre-release versions: name-0.99.1pre130312, name-1.0alpha2
        ".*-([0-9]+\\.[0-9]+(\\.[0-9]+)*[a-z]+[0-9]+)$"
        # Milestone versions: name-0.0.m8, name-1.0.rc1 (letter after dot)
        ".*-([0-9]+\\.[0-9]+\\.[a-z]+[0-9]*)$"
        # Semver with letter+digit suffix: name-1.2.3rc1, name-2.0beta2
        ".*-([0-9]+\\.[0-9]+(\\.[0-9]+)*[a-z]+[0-9]*)$"
        # Semver with optional single letter suffix: name-1.2.3, name-1.2.3a
        ".*-([0-9]+\\.[0-9]+(\\.[0-9]+)*[a-z]?)$"
        # v-prefixed version: name-v1.2.3
        ".*-v([0-9]+\\.[0-9]+(\\.[0-9]+)*)$"
        # ISO date: name-2021-07-29
        ".*-([0-9]{4}-[0-9]{2}-[0-9]{2})$"
        # Compact date: name-202202, name-20220215
        ".*-([0-9]{6,8})$"
        # Java-style: name-7u111b01
        ".*-([0-9]+u[0-9]+[a-z][0-9]*)$"
        # Release prefix: name-r10e
        ".*-(r[0-9]+[a-z]?)$"
        # Alphanumeric: name-9100h, name-21a
        ".*-([0-9]+[a-z][0-9]*)$"
        # Git hash (7-12 chars): name-08ae128
        ".*-([a-f0-9]{7,12})$"
        # Single number: name-42
        ".*-([0-9]+)$"
      ];

      tryPattern = pattern:
        let
          match = builtins.match pattern cleanName;
        in
          if match != null && builtins.length match > 0
          then builtins.elemAt match 0
          else null;

      # Try each pattern until one matches
      findVersion = pats:
        if builtins.length pats == 0 then null
        else
          let
            result = tryPattern (builtins.elemAt pats 0);
          in
            if result != null then result
            else findVersion (builtins.tail pats);

      extracted = tryDeep (findVersion patterns);
    in
      if extracted != null then extracted else null;

  # Get version with fallback chain and source tracking
  # Returns: { version = "1.2.3"; source = "direct"|"unwrapped"|"passthru"|"name"|null; }
  getVersionWithSource = pkg: name:
    let
      # 1. Try direct pkg.version
      directVersion = tryDeep (pkg.version or null);
      directResult = if directVersion != null && directVersion != ""
        then { version = toString' directVersion; source = "direct"; }
        else null;

      # 2. Try pkg.unwrapped.version (wrapper pattern)
      unwrappedVersion = tryDeep ((pkg.unwrapped.version or null));
      unwrappedResult = if unwrappedVersion != null && unwrappedVersion != ""
        then { version = toString' unwrappedVersion; source = "unwrapped"; }
        else null;

      # 3. Try pkg.passthru.unwrapped.version
      passthruVersion = tryDeep ((pkg.passthru.unwrapped.version or null));
      passthruResult = if passthruVersion != null && passthruVersion != ""
        then { version = toString' passthruVersion; source = "passthru"; }
        else null;

      # 4. Try extracting from package name
      nameVersion = if name != null then extractVersionFromName name else null;
      nameResult = if nameVersion != null
        then { version = nameVersion; source = "name"; }
        else null;

      # Return first successful result, or null
      result =
        if directResult != null then directResult
        else if unwrappedResult != null then unwrappedResult
        else if passthruResult != null then passthruResult
        else if nameResult != null then nameResult
        else { version = null; source = null; };
    in result;

  # Safely extract package info - each field is independently evaluated
  getPackageInfo = attrPath: pkg:
    let
      meta = pkg.meta or {};
      name = tryDeep (toString' (pkg.pname or pkg.name or attrPath));
      versionInfo = getVersionWithSource pkg name;
      sourcePath = getSourcePath meta;
      storePath = getStorePath pkg;
    in {
      name = if name != null then name else attrPath;
      version = versionInfo.version;
      versionSource = versionInfo.source;
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
  # Empty list or null triggers full discovery via builtins.attrNames
  names = if attrNames != null && builtins.length attrNames > 0 then attrNames else builtins.attrNames pkgs;

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
  # Empty list or null triggers full discovery including nested sets
  nestedResults = if attrNames != null && builtins.length attrNames > 0 then [] else
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
