# Public source and private operator material

The OSS repository contains reusable code, synthetic tests and public
documentation. Account histories, real trading records, wallet state and operator
notes belong in the private repository or private local storage.

- `.gitignore` ignores every `Z-`/`z-` name at any depth and with any extension,
  plus the designated local data, archive, journal and keystore directories.
- Already tracked private journals remain tracked in the **private** repository.
  Ignore rules do not untrack them or erase commits. New private notes require
  an intentional `git add -f` only in the verified private repository.
- `.gitattributes` excludes private names and data from `git archive` as another
  layer. The exporter uses `tools/check_public_export.py` to prune private paths
  recursively and independently validate the resulting tree and relative links.
- Private history must never be pushed or merged into a public repository. The
  exporter starts with public history and overlays a filtered source tree.
  A clean current tree does not make its private ancestors safe to publish.
- With `git config core.hooksPath tools/hooks`, `pre-push` checks reachable paths,
  including deleted private files, using NUL-delimited tree paths so embedded
  newlines cannot hide names. It allows private history only to the exact
  URL stored in local `hypercarry.privatePushUrl`. Set that value only after
  independently verifying the repository is private. Hooks can be bypassed and
  the allowlist cannot detect a later hosting-visibility change.
- Portfolio archives use `Z-observation-*.json` (and `Z-*.tmp` while writing),
  so even archives outside `data/` retain the private filename boundary. The
  recommended location is still `data/account-snapshots/`, with mode 0700.

Never copy real account identifiers, fills or histories into public fixtures,
README examples or issue bodies. Filename and credential checks cannot detect
all private content moved into an innocently named file; independently inspect
the exact filtered diff before any separately authorized public publication.
No permission to publish follows from passing these checks.

Example setup, using the verified private URL for your own installation:

```sh
git config core.hooksPath tools/hooks
git config hypercarry.privatePushUrl 'YOUR_VERIFIED_PRIVATE_GIT_URL'
```

Private raw evidence is intentionally ignored. A Git push of source and private
notes does not back up the raw data directory or wallet keystore.
