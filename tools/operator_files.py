"""Bounded, regular-file snapshots for offline operator preparation tools."""
import hashlib
import json
import os
from pathlib import Path
import stat

LIMIT = 4 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def read_file(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    if not stat.S_ISREG(os.fstat(descriptor).st_mode):
        os.close(descriptor)
        raise ValueError("input must be a regular file")
    with os.fdopen(descriptor, "rb") as stream:
        data = stream.read(LIMIT + 1)
    require(len(data) <= LIMIT, "input exceeds 4 MiB limit")
    return data


def parse_json(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result
    def invalid(value):
        raise ValueError("non-finite JSON value: " + value)
    return json.loads(data, object_pairs_hook=unique, parse_constant=invalid)


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n").encode()


def write_new(path, data):
    descriptor = os.open(Path(path), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
