# rclihdfs

[![CI](https://github.com/damianm-dev/rclihdfs/actions/workflows/ci.yml/badge.svg)](https://github.com/damianm-dev/rclihdfs/actions/workflows/ci.yml)

A small CLI that wraps `hdfs dfs` for `cp`, `mv`, `rm` and `mkdir` and enforces
a fixed path policy. Operations on privileged roots run as a Kerberos service
principal from a keytab, in a separate credential cache; everything else runs
with the caller's own Kerberos ticket. Every object that is copied, moved,
removed or created is written to a Postgres audit table.

## Requirements

- Linux with the `hdfs` client and MIT Kerberos tools (`kinit`, `klist`,
  `kdestroy`) on `PATH`.
- A keytab for the service principal.
- A Postgres table for the audit log (see [Audit log](#audit-log)).

## Build

```sh
cargo build --release
```

Static Linux binary (musl), built in Docker:

```sh
docker run --rm --platform linux/amd64 -v "$PWD":/project -w /project rust:alpine \
  sh -c "apk add --no-cache musl-dev && cargo build --release --target x86_64-unknown-linux-musl"
```

The binary ends up in `target/x86_64-unknown-linux-musl/release/rclihdfs`.

## Usage

```
rclihdfs cp    <source> <target>
rclihdfs mv    <source> <target> [-y|--yes]
rclihdfs rm    <source> [-y|--yes]
rclihdfs mkdir <path>
```

- `mv` and `rm` ask for confirmation. `-y` skips the prompt.
- `cp` and `mv` accept a wildcard (`*`, `?`, `[`) in the source. The target
  must then be an existing directory. Wildcards in the target are rejected.
- `rm` does not delete: it moves the path to a trash location (see below).
- A wildcard `mv` or `cp` handles all matches with a single `hdfs dfs` call.

Exit codes: `0` success, `1` policy violation, declined confirmation or
missing path, `2` any other error.

## Configuration

Settings are read from `/etc/clihdfs/conf` (dotenv format). The command fails
with exit code `2` if the file is missing, unreadable or malformed. Values in
the file override environment variables of the same name; keys missing from
the file fall back to the environment. All keys are required.

```sh
# Kerberos service principal used for privileged roots
TECH_PRINCIPAL=svc_hdfs@EXAMPLE.COM
TECH_KEYTAB_PATH=/etc/security/keytabs/svc_hdfs.keytab

# HDFS roots (absolute, not "/")
HDFS_PROD_ROOT=/data
HDFS_STAGING_ROOT=/data/staging
HDFS_EXTERNAL_ROOT=/external
HDFS_MV_TO_PROD_ROOT=/compute
HDFS_TO_STAGING_ROOT=/science

# Audit log
DB.HOST=db.example.com
DB.PORT=5432
DB.NAME=audit
DB.USER=rclihdfs
DB.PASSWORD=change-me
DB.TABLE=public.hdfs_audit
```

The caller is identified by the real uid (from the passwd database), not by
`USER`/`LOGNAME`.

## Path policy

`<home>` is the caller's `/user/<username>`.

| Source | `cp` allowed into | `mv` allowed into |
|---|---|---|
| `PROD_ROOT/…` | `PROD_ROOT` | `PROD_ROOT` |
| `EXTERNAL_ROOT/…` | `PROD_ROOT`, `EXTERNAL_ROOT/…` | `PROD_ROOT`, `EXTERNAL_ROOT/…` |
| `MV_TO_PROD_ROOT/…` | — | `PROD_ROOT` |
| `TO_STAGING_ROOT` | `STAGING_ROOT` | `STAGING_ROOT` |
| `<home>/…` | `<home>/…`, `STAGING_ROOT` | `<home>/…`, `STAGING_ROOT` |

- Paths with a `.` or `..` component are rejected, since HDFS would resolve
  them and move the real target outside the policy.
- `mkdir` and `rm` only accept paths under `PROD_ROOT/`, `EXTERNAL_ROOT/` or
  `<home>/`.
- `rm` moves the path to `<home>/.trash<path>` for home paths, or to
  `STAGING_ROOT/trash<path>` otherwise. An existing object at the trash path is
  replaced.
- `mv` from `STAGING_ROOT` into the rest of `PROD_ROOT` requires every file to
  have a replication factor of at least 3.
- The service principal is used when any path is under `PROD_ROOT/` or
  `EXTERNAL_ROOT/`, plus `cp` from `TO_STAGING_ROOT` and `mv` from or into
  `MV_TO_PROD_ROOT/` or `TO_STAGING_ROOT/`.

## Audit log

After each operation, one row per affected object is inserted:

```sql
INSERT INTO <DB.TABLE>(username, mode, source, target, date) VALUES (...)
```

`mode` is `cp`, `mv`, `rm` or `mkdir`; `date` is local time without a time
zone.

The audit database connection is opened before the HDFS change is made: if it
is misconfigured or unreachable, the command aborts with exit code `2` and
nothing is changed. That same connection is reused for every record. If a
record still cannot be written (e.g. the database drops mid-run), the command
fails with exit code `2` — the HDFS change has already been made, so an
incomplete audit is surfaced, not hidden.

If a wildcard `cp` or `mv` partly fails, rows are written only for the objects
it actually copied or moved, and the command exits with code `2`.

## Security note

The policy is only enforced by this binary, which runs as the calling user.
That user therefore needs read access to the config file (including the
database password) and to the keytab, and can use them directly: `kinit` as
the service principal and run `hdfs` without any policy. Treat the tool as a
guard against mistakes, not as a security boundary.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
