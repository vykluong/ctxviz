"""Minimal schema-less protobuf wire decoder (no dependencies)."""


def varint(buf, i):
    shift = result = 0
    while True:
        b = buf[i]
        i += 1
        result |= (b & 0x7F) << shift
        if not b & 0x80:
            return result, i
        shift += 7


def fields(buf):
    """Yield (field_number, wire_type, value) for each top-level field."""
    i = 0
    while i < len(buf):
        key, i = varint(buf, i)
        no, wt = key >> 3, key & 7
        if wt == 0:
            v, i = varint(buf, i)
        elif wt == 1:
            v, i = buf[i:i + 8], i + 8
        elif wt == 2:
            n, i = varint(buf, i)
            v, i = buf[i:i + n], i + n
        elif wt == 5:
            v, i = buf[i:i + 4], i + 4
        else:
            raise ValueError(f"unsupported wire type {wt}")
        yield no, wt, v


def group(buf):
    """Return {field_number: [values...]}."""
    out = {}
    for no, _, v in fields(buf):
        out.setdefault(no, []).append(v)
    return out
