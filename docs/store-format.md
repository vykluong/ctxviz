# Cursor CLI chat store format

This page describes the parts of the cursor-agent chat store that `ctxviz` reads. The format is undocumented and was worked out by inspection, so it may change between Cursor releases. When it does, `ctxviz doctor` shows what the store looks like now.

## Location

Each chat has its own SQLite database:

```
<cursor dir>/chats/<md5(abs cwd)>/<agentId>/store.db
```

- `<cursor dir>` is the Cursor CLI config dir: `$CURSOR_CONFIG_DIR` when set and not blank, else `$XDG_CONFIG_HOME/cursor` when that is set and not blank, else `~/.cursor`. `CURSOR_DATA_DIR` does not move chats; it only moves `projects/` (transcripts).

- `<md5(abs cwd)>` is the lowercase hex md5 of the absolute workspace path, for example `0123456789abcdef0123456789abcdef`.
- `<agentId>` is the chat id. It equals the `session_id` field of the status line payload, which is how the status line finds its store.
- Subagent chats live in the same project directory as their parent, so lookups by id must match the exact directory name.

The database runs in WAL mode. Recent writes sit in `store.db-wal` until SQLite checkpoints them, and that file can shrink back to zero bytes, so its size is not a reliable change signal. `ctxviz` opens the store read-only and never modifies it, but SQLite still creates `store.db-wal` and `store.db-shm` next to it when they are missing, because any reader of a WAL-mode database needs the shared-memory index.

## Tables

- `blobs(id, data)`: content-addressed blobs. `id` is a 64-character hex string; `data` is either a JSON document (a message) or a protobuf message. Rows are append-only, and `rowid` follows insertion order.
- `meta(key, value)`: one row with key `0`. Its value is a hex-encoded JSON object.

## Meta keys

The decoded meta object carries these keys (others may appear and are ignored):

| Key | Meaning |
| --- | --- |
| `agentId` | the chat id, same as the directory name |
| `name` | the chat title |
| `latestRootBlobId` | blob id of the newest root blob |
| `createdAt` | chat creation time |
| `mode` | agent mode |
| `lastUsedModel` | model id of the last turn |
| `blobEncryptionKey` | present in current stores; blobs are still stored in plain text |

## Root blobs

Every checkpoint writes a new root blob (protobuf) and leaves the earlier ones in place. `latestRootBlobId` points at the newest. These root fields matter:

| Field | Type | Meaning |
| --- | --- | --- |
| 1 | repeated bytes | ordered message blob ids, 32 raw bytes each (hex form is the blob id) |
| 4 | bytes | differs between the two twin roots written for each checkpoint |
| 5 | message | token details: used tokens (1), max tokens (2), breakdown (3) |
| 26 | varint | conversation creation time, the same in every root of a store |

Inside the breakdown, field 3 repeats one entry per category, each with an id (1), a label (2), and an estimated token count (3).

`ctxviz` recognizes a root by shape rather than by a type tag: the blob is not JSON, decodes cleanly as protobuf, every field 1 entry is a 32-byte id, and field 26 is a varint. The first roots of a new chat have no field 5 and are skipped. Twin roots are collapsed into one checkpoint.

All field numbers live in `ctxviz/src/schema.rs`, so a renumbering in a future Cursor release is a one-file change.
