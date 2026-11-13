# Pure helpers over the public key registry. Exported as `lib.sshTrust`.
{ lib }:
let
  inherit (lib)
    attrNames
    concatStringsSep
    filterAttrs
    groupBy
    isString
    mapAttrs
    sort
    ;

  registry = import ./_registry.nix;

  validName = s: isString s && builtins.match "[^[:space:],]+" s != null;

  normalizeKey =
    id: k:
    let
      fields = lib.splitString " " k.publicKey;
    in
    assert lib.assertMsg (
      builtins.length fields == 2
    ) "ssh-trust: key `${id}` publicKey must be exactly `TYPE BASE64`, keep comments in `comment`";
    {
      inherit id;
      inherit (k) publicKey;
      description = k.description or id;
      comment = k.comment or null;
      trust = {
        login = false;
        userCa = false;
      }
      // (k.trust or { });
    };

  keys =
    let
      normalized = mapAttrs normalizeKey registry.keys;
      duplicates = filterAttrs (_: ids: builtins.length ids > 1) (
        lib.mapAttrs (_: map (k: k.id)) (groupBy (k: k.publicKey) (builtins.attrValues normalized))
      );
    in
    assert lib.assertMsg (
      duplicates == { }
    ) "ssh-trust: registry ids share key material: ${builtins.toJSON (builtins.attrValues duplicates)}";
    normalized;

  key = id: keys.${id} or (throw "ssh-trust: unknown key id `${id}`");

  # An authorized_keys / TrustedUserCaKeys line. The registry id is the
  # comment, so audit logs and files name the key the same way the registry does.
  keyLine = id: "${(key id).publicKey} ${id}";

  idsWithRole = role: sort lib.lessThan (attrNames (filterAttrs (_: k: k.trust.${role}) keys));

  # `TYPE BASE64` of an authorized_keys line, dropping options and comment.
  # Options may contain quoted spaces, so locate the key type instead of
  # counting fields.
  identityOf =
    line:
    let
      m = builtins.match "(.*[ ,])?((ssh|ecdsa|sk)-[^ ]+ [A-Za-z0-9+/=]+)( .*)?" line;
    in
    if m == null then throw "ssh-trust: cannot parse key line `${line}`" else builtins.elemAt m 1;

  idByIdentity = lib.listToAttrs (lib.mapAttrsToList (id: k: lib.nameValuePair k.publicKey id) keys);

  # Principal entries are either a bare principal name, or
  # `{ name; options; }` for an authorized_principals line with options,
  # such as `restrict,command="..."`.
  normalizePrincipal =
    p:
    let
      entry =
        if isString p then
          {
            name = p;
            options = null;
          }
        else
          {
            inherit (p) name;
            options = p.options or null;
          };
    in
    assert lib.assertMsg (validName entry.name)
      "ssh-trust: principal `${builtins.toJSON p}` must be a bare name without whitespace or commas; put options in `options`";
    entry;

  principalLine = p: if p.options == null then p.name else "${p.options} ${p.name}";

  # Direct key grants: a registry id, or `{ key = id; options = "..."; }`.
  normalizeDirectKey =
    g:
    if isString g then
      {
        key = (key g).id;
        options = null;
      }
    else
      {
        key = (key g.key).id;
        options = g.options or null;
      };

  directKeyLine = g: (lib.optionalString (g.options != null) "${g.options} ") + keyLine g.key;

  defaultPrincipal =
    machine: user:
    assert lib.assertMsg (validName machine) "ssh-trust: invalid machine name `${machine}`";
    assert lib.assertMsg (validName user) "ssh-trust: invalid account name `${user}`";
    "${machine}--${user}";

  manifestSchema = "swan-lake-ssh-trust-manifest";
  manifestVersion = 1;

  # Combine per-machine `swanLake.sshTrust.manifestEntry` values into the
  # versioned document the signing CLI consumes.
  mkManifest =
    entries:
    let
      byMachine = groupBy (e: e.machine) entries;
      dup = attrNames (filterAttrs (_: es: builtins.length es > 1) byMachine);
    in
    assert lib.assertMsg (
      dup == [ ]
    ) "ssh-trust: duplicate manifest machines: ${concatStringsSep ", " dup}";
    {
      schema = manifestSchema;
      version = manifestVersion;
      userCaKeys = map (id: {
        inherit id;
        inherit (key id) publicKey;
      }) (idsWithRole "userCa");
      machines = mapAttrs (_: es: (builtins.head es).accounts) byMachine;
    };
in
{
  inherit
    registry
    keys
    key
    keyLine
    identityOf
    idByIdentity
    normalizePrincipal
    principalLine
    normalizeDirectKey
    directKeyLine
    defaultPrincipal
    manifestSchema
    manifestVersion
    mkManifest
    ;

  loginKeyIds = idsWithRole "login";
  userCaKeyIds = idsWithRole "userCa";
  loginKeyLines = map keyLine (idsWithRole "login");
  userCaKeyLines = map keyLine (idsWithRole "userCa");
}
