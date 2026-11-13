{
  pkgs,
  inputs,
  module,
}:
let
  hm =
    mode:
    inputs.home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        (if mode == "before" then { } else module)
        ({ lib, ... }: {
          home.username = "tester";
          home.homeDirectory = "/home/tester";
          home.stateVersion = "26.05";
          programs.man.enable = false;
          submoduleSupport.externalPackageInstall = true;
          home.file = {
            sentinel = {
              text = mode;
              onChange = lib.optionalString (mode == "active") ''
                test -L /home/tester/config/file
                test -f /home/tester/config/file
              '';
            };
          }
          // lib.optionalAttrs (mode == "handback") {
            "config/file".source = ./fallback/file;
          };
          home.localCheckoutLinks = lib.mkIf (mode == "active") {
            checkouts.project = "/home/tester/checkout";
            files."config/file" = {
              checkout = "project";
              source = "file";
              fallback = ./fallback/file;
            };
            files.".local/app/config" = {
              checkout = "project";
              source = "file";
              fallback = ./fallback/file;
            };
            files.tree = {
              checkout = "project";
              source = "tree";
              fallback = ./fallback;
              type = "directory";
            };
          };
        })
      ];
    };
  # Before deliberately has no module/options. Do not define even a false mkIf
  # for an option that does not exist in that generation.
  before = inputs.home-manager.lib.homeManagerConfiguration {
    inherit pkgs;
    modules = [
      {
        home.username = "tester";
        home.homeDirectory = "/home/tester";
        home.stateVersion = "26.05";
        programs.man.enable = false;
        submoduleSupport.externalPackageInstall = true;
        home.file."config/file".source = ./fallback/file;
      }
    ];
  };
  generations = {
    before = before.activationPackage;
    active = (hm "active").activationPackage;
    empty = (hm "empty").activationPackage;
    handback = (hm "handback").activationPackage;
  };
  closure = pkgs.closureInfo { rootPaths = builtins.attrValues generations; };
  script = pkgs.writeText "activation-contract.py" ''
    import json, os, pathlib, subprocess
    home = pathlib.Path('/home/tester')
    generations = json.loads('${
      builtins.toJSON (builtins.mapAttrs (_: builtins.toString) generations)
    }')
    def run(command, env=None):
        subprocess.run(command, shell=True, check=True, env=dict(os.environ, **(env or {})))
    def activate(name, env=None):
        run(generations[name] + '/activate', dict(HOME_MANAGER_BACKUP_COMMAND='${pkgs.lib.getExe pkgs.trash-cli}', **(env or {})))
    def snapshot():
        return {str(p): (p.lstat().st_ino, p.lstat().st_mtime_ns, p.lstat().st_mode,
            os.readlink(p) if p.is_symlink() else p.read_bytes() if p.is_file() else None)
            for p in home.rglob('*')}
    activate('before')
    target = home / 'config/file'
    assert '-home-manager-files/' in os.readlink(target)
    checkout = home / 'checkout'
    (checkout / 'tree/nested').mkdir(parents=True)
    (checkout / 'file').write_text('local')
    (checkout / 'tree/file').write_text('tree local')
    (checkout / 'tree/.hidden').write_text('hidden')
    (home / 'tree').mkdir()
    (home / 'tree/unrelated').write_text('preserve')
    first_conflict = home / '.local/app/config'
    first_conflict.parent.mkdir(parents=True)
    first_conflict.write_text('first trash backup')
    assert not (home / '.local/share').exists()
    activate('active')
    assert os.readlink(first_conflict) == str(checkout / 'file')
    assert any(p.is_file() and p.read_text() == 'first trash backup'
        for p in (home / '.local/share/Trash/files').iterdir())
    assert os.readlink(target) == str(checkout / 'file')
    assert (home / 'tree/file').read_text() == 'tree local'
    assert (home / 'tree/nested/child').read_text() == 'nested fallback\n'
    assert (home / 'tree/unrelated').read_text() == 'preserve'
    assert not (home / 'tree').is_symlink()
    inode = target.lstat().st_ino
    activate('active')
    assert target.lstat().st_ino == inode
    (checkout / 'file').unlink()
    activate('active')
    assert target.read_text() == 'store fallback\n'
    (checkout / 'file').write_text('returned')
    activate('active')
    assert target.read_text() == 'returned'
    checkout.rename(home / 'away')
    activate('active')
    assert target.read_text() == 'store fallback\n'
    (home / 'away').rename(checkout)
    activate('active')
    assert target.read_text() == 'returned'
    for value in ("", "1"):
        before = snapshot()
        activate('active', {'DRY_RUN': value})
        assert snapshot() == before, 'dry activation mutated home/state/trash'
    # Active declarations replace user content using the real pinned trash.
    target.unlink()
    target.write_text('user conflict')
    activate('active')
    trash = home / '.local/share/Trash/files'
    assert any(p.is_file() and p.read_text() == 'user conflict' for p in trash.iterdir())
    # HM accepts identical non-HM symlinks, but rejects differing ones before
    # its backup command runs. Exercise direct handback with identical content.
    (checkout / 'file').write_text('store fallback\n')
    ledger_path = home / '.local/state/home-manager/local-checkout-links/ledger.json'
    ledger = json.loads(ledger_path.read_text())
    ledger['pending'] = {'path': 'config/file', 'old': str(checkout / 'file'),
        'new': str((pathlib.Path(generations['handback']) / 'home-files').resolve() / 'config/file')}
    ledger_path.write_text(json.dumps(ledger))
    before = snapshot()
    activate('handback', {'DRY_RUN': ""})
    assert snapshot() == before, 'dry handback mutated home/state/trash'
    activate('handback')
    assert '-home-manager-files/' in os.readlink(target)
    assert target.read_text() == 'store fallback\n'
    assert json.loads(ledger_path.read_text()) == {'version': 1, 'links': {}, 'pending': None}
    activate('handback')
    assert '-home-manager-files/' in os.readlink(target)
    (checkout / 'file').write_text('returned')
    activate('active')
    assert target.read_text() == 'returned'
    # Non-link conflicts go to trash even with an existing fixed-name backup.
    target.unlink()
    target.write_text('handback conflict')
    (home / 'config/file.backup').write_text('keep old backup')
    activate('handback')
    assert target.read_text() == 'store fallback\n'
    assert (home / 'config/file.backup').read_text() == 'keep old backup'
    assert any(p.is_file() and p.read_text() == 'handback conflict' for p in trash.iterdir())
    activate('active')
    # Empty declarations retire exact owned links but preserve replacements.
    hidden = home / 'tree/.hidden'
    hidden.unlink()
    hidden.write_text('user replacement')
    activate('empty')
    assert not os.path.lexists(target)
    assert hidden.read_text() == 'user replacement'
    assert (home / 'tree/unrelated').read_text() == 'preserve'
    assert not os.path.lexists(home / 'tree/file')
    # Two-step handback is still supported.
    activate('handback')
    assert '-home-manager-files/' in os.readlink(target)
    assert target.read_text() == 'store fallback\n'
    # Empty activation before uninstalling the import leaves no custom links.
    activate('empty')
    activate('before')
    assert target.read_text() == 'store fallback\n'
  '';
in
pkgs.testers.runNixOSTest {
  name = "local-checkout-links-activation";
  containers.machine = { ... }: {
    users.users.tester = {
      isNormalUser = true;
      uid = 1000;
    };
    environment.systemPackages = [
      pkgs.python3
      pkgs.nix
    ];
    system.extraDependencies = [ closure ] ++ builtins.attrValues generations;
    system.stateVersion = "26.05";
  };
  testScript = ''
    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("NIX_REMOTE=local nix-store --load-db < ${closure}/registration")
    machine.succeed("su -l tester -c '${pkgs.python3}/bin/python3 ${script}'")
  '';
}
