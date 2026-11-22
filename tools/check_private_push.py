#!/usr/bin/env python3
"""Reject pushing private history anywhere except an explicitly configured URL.

Invoked by tools/hooks/pre-push. The local config is a destination allowlist,
not a proof of hosting visibility. Verify the repository is private before push.
A filtered public clone has its own clean history and needs no allowlist.
"""
import subprocess
import sys
from check_public_export import excluded, forbidden

ZERO = '0' * 40


def contains_private_history(commit):
    # rev-list --objects truncates paths at embedded newlines. Enumerate every
    # distinct reachable tree, then use NUL-delimited ls-tree output instead.
    trees = subprocess.check_output(['git', 'log', '--format=%T', commit], text=True).splitlines()
    for tree in dict.fromkeys(trees):
        data = subprocess.check_output(['git', 'ls-tree', '-r', '--name-only', '-z', tree])
        for raw_name in data.split(b'\0'):
            name = raw_name.decode('utf-8', errors='surrogateescape')
            if name and (excluded(name) or forbidden(name)):
                return True
    return False


def check_push(remote_url, updates):
    private = False
    for line in updates.splitlines():
        fields = line.split()
        if len(fields) != 4:
            raise ValueError('malformed pre-push input')
        commit = fields[1]
        if commit == ZERO:
            continue  # Deleting a remote ref does not publish objects.
        if len(commit) != 40 or any(c not in '0123456789abcdef' for c in commit):
            raise ValueError('invalid local object ID')
        private = contains_private_history(commit) or private
    if not private:
        return
    config = subprocess.run(['git', 'config', '--get', 'hypercarry.privatePushUrl'], text=True, capture_output=True)
    if config.returncode != 0 or config.stdout.strip() != remote_url:
        raise ValueError('private history push blocked: destination is not hypercarry.privatePushUrl; use a filtered public export, never private history')


def main():
    try:
        if len(sys.argv) != 3:
            raise ValueError('expected remote name and destination URL')
        check_push(sys.argv[2], sys.stdin.read())
    except (ValueError, subprocess.SubprocessError) as error:
        print(f'pre-push: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
