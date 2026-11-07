"""Activation-time leaf reconciliation. No mutable source is read by Nix."""
import argparse
import contextlib
import fcntl
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile


@contextlib.contextmanager
def closing_fd(fd):
    try:
        yield fd
    finally:
        os.close(fd)


class Error(RuntimeError):
    pass


def relative(value):
    if not isinstance(value, str) or not value or '\0' in value or any(
        p in ('', '.', '..') for p in value.split('/')
    ):
        raise Error(f'invalid relative path: {value!r}')
    return value


def overlaps(a, b):
    return a == b or a.startswith(b.rstrip('/') + '/') or b.startswith(a.rstrip('/') + '/')


def identity(path):
    try:
        s = os.lstat(path)
        return (s.st_dev, s.st_ino, s.st_mode, s.st_size, s.st_mtime_ns,
                os.readlink(path) if stat.S_ISLNK(s.st_mode) else None)
    except FileNotFoundError:
        return None


def kind(path, required=False):
    try:
        s = os.lstat(path)
    except FileNotFoundError:
        return None
    if stat.S_ISLNK(s.st_mode):
        try:
            s = os.stat(path)
        except FileNotFoundError:
            if required:
                raise Error(f'dangling fallback symlink: {path}')
            return None
        if not stat.S_ISREG(s.st_mode):
            raise Error(f'source symlink must resolve to a regular file: {path}')
    if stat.S_ISDIR(s.st_mode):
        return 'directory'
    if stat.S_ISREG(s.st_mode):
        return 'file'
    raise Error(f'unsupported source type: {path}')


def inventory(root, watch=None, required=False):
    result = {}
    def visit(path, name):
        if watch:
            watch(path)
        t = kind(path, required=True) if required else kind(path)
        if t is None:
            return
        result[name] = t
        if t == 'directory':
            for child in sorted(path.iterdir()):
                visit(child, f'{name}/{child.name}' if name else child.name)
    visit(Path(root), '')
    return result


def prepare(raw, immutable=False):
    for entry in raw['files'].values():
        tree = inventory(entry['fallback'], required=True)
        if tree.get('') != entry['type']:
            raise Error(f'fallback type mismatch: {entry["fallback"]}')
        if immutable:
            for name, t in tree.items():
                if t == 'file' and not str((Path(entry['fallback']) / name).resolve()).startswith('/nix/store/'):
                    raise Error(f'fallback leaf is not immutable: {entry["fallback"]}/{name}')
        entry['inventory'] = tree
    return raw


