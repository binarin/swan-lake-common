import copy
import fcntl
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import reconcile as r


class Runtime(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.home = self.root / 'home'
        self.home.mkdir()
        self.checkout = self.root / 'checkout'
        self.checkout.mkdir()
        self.fallback = self.root / 'fallback'
        self.fallback.write_text('immutable')
        self.local = self.checkout / 'source'
        self.local.write_text('editable')
        self.trash = self.root / 'trash'
        self.trash.mkdir()
        self.backup = self.root / 'backup'
        self.backup.write_text('#!' + os.sys.executable + '\nimport os,sys,shutil\nfrom pathlib import Path\np=Path(sys.argv[-1]); d=Path(os.environ["TEST_TRASH"]); shutil.move(str(p),d/str(len(list(d.iterdir()))))\n')
        self.backup.chmod(0o755)
        self.env = patch.dict(os.environ, TEST_TRASH=str(self.trash))
        self.env.start()
        self.addCleanup(self.env.stop)
        self.manifest = {'home': str(self.home), 'state': str(self.home / '.state/links'),
                         'checkouts': {'test': str(self.checkout)}, 'files': {'config/file': {
                             'checkout': 'test', 'source': 'source', 'fallback': str(self.fallback), 'type': 'file'}}}

    def reconciler(self):
        return r.Reconciler(r.prepare(copy.deepcopy(self.manifest)), str(self.backup))

    def run_links(self, **kwargs):
        self.reconciler().run(**kwargs)

    @property
    def dest(self):
        return self.home / 'config/file'

    def snapshot(self):
        return {str(p.relative_to(self.root)): (p.lstat().st_mode, p.lstat().st_ino, p.lstat().st_mtime_ns,
            os.readlink(p) if p.is_symlink() else p.read_bytes() if p.is_file() else None)
            for p in self.root.rglob('*')}

    def ledger(self):
        return json.loads((Path(self.manifest['state']) / 'ledger.json').read_text())

    def test_file_same_generation_toggles_idempotence_and_live_edit(self):
        self.run_links()
        inode = self.dest.lstat().st_ino
        self.run_links()
        self.assertEqual(self.dest.lstat().st_ino, inode)
        self.assertEqual(list(self.trash.iterdir()), [])
        self.local.write_text('edited')
        self.assertEqual(self.dest.read_text(), 'edited')
        self.local.unlink()
        self.run_links()
        self.assertEqual(os.readlink(self.dest), str(self.fallback))
        self.local.write_text('back')
        self.run_links()
        self.assertEqual(self.dest.read_text(), 'back')
        self.checkout.rename(self.root / 'away')
        self.run_links()
        self.assertEqual(self.dest.read_text(), 'immutable')
        (self.root / 'away').rename(self.checkout)
        self.run_links()
        self.assertEqual(self.dest.read_text(), 'back')

    def test_null_checkout(self):
        self.manifest['checkouts']['test'] = None
        self.run_links()
        self.assertEqual(os.readlink(self.dest), str(self.fallback))

    def test_conflict_matrix(self):
        for t in ('file', 'identical', 'directory', 'empty-directory', 'link', 'dangling', 'directory-link'):
            with self.subTest(t=t):
                if os.path.lexists(self.dest):
                    self.dest.unlink()
                self.dest.parent.mkdir(exist_ok=True)
                if t in ('directory', 'empty-directory'):
                    self.dest.mkdir()
                    if t == 'directory':
                        (self.dest / 'keep').write_text('saved')
                elif t in ('link', 'dangling', 'directory-link'):
                    self.dest.symlink_to(self.checkout if t == 'directory-link' else self.fallback if t == 'link' else '/missing')
                else:
                    self.dest.write_text('immutable' if t == 'identical' else 'user')
                before = len(list(self.trash.iterdir()))
                self.run_links()
                self.assertEqual(len(list(self.trash.iterdir())), before + 1)
                self.assertEqual(self.dest.read_text(), 'editable')
                self.assertEqual(self.local.read_text(), 'editable')

    def test_parent_conflicts(self):
        for t in ('file', 'symlink', 'dangling'):
            with self.subTest(t=t):
                if self.dest.parent.exists():
                    shutil.rmtree(self.dest.parent)
                if t == 'file':
                    self.dest.parent.write_text('parent')
                else:
                    self.dest.parent.symlink_to(self.checkout if t == 'symlink' else '/missing')
                self.run_links()
                self.assertFalse(self.dest.parent.is_symlink())
                self.assertEqual(self.dest.read_text(), 'editable')
                self.assertFalse((self.checkout / 'file').exists())

    def directory_setup(self):
        self.local.unlink()
        self.local.mkdir()
        self.fallback.unlink()
        self.fallback.mkdir()
        (self.fallback / 'nested').mkdir()
        (self.fallback / 'nested/b').write_text('fallback-b')
        (self.fallback / 'a').write_text('fallback-a')
        (self.fallback / 'empty').mkdir()
        (self.local / 'a').write_text('local-a')
        (self.local / '.hidden').write_text('hidden')
        (self.local / 'extra').write_text('extra')
        self.manifest['files'] = {'tree': dict(self.manifest['files']['config/file'], type='directory')}
        return self.home / 'tree'

    def test_recursive_merge_union_removal_and_real_directories(self):
        tree = self.directory_setup()
        (tree / 'nested').mkdir(parents=True)
        (tree / 'nested/unrelated').write_text('keep')
        inode = (tree / 'nested').stat().st_ino
        self.run_links()
        self.assertFalse(tree.is_symlink())
        self.assertEqual((tree / 'a').read_text(), 'local-a')
        self.assertEqual((tree / 'nested/b').read_text(), 'fallback-b')
        self.assertEqual((tree / '.hidden').read_text(), 'hidden')
        self.assertTrue((tree / 'empty').is_dir())
        self.assertEqual((tree / 'nested').stat().st_ino, inode)
        (self.local / 'extra').unlink()
        self.run_links()
        self.assertFalse(os.path.lexists(tree / 'extra'))
        self.assertEqual((tree / 'nested/unrelated').read_text(), 'keep')
        (self.local / 'a').unlink()
        self.run_links()
        self.assertEqual((tree / 'a').read_text(), 'fallback-a')
        self.assertEqual((tree / 'nested').stat().st_ino, inode)

    def test_directory_file_conflicts_and_type_transition(self):
        tree = self.directory_setup()
        tree.write_text('conflict')
        self.run_links()
        self.assertTrue(tree.is_dir())
        shutil.rmtree(self.local)
        shutil.rmtree(self.fallback)
        self.local.write_text('new-file')
        self.fallback.write_text('store-file')
        self.manifest['files']['tree']['type'] = 'file'
        self.run_links()
        self.assertTrue(tree.is_symlink())
        self.assertEqual(tree.read_text(), 'new-file')
        self.assertEqual(self.ledger()['links'], {'tree': str(self.local)})

    def test_source_types_symlinks_and_mismatch_before_writes(self):
        self.local.unlink()
        self.local.symlink_to('/missing')
        self.run_links()
        self.assertEqual(self.dest.read_text(), 'immutable')
        self.local.unlink()
        self.local.symlink_to(self.fallback)
        self.run_links()
        self.assertEqual(os.readlink(self.dest), str(self.local))
        self.local.unlink()
        self.local.mkdir()
        before = self.snapshot()
        with self.assertRaises(r.Error):
            self.run_links()
        self.assertEqual(before, self.snapshot())
        tree = self.directory_setup_after_reset()
        (self.local / 'a').unlink()
        (self.local / 'a').mkdir()
        with self.assertRaises(r.Error):
            self.run_links()
        (self.local / 'a').rmdir()
        (self.local / 'dirlink').symlink_to(self.checkout)
        with self.assertRaises(r.Error):
            self.run_links()
        (self.local / 'dirlink').unlink()
        os.mkfifo(self.local / 'fifo')
        with self.assertRaises(r.Error):
            self.run_links()
        self.assertFalse(tree.exists())

    def directory_setup_after_reset(self):
        shutil.rmtree(self.local)
        self.local.write_text('reset')
        return self.directory_setup()

    def test_backup_failures_and_retry(self):
        self.dest.parent.mkdir()
        for script, moved in [('exit 1', False), ('exit 0', False),
                              ('mv -- "$2" "$TEST_TRASH/moved"; exit 1', True)]:
            with self.subTest(script=script):
                self.dest.write_text('save me')
                self.backup.write_text('#!/bin/sh\n' + script + '\n')
                with self.assertRaises(r.Error):
                    self.run_links()
                self.assertFalse(self.dest.is_symlink())
                if moved:
                    self.assertEqual((self.trash / 'moved').read_text(), 'save me')
                    self.assertFalse(self.dest.exists())
                else:
                    self.assertEqual(self.dest.read_text(), 'save me')
                if self.dest.exists():
                    self.dest.unlink()
        self.run_links()
        self.assertEqual(self.dest.read_text(), 'editable')

    def test_stale_cleanup_preserves_replacements(self):
        for replacement in ('file', 'link', 'directory', 'owned'):
            with self.subTest(replacement=replacement):
                self.manifest['files'] = {'config/file': {'checkout': 'test', 'source': 'source', 'fallback': str(self.fallback), 'type': 'file'}}
                if self.dest.is_dir() and not self.dest.is_symlink():
                    self.dest.rmdir()
                self.run_links()
                if replacement != 'owned':
                    self.dest.unlink()
                    if replacement == 'file':
                        self.dest.write_text('user')
                    elif replacement == 'link':
                        self.dest.symlink_to('/user-replacement')
                    else:
                        self.dest.mkdir()
                self.manifest['files'] = {}
                self.run_links()
                self.assertEqual(os.path.lexists(self.dest), replacement != 'owned')
                self.assertEqual(self.ledger()['links'], {})
                if self.dest.is_symlink() or self.dest.is_file():
                    self.dest.unlink()

    def test_dry_run_and_check_no_mutation_including_empty_env(self):
        for warm in (False, True):
            if warm:
                self.run_links()
                self.local.unlink()
            for args, env in [({'check': True}, {}), ({'dry': True}, {}), ({}, {'DRY_RUN': ''}), ({}, {'DRY_RUN': '1'})]:
                with patch.dict(os.environ, env):
                    before = self.snapshot()
                    if 'DRY_RUN' in env:
                        raw = self.root / 'manifest.json'
                        raw.write_text(json.dumps(r.prepare(copy.deepcopy(self.manifest))))
                        before = self.snapshot()
                        subprocess.run([os.sys.executable, r.__file__, str(raw), '--trash', str(self.backup)], check=True)
                    else:
                        self.run_links(**args)
                    self.assertEqual(before, self.snapshot())

    def test_dry_run_conflicts_and_stale_links(self):
        self.dest.parent.mkdir()
        self.dest.write_text('conflict')
        before = self.snapshot()
        self.run_links(dry=True)
        self.assertEqual(self.snapshot(), before)
        self.run_links()
        self.manifest['files'] = {}
        before = self.snapshot()
        self.run_links(dry=True)
        self.assertEqual(self.snapshot(), before)
        self.assertTrue(self.dest.is_symlink())

    def test_journal_recovery_each_interruption(self):
        for stage in ('journal', 'trash', 'install', 'ledger'):
            with self.subTest(stage=stage):
                self.run_links()
                new = str(self.fallback)
                data = self.ledger()
                data['pending'] = {'path': 'config/file', 'old': str(self.local), 'new': new}
                if stage != 'journal':
                    self.dest.unlink()
                if stage in ('install', 'ledger'):
                    self.dest.symlink_to(new)
                if stage == 'ledger':
                    data['links']['config/file'] = new
                self.reconciler().save(data)
                self.manifest['files'] = {}
                self.run_links()
                self.assertFalse(os.path.lexists(self.dest))
                self.assertIsNone(self.ledger()['pending'])
                self.manifest['files'] = {'config/file': {'checkout': 'test', 'source': 'source', 'fallback': str(self.fallback), 'type': 'file'}}

    def test_malformed_state_and_state_symlink_no_home_mutation(self):
        state = Path(self.manifest['state'])
        state.mkdir(parents=True, mode=0o700)
        for bad in ({'version': 2}, {'version': 1, 'links': {'../escape': '/source'}, 'pending': None},
                    {'version': 1, 'links': {}, 'pending': {'path': '../escape', 'old': None, 'new': '/source'}}):
            (state / 'ledger.json').write_text(json.dumps(bad))
            before = self.snapshot()
            with self.assertRaises(r.Error):
                self.run_links()
            self.assertEqual(before, self.snapshot())
        (state / 'ledger.json').unlink()
        state.rmdir()
        state.symlink_to(self.checkout)
        with self.assertRaises(r.Error):
            self.run_links()
        self.assertFalse(self.dest.exists())

    def test_lock_contention(self):
        self.run_links()
        with (Path(self.manifest['state']) / 'lock').open('w') as stream:
            fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaises(r.Error):
                self.run_links()

    def test_source_and_destination_races_stop_before_backup(self):
        self.dest.parent.mkdir()
        self.dest.write_text('old')
        for change in ('source', 'destination', 'parent'):
            rec = self.reconciler()
            rec.state_path(create=True)
            data, desired = rec.preflight()
            if change == 'source':
                self.local.write_text('raced')
            elif change == 'destination':
                self.dest.write_text('user edit')
            else:
                self.dest.parent.rename(self.home / 'old-parent')
                self.dest.parent.symlink_to(self.checkout)
            with self.assertRaises(r.Error):
                rec.apply(data, desired)
            self.assertEqual(list(self.trash.iterdir()), [])

    def test_overlap_and_one_step_handback_rejected(self):
        self.manifest['files']['config/file']['source'] = 'source'
        self.manifest['checkouts']['test'] = str(self.home)
        self.manifest['files']['config/file']['source'] = 'config'
        with self.assertRaises(r.Error):
            self.run_links()
        self.manifest['checkouts']['test'] = str(self.checkout)
        self.manifest['files']['config/file']['source'] = 'source'
        self.run_links()
        self.manifest['files'] = {}
        self.manifest['homeFiles'] = [str(self.dest)]
        with self.assertRaisesRegex(r.Error, 'handback'):
            self.run_links()

    def test_real_interruptions_leave_recoverable_journal(self):
        for stage in ('journal', 'trash', 'install', 'ledger'):
            with self.subTest(stage=stage):
                self.run_links()
                self.local.unlink()
                rec = self.reconciler()
                original_save, original_run, original_symlink = rec.save, r.subprocess.run, r.os.symlink
                def save(data):
                    original_save(data)
                    if stage == 'journal' and data['pending']:
                        raise InterruptedError('after journal')
                    if stage == 'ledger' and not data['pending'] and data['links'].get('config/file') == str(self.fallback):
                        raise InterruptedError('after ledger')
                def backup(*args, **kwargs):
                    result = original_run(*args, **kwargs)
                    if stage == 'trash':
                        raise InterruptedError('after trash')
                    return result
                def symlink(*args, **kwargs):
                    original_symlink(*args, **kwargs)
                    if stage == 'install':
                        raise InterruptedError('after install')
                with patch.object(rec, 'save', save), patch.object(r.subprocess, 'run', backup), patch.object(r.os, 'symlink', symlink):
                    with self.assertRaises(InterruptedError):
                        rec.run()
                saved_files = copy.deepcopy(self.manifest['files'])
                self.manifest['files'] = {}
                self.run_links()
                self.assertFalse(os.path.lexists(self.dest))
                self.assertEqual(self.ledger()['links'], {})
                self.assertIsNone(self.ledger()['pending'])
                self.manifest['files'] = saved_files
                self.local.write_text('editable')

    def test_rename_and_file_to_directory_transition(self):
        self.run_links()
        entry = self.manifest['files'].pop('config/file')
        self.manifest['files']['renamed'] = entry
        self.run_links()
        self.assertFalse(os.path.lexists(self.dest))
        self.assertEqual((self.home / 'renamed').read_text(), 'editable')
        self.local.unlink()
        self.fallback.unlink()
        self.local.mkdir()
        self.fallback.mkdir()
        (self.local / 'child').write_text('child')
        entry['type'] = 'directory'
        self.run_links()
        self.assertFalse((self.home / 'renamed').is_symlink())
        self.assertEqual((self.home / 'renamed/child').read_text(), 'child')

    def test_fallback_inventory_rejects_missing_mismatch_special_and_symlink_directory(self):
        self.fallback.unlink()
        for form in ('missing', 'directory', 'fifo', 'dangling', 'directory-link'):
            with self.subTest(form=form):
                if form == 'directory':
                    self.fallback.mkdir()
                elif form == 'fifo':
                    os.mkfifo(self.fallback)
                elif form in ('dangling', 'directory-link'):
                    self.fallback.symlink_to('/missing' if form == 'dangling' else self.checkout)
                with self.assertRaises(r.Error):
                    self.reconciler()
                self.assertFalse(self.dest.exists())
                if self.fallback.is_dir() and not self.fallback.is_symlink():
                    self.fallback.rmdir()
                elif os.path.lexists(self.fallback):
                    self.fallback.unlink()

    def test_directory_leaf_referent_overlap_rejected(self):
        tree = self.directory_setup()
        tree.mkdir()
        (tree / 'a').write_text('user')
        (self.local / 'a').unlink()
        (self.local / 'a').symlink_to(tree / 'a')
        with self.assertRaisesRegex(r.Error, 'overlap'):
            self.run_links()
        self.assertEqual((tree / 'a').read_text(), 'user')

    def test_discovery_race_is_detected(self):
        self.directory_setup()
        original_kind = r.kind
        def change(path, **kwargs):
            value = original_kind(path, **kwargs)
            if path == self.local / 'a':
                (self.local / 'late').write_text('late addition')
            return value
        with patch.object(r, 'kind', change):
            with self.assertRaisesRegex(r.Error, 'changed during'):
                self.run_links()
        self.assertFalse((self.home / 'tree').exists())

    def test_source_symlink_referent_overlap_rejected(self):
        self.dest.parent.mkdir()
        self.dest.write_text('user')
        self.local.unlink()
        self.local.symlink_to(self.dest)
        with self.assertRaisesRegex(r.Error, 'overlap'):
            self.run_links()
        self.assertEqual(self.dest.read_text(), 'user')

    def test_home_file_aliases_reject_active_and_old_ownership_without_writes(self):
        self.dest.parent.mkdir()
        self.dest.symlink_to(self.fallback)
        aliases = ('config/./file', 'config/a/../file', 'config//file',
                   './config', 'other/../config/file/child', 'config/..')
        for target in aliases:
            with self.subTest(target=target):
                self.manifest['homeFiles'] = [str(self.home) + '/' + target]
                before = self.snapshot()
                with self.assertRaisesRegex(r.Error, 'overlaps home.file'):
                    self.run_links()
                self.assertEqual(self.snapshot(), before)
        self.manifest['homeFiles'] = [str(self.home) + '/config/./sibling']
        self.run_links()
        self.manifest['files'] = {}
        for ownership in ('links', 'pending'):
            if ownership == 'pending':
                self.manifest['homeFiles'] = []
                self.reconciler().save({'version': 1, 'links': {}, 'pending': {
                    'path': 'config/file', 'old': None, 'new': str(self.local)}})
            for target in aliases:
                with self.subTest(handback=target, ownership=ownership):
                    self.manifest['homeFiles'] = [str(self.home) + '/' + target]
                    before = self.snapshot()
                    with self.assertRaisesRegex(r.Error, 'handback'):
                        self.run_links()
                    self.assertEqual(self.snapshot(), before)

    def test_nested_dangling_fallback_rejected_but_local_links_fall_back_or_retire(self):
        tree = self.directory_setup()
        (self.fallback / 'nested/broken').symlink_to('/nonexistent-zl0040')
        before = self.snapshot()
        with self.assertRaisesRegex(r.Error, 'dangling fallback symlink'):
            self.run_links()
        self.assertEqual(self.snapshot(), before)
        (self.fallback / 'nested/broken').unlink()
        self.run_links()
        (self.local / 'a').unlink()
        (self.local / 'a').symlink_to('/nonexistent-zl0040')
        (self.local / 'extra').unlink()
        (self.local / 'extra').symlink_to('/nonexistent-zl0040')
        self.run_links()
        self.assertEqual(os.readlink(tree / 'a'), str(self.fallback / 'a'))
        self.assertFalse(os.path.lexists(tree / 'extra'))
        self.assertEqual(self.ledger()['links']['tree/a'], str(self.fallback / 'a'))

    @unittest.skipUnless(os.environ.get('REAL_TRASH'), 'requires pinned trash-cli')
    def test_real_trash_first_use_in_watched_ancestor(self):
        self.backup = Path(os.environ['REAL_TRASH'])
        entry = self.manifest['files'].pop('config/file')
        self.manifest['files']['.local/app/config'] = entry
        dest = self.home / '.local/app/config'
        dest.parent.mkdir(parents=True)
        dest.write_text('first use backup')
        self.assertFalse((self.home / '.local/share').exists())
        with patch.dict(os.environ, HOME=str(self.home), XDG_DATA_HOME=str(self.home / '.local/share')):
            self.run_links()
            self.assertEqual(os.readlink(dest), str(self.local))
            files = list((self.home / '.local/share/Trash/files').iterdir())
            self.assertEqual([p.read_text() for p in files], ['first use backup'])
            inode = dest.lstat().st_ino
            self.run_links()
            self.assertEqual(dest.lstat().st_ino, inode)
            self.assertEqual(list((self.home / '.local/share/Trash/files').iterdir()), files)

    @unittest.skipUnless(os.environ.get('REAL_TRASH'), 'requires pinned trash-cli')
    def test_trash_creation_does_not_mask_unrelated_or_source_races(self):
        self.backup = Path(os.environ['REAL_TRASH'])
        entry = self.manifest['files'].pop('config/file')
        self.manifest['files']['.local/app/config'] = entry
        dest = self.home / '.local/app/config'
        dest.parent.mkdir(parents=True)
        real_run = r.subprocess.run
        with patch.dict(os.environ, HOME=str(self.home), XDG_DATA_HOME=str(self.home / '.local/share')):
            for change in ('addition', 'replacement', 'source', 'source-directory'):
                with self.subTest(change=change):
                    shutil.rmtree(self.home / '.local/share', ignore_errors=True)
                    unrelated = self.home / '.local/unrelated'
                    unrelated.write_text('preserve')
                    dest.write_text('backup bytes')
                    def backup(*args, **kwargs):
                        result = real_run(*args, **kwargs)
                        if change == 'addition':
                            (self.home / '.local/raced').write_text('raced')
                        elif change == 'replacement':
                            unrelated.rename(unrelated.with_name('old'))
                            unrelated.write_text('replacement')
                        elif change == 'source':
                            self.local.write_text('changed source')
                        else:
                            (self.checkout / 'new').write_text('added source')
                        return result
                    with patch.object(r.subprocess, 'run', backup):
                        with self.assertRaisesRegex(r.Error, 'changed during reconciliation'):
                            self.run_links()
                    self.assertFalse(os.path.lexists(dest))
                    files = list((self.home / '.local/share/Trash/files').iterdir())
                    self.assertEqual([p.read_text() for p in files], ['backup bytes'])

    @unittest.skipUnless(os.environ.get('REAL_TRASH'), 'requires pinned trash-cli')
    def test_real_trash_files_directories_dangling(self):
        self.backup = Path(os.environ['REAL_TRASH'])
        with patch.dict(os.environ, HOME=str(self.home), XDG_DATA_HOME=str(self.home / '.local/share')):
            self.dest.parent.mkdir()
            for t in ('file', 'directory', 'dangling'):
                if os.path.lexists(self.dest):
                    self.dest.unlink()
                if t == 'file':
                    self.dest.write_text('saved bytes')
                elif t == 'directory':
                    self.dest.mkdir()
                    (self.dest / 'child').write_text('saved directory')
                else:
                    self.dest.symlink_to('/missing')
                self.run_links()
            infos = list((self.home / '.local/share/Trash/info').glob('*.trashinfo'))
            self.assertEqual(len(infos), 3)
            files = list((self.home / '.local/share/Trash/files').iterdir())
            self.assertTrue(any(p.is_symlink() and os.readlink(p) == '/missing' for p in files))
            self.assertTrue(any(p.is_dir() and (p / 'child').read_text() == 'saved directory' for p in files))
            self.assertTrue(any(p.is_file() and not p.is_symlink() and p.read_text() == 'saved bytes' for p in files))


if __name__ == '__main__':
    unittest.main()
