"""Private durable records and single-writer locks; no credential storage."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
from contextlib import contextmanager


def require(ok, message):
    if not ok:
        raise ValueError(message)


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def private_dir(path):
    path = Path(path).absolute()
    require(not any(p.is_symlink() for p in (path, *path.parents)), "symlink in private path")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    st = path.stat()
    require(stat.S_ISDIR(st.st_mode) and st.st_uid == os.getuid() and st.st_mode & 0o077 == 0,
            "state directory must be owned by this user and mode 0700")
    return path


def read(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as stream:
        st = os.fstat(stream.fileno())
        require(stat.S_ISREG(st.st_mode) and st.st_uid == os.getuid()
                and st.st_mode & 0o077 == 0 and st.st_size <= 16 * 1024 * 1024,
                "record must be private, regular and bounded")
        return json.load(stream)


def write(path, value, *, replace=False):
    path = Path(path)
    parent = private_dir(path.parent)
    fd, name = tempfile.mkstemp(prefix=".pending-", dir=parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(canonical(value) + b"\n")
            stream.flush()
            os.fsync(stream.fileno())
        if replace:
            require(not path.is_symlink(), "symlink record")
            os.replace(name, path)
        else:
            os.link(name, path)  # No overwrite, including after an uncertain attempt.
        directory = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(name):
            os.unlink(name)


@contextmanager
def lock(path):
    private_dir(Path(path).parent)
    fd = os.open(path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    try:
        st = os.fstat(fd)
        require(stat.S_ISREG(st.st_mode) and st.st_uid == os.getuid() and st.st_mode & 0o077 == 0,
                "unsafe lock file")
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield
    finally:
        os.close(fd)