class Reconciler:
    def __init__(self, manifest, trash):
        self.manifest = manifest
        self.home = Path(manifest['home'])
        self.state = Path(manifest['state'])
        self.trash = trash
        self.expected = {}
        self.sources = {}
        if not self.home.is_absolute() or self.home.resolve() != self.home:
            raise Error('home must be an absolute real directory path')
        if not self.home.is_dir() or self.home.is_symlink():
            raise Error('home must be a real directory')
        if not self.state.is_absolute() or '..' in self.state.parts:
            raise Error('invalid state directory')
        # Match HM's build-time lexical normalization, never resolve through home.
        self.protected = []
        for target in manifest.get('homeFiles', []):
            if not isinstance(target, str) or not target.startswith('/') or '\0' in target:
                raise Error('invalid home.file target')
            self.protected.append(os.path.normpath('/' + target.lstrip('/')))
        targets = list(manifest['files'])
        for target in targets:
            relative(target)
            destination = str(self.home / target)
            if overlaps(destination, str(self.state)):
                raise Error('target overlaps ownership state')
            if any(overlaps(target, other) for other in targets if other != target):
                raise Error('overlapping declarations')
            if any(overlaps(destination, other) for other in self.protected):
                raise Error('target overlaps home.file')

    def watch(self, path, source=False):
        # Watching resolved files also detects a symlink referent changing.
        table = self.sources if source else self.expected
        if source:
            table.setdefault(str(path), identity(path))
        else:
            table[str(path)] = identity(path)
        if source and Path(path).is_symlink() and Path(path).exists():
            resolved = Path(path).resolve()
            table.setdefault(str(resolved), identity(resolved))

    def unchanged(self):
        for path, before in {**self.expected, **self.sources}.items():
            if identity(path) != before:
                raise Error(f'path changed during reconciliation: {path}; retry activation')

    def state_path(self, create=False):
        current = Path('/')
        for part in self.state.parts[1:]:
            current /= part
            s = identity(current)
            if s is None:
                if create:
                    current.mkdir(mode=0o700)
                else:
                    continue
            elif not stat.S_ISDIR(s[2]):
                raise Error(f'state ancestor is not a real directory: {current}')
        if self.state.exists():
            s = self.state.stat()
            if s.st_uid != os.getuid() or s.st_mode & 0o077:
                raise Error(f'state must be private and owned by current user: {self.state}')

    def load(self):
        self.state_path()
        path = self.state / 'ledger.json'
        if not os.path.lexists(path):
            return {'version': 1, 'links': {}, 'pending': None}
        if path.is_symlink() or not path.is_file():
            raise Error('ledger is not a regular file')
        with path.open() as stream:
            data = json.load(stream)
        if set(data) != {'version', 'links', 'pending'} or data['version'] != 1:
            raise Error('unsupported ledger format')
        if not isinstance(data['links'], dict):
            raise Error('invalid ownership links')
        for name, text in data['links'].items():
            relative(name)
            if not isinstance(text, str) or not text.startswith('/') or '\0' in text:
                raise Error('invalid owned link')
        pending = data['pending']
        if pending is not None:
            if not isinstance(pending, dict) or set(pending) != {'path', 'old', 'new'}:
                raise Error('invalid recovery journal')
            relative(pending['path'])
            for text in (pending['old'], pending['new']):
                if text is not None and (not isinstance(text, str) or not text.startswith('/') or '\0' in text):
                    raise Error('invalid journal link')
        for name in list(data['links']) + ([pending['path']] if pending else []):
            if overlaps(str(self.home / name), str(self.state)):
                raise Error('ledger overlaps state')
            if any(overlaps(str(self.home / name), p) for p in self.protected):
                raise Error('old ownership overlaps home.file; activate empty declarations before handback')
        return data

    def save(self, data):
        self.state_path()
        fd, name = tempfile.mkstemp(prefix='.ledger-', dir=self.state)
        try:
            with os.fdopen(fd, 'w') as stream:
                json.dump(data, stream, sort_keys=True)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, self.state / 'ledger.json')
            with closing_fd(os.open(self.state, os.O_RDONLY | os.O_DIRECTORY)) as directory_fd:
                os.fsync(directory_fd)
        finally:
            if os.path.exists(name):
                os.unlink(name)

    def link_text(self, name):
        # Never inspect a child through an obstructing/symlinked parent.
        path = self.home
        parts = Path(relative(name)).parts
        for part in parts[:-1]:
            path /= part
            s = identity(path)
            if s is None or not stat.S_ISDIR(s[2]):
                return None
        path /= parts[-1]
        s = identity(path)
        return s[5] if s and stat.S_ISLNK(s[2]) else None

    def recover(self, data):
        pending = data['pending']
        if pending:
            actual = self.link_text(pending['path'])
            selected = next((t for t in (pending['new'], pending['old']) if t is not None and t == actual), None)
            if selected:
                data['links'][pending['path']] = selected
            else:
                data['links'].pop(pending['path'], None)
            data['pending'] = None

    def select(self):
        desired = {}
        for target, entry in self.manifest['files'].items():
            root = self.manifest['checkouts'][entry['checkout']]
            local = Path(root) / entry['source'] if root is not None else None
            if root is not None:
                if Path(root).exists() and not Path(root).is_dir():
                    raise Error(f'checkout is not a directory: {root}')
                self.watch(root, True)
            fallback = Path(entry['fallback'])
            local_tree = inventory(local, lambda p: self.watch(p, True)) if local else {}
            fallback_tree = entry['inventory']
            if local_tree and local_tree[''] != entry['type']:
                raise Error(f'local source type mismatch: {local}')
            for name in set(local_tree) | set(fallback_tree):
                if name in local_tree and name in fallback_tree and local_tree[name] != fallback_tree[name]:
                    raise Error(f'local/fallback type mismatch at {local / name}')
                t = local_tree.get(name, fallback_tree.get(name))
                source = local / name if name in local_tree else fallback / name
                dest = target + ('/' + name if name else '')
                desired[dest] = (t, str(source) if t == 'file' else None)
                self.watch(source, True)
            if local:
                # Include missing local paths and directory mtimes to detect additions/removals.
                self.watch(local, True)
                for name in fallback_tree:
                    self.watch(local / name, True)
            source_roots = [local, fallback] + [Path(text) for t, text in desired.values() if t == 'file']
            for source_root in source_roots:
                if source_root:
                    resolved = str(source_root.resolve())
                    lexical = str(source_root)
                    for dest in self.manifest['files']:
                        destination = str(self.home / dest)
                        if overlaps(resolved, destination) or overlaps(lexical, destination):
                            raise Error(f'source/destination overlap: {source_root} and {destination}')
        for name in list(desired):
            for parent in Path(name).parents:
                if str(parent) != '.':
                    desired.setdefault(str(parent), ('directory', None))
        return desired

    def observe_destinations(self, desired, links):
        for name in set(desired) | set(links):
            path = self.home
            for part in Path(name).parts:
                path /= part
                self.watch(path)
                s = identity(path)
                if s is None or not stat.S_ISDIR(s[2]):
                    break

    def preflight(self):
        data = self.load()
        self.recover(data)
        desired = self.select()
        self.observe_destinations(desired, data['links'])
        self.unchanged()
        return data, desired

    def parent_fd(self, path):
        fd = os.open(self.home, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            for part in path.relative_to(self.home).parts[:-1]:
                new = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                os.close(fd)
                fd = new
            return fd
        except BaseException:
            os.close(fd)
            raise

    def refresh(self, path):
        # Our changes affect destination directory mtimes, possibly also home
        # (which can contain a trusted checkout). Refresh only destination watches.
        for p in (path, path.parent):
            if str(p) in self.expected:
                self.expected[str(p)] = identity(p)
        if not path.is_dir() or path.is_symlink():
            for watched in list(self.expected):
                if watched.startswith(str(path) + '/'):
                    del self.expected[watched]

    def trash_creation_watches(self, path):
        # Pinned trash may mkdir -p $XDG_DATA_HOME/Trash/{info,files}. Only
        # the first existing ancestor of a missing branch changes metadata.
        # Snapshot its entries so accepting that one addition cannot hide an
        # unrelated replacement/addition. Source watches are never refreshed.
        data_home = os.environ.get('XDG_DATA_HOME', str(Path(os.environ.get('HOME', str(self.home))) / '.local/share'))
        snapshots = {}
        for leaf in ('info', 'files'):
            child = Path(os.path.abspath(os.path.join(data_home, 'Trash', leaf)))
            while identity(child) is None and child != child.parent:
                parent = child.parent
                before = identity(parent)
                if before is not None:
                    if (str(parent) in self.expected and parent != path.parent
                            and stat.S_ISDIR(before[2])):
                        if parent not in snapshots:
                            entries = {p.name: identity(p) for p in parent.iterdir()}
                            snapshots[parent] = (set(), before, entries)
                        snapshots[parent][0].add(child.name)
                    break
                child = parent
        return snapshots

    def refresh_trash_creation(self, snapshots, path):
        for parent, (children, before, entries) in snapshots.items():
            after = identity(parent)
            if after == before:
                continue
            current = {p.name: identity(p) for p in parent.iterdir()} if after and after[:3] == before[:3] else {}
            created = [current.pop(name, None) for name in children]
            # Backup also changes metadata of a child containing the target.
            # Its inode/type must survive; other entries must match exactly.
            def stable(items):
                return {name: value[:3] if parent / name in path.parents and value and stat.S_ISDIR(value[2])
                        else value for name, value in items.items()}
            if (not all(node and stat.S_ISDIR(node[2]) for node in created)
                    or not after or after[:3] != before[:3]
                    or stable(current) != stable(entries)):
                raise Error(f'path changed during reconciliation: {parent}; retry activation')
            self.expected[str(parent)] = after

    def apply(self, data, desired):
        self.save(data)  # prove state is writable before home mutations
        for name, old in sorted(list(data['links'].items()), reverse=True):
            if name in desired and desired[name][0] == 'file':
                continue
            self.unchanged()
            path = self.home / name
            if self.link_text(name) == old:
                print(f'retire {path}')
                data['pending'] = {'path': name, 'old': old, 'new': None}
                self.save(data)
                with closing_fd(self.parent_fd(path)) as fd:
                    os.unlink(path.name, dir_fd=fd)
                self.refresh(path)
            else:
                print(f'preserve replaced stale path {path}')
            data['links'].pop(name, None)
            data['pending'] = None
            self.save(data)
        for name, (t, source) in sorted(desired.items(), key=lambda item: (len(Path(item[0]).parts), item[0])):
            self.unchanged()
            path = self.home / name
            s = identity(path)
            correct = s and ((t == 'directory' and stat.S_ISDIR(s[2])) or (t == 'file' and s[5] == source))
            if not correct:
                data['pending'] = {'path': name, 'old': data['links'].get(name), 'new': source}
                self.save(data)
                self.unchanged()
                if s:
                    print(f'trash {path}', flush=True)
                    creation = self.trash_creation_watches(path)
                    self.unchanged()
                    result = subprocess.run([self.trash, '--', str(path)], check=False)
                    if result.returncode or os.path.lexists(path):
                        raise Error(f'backup failed for {path} (exit {result.returncode}); preserved backup if moved; stopping')
                    self.refresh_trash_creation(creation, path)
                    self.refresh(path)
                self.unchanged()
                with closing_fd(self.parent_fd(path)) as fd:
                    if t == 'directory':
                        os.mkdir(path.name, mode=0o755, dir_fd=fd)
                    else:
                        os.symlink(source, path.name, dir_fd=fd)
                print(f'{t} {path}' + (f' -> {source}' if source else ''))
                self.refresh(path)
                # A newly created directory now exposes its children; watch before use.
                if t == 'directory':
                    for child in desired:
                        if str(Path(child).parent) == name:
                            self.watch(self.home / child)
            if t == 'file':
                data['links'][name] = source
            data['pending'] = None
            self.save(data)

    def run(self, check=False, dry=False):
        data, desired = self.preflight()
        if check or dry:
            for name, (t, source) in sorted(desired.items()):
                path = self.home / name
                print(f'plan {t} {path}' + (f' -> {source}' if source else '') + ' (trash conflicts)')
            for name in sorted(set(data['links']) - set(desired)):
                print(f'plan retire exact owned link {self.home / name}')
            return
        self.state_path(create=True)
        lock = self.state / 'lock'
        fd = os.open(lock, os.O_WRONLY | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        try:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError as error:
                raise Error('another reconciliation holds the state lock') from error
            # Recompute inside the lock: preflight may be stale after HM cleanup.
            self.expected, self.sources = {}, {}
            data, desired = self.preflight()
            self.apply(data, desired)
        finally:
            os.close(fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest')
    parser.add_argument('--prepare', metavar='OUTPUT')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--trash', required=False)
    args = parser.parse_args()
    try:
        with open(args.manifest) as stream:
            manifest = json.load(stream)
        if args.prepare:
            with open(args.prepare, 'w') as stream:
                json.dump(prepare(manifest, immutable=True), stream, sort_keys=True)
        else:
            if not args.trash:
                parser.error('--trash is required for reconciliation')
            Reconciler(manifest, args.trash).run(check=args.check, dry=args.dry_run or 'DRY_RUN' in os.environ)
    except (Error, OSError, ValueError, KeyError, TypeError) as error:
        print(f'local-checkout-links: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
