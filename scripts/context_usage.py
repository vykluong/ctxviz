#!/usr/bin/env python3
"""Cursor CLI "Context Usage" report, ported from the Cursor desktop app.

Reads a cursor-agent chat store (<cursor dir>/chats/<md5(cwd)>/<agentId>/store.db,
where the cursor dir is $CTXVIZ_CURSOR_DIR, $CURSOR_CONFIG_DIR, $XDG_CONFIG_HOME/cursor,
or ~/.cursor):

  * Totals and category breakdown come from the server-computed
    agent.v1.ConversationTokenDetails.breakdown persisted in the latest root
    ConversationStateStructure blob (same numbers as the desktop tray and the
    CLI's /context pager).
  * The Context Explorer tree is rebuilt locally from root_prompt_messages_json,
    mirroring the desktop's local report builder: same tag-to-category table
    and the same round(chars / 4) token estimate. Nodes follow the
    agent.v1.PromptContextNode shape.

Modes:
  static     context_usage.py [--chat ID|PATH] [--json] [--depth N]
  streaming  context_usage.py --watch [--json]   (re-renders on every checkpoint;
             with --json emits one NDJSON snapshot per checkpoint)
"""

import argparse
import glob
import hashlib
import json
import os
import re
import sqlite3
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pb import group  # noqa: E402

def cursor_dir():
    """The Cursor CLI config dir, as ctxviz resolves it: $CTXVIZ_CURSOR_DIR,
    else (like the CLI) $CURSOR_CONFIG_DIR, else $XDG_CONFIG_HOME/cursor,
    else ~/.cursor. Blank values count as unset; a relative value exits with
    an error naming the variable."""
    for var, suffix in (("CTXVIZ_CURSOR_DIR", ""), ("CURSOR_CONFIG_DIR", ""), ("XDG_CONFIG_HOME", "cursor")):
        value = os.environ.get(var, "")
        if value.strip():
            path = os.path.join(value, suffix) if suffix else value
            if not os.path.isabs(path):
                name = f"${var}/{suffix}" if suffix else f"${var}"
                sys.exit(f"{name} is not an absolute path: {path}")
            return path
    return os.path.expanduser("~/.cursor")


def chats_dir():
    return os.path.join(cursor_dir(), "chats")

# Category ids and order match agent.v1.PromptTokenBreakdownCategory ids.
CATEGORIES = [
    ("system_prompt", "System prompt", 244),
    ("tools", "Tool definitions", 99),
    ("rules", "Rules", 29),
    ("skills", "Skills", 136),
    ("mcp", "MCP & dynamic tools", 90),
    ("subagents", "Subagent definitions", 33),
    ("summarized_conversation", "Summarized conversation", 37),
    ("conversation", "Conversation", 160),
]
LABELS = {cid: label for cid, label, _ in CATEGORIES}
COLORS = {cid: color for cid, _, color in CATEGORIES}

# Desktop table M0h ("prompt-context-parts.ts"): user-info sections that are
# attributed to a category instead of the conversation.
SECTION_TAGS = [
    ("rules", "rules"),
    ("agent_skills", "skills"),
    ("available_subagent_models", "subagents"),
    ("available_subagent_types", "subagents"),
    ("dynamic_tool_catalog", "mcp"),
]


def estimate(text):
    """Desktop token-estimate.ts: Math.round(text.length / 4)."""
    return round(len(text) / 4)


# --------------------------------------------------------------------------
# Store access


def last_write(store):
    """Stores run in WAL mode: writes land in store.db-wal, not store.db."""
    return max(os.path.getmtime(p) for p in (store, store + "-wal") if os.path.exists(p))


def find_store(chat, cwd, latest=False):
    if chat and os.path.isfile(chat):
        return chat
    chats = chats_dir()
    if chat:
        hits = glob.glob(os.path.join(chats, "*", chat, "store.db"))
        if not hits:
            sys.exit(f"no chat store for agent id {chat}")
        return hits[0]
    if latest:
        stores = glob.glob(os.path.join(chats, "*", "*", "store.db"))
        if not stores:
            sys.exit(f"no cursor-agent chats in {chats}")
        return max(stores, key=last_write)
    project = hashlib.md5(os.path.abspath(cwd).encode()).hexdigest()
    stores = glob.glob(os.path.join(chats, project, "*", "store.db"))
    if not stores:
        sys.exit(f"no cursor-agent chats for {cwd} (looked in {chats}/{project}); "
                 "pass --cwd <workspace> or --latest; set CTXVIZ_CURSOR_DIR if the agent "
                 "uses another config dir (ctxviz doctor shows the dirs)")
    return max(stores, key=last_write)


class Store:
    def __init__(self, path):
        self.path = path
        self.db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)

    def meta(self):
        row = self.db.execute("select value from meta where key='0'").fetchone()
        return json.loads(bytes.fromhex(row[0])) if row else {}

    def blob(self, blob_id):
        row = self.db.execute("select data from blobs where id=?", (blob_id,)).fetchone()
        return row[0] if row else None


# --------------------------------------------------------------------------
# Decoding


def decode_token_details(root):
    g = group(root)
    out = {"usedTokens": 0, "maxTokens": 0, "categories": [], "messageIds": [b.hex() for b in g.get(1, [])]}
    if 5 not in g:
        return out
    td = group(g[5][0])
    out["usedTokens"] = td.get(1, [0])[0]
    out["maxTokens"] = td.get(2, [0])[0]
    if 3 in td:
        for raw in group(td[3][0]).get(3, []):
            c = group(raw)
            out["categories"].append({
                "id": c[1][0].decode(),
                "label": c[2][0].decode(),
                "estimatedTokens": c.get(3, [0])[0],
            })
    return out


def part_text(part):
    kind = part.get("type")
    if kind in ("text", "reasoning"):
        return part.get("text", "")
    if kind == "tool-call":
        return json.dumps(part.get("input", part.get("args", {})))
    if kind == "tool-result":
        out = part.get("output", part.get("result", ""))
        if isinstance(out, dict) and "value" in out:
            out = out["value"]
        return out if isinstance(out, str) else json.dumps(out)
    return json.dumps(part)


def message_parts(msg):
    """Yield (label, text) for each displayable piece of a prompt message."""
    role, content = msg.get("role"), msg.get("content")
    if isinstance(content, str):
        content = [{"type": "text", "text": content}]
    for part in content or []:
        kind, text = part.get("type"), part_text(part)
        if role == "user":
            label = "User Message" if "<user_query>" in text else "Context Message"
        elif kind == "reasoning":
            label = "Thinking"
        elif kind == "text":
            label = "Agent Message"
        elif kind == "tool-call":
            label = f"Tool: {part.get('toolName', '?')}"
        elif kind == "tool-result":
            label = f"Tool Result: {part.get('toolName', '?')}"
        else:
            label = kind or role
        yield label, text


ATTR = r'(?:name|fullPath|path|description)="([^"]+)"'


def split_section(category, body):
    """Split a section body into named children; mirrors FI_/UI_/$I_ grouping."""
    if category == "rules":
        pat = r"<(\w*rule)\s[^>]*?name=\"([^\"]+)\"[^>]*>(.*?)</\1>"
        items = [(os.path.basename(m.group(2)), m.group(0)) for m in re.finditer(pat, body, re.S)]
    elif category == "skills":
        pat = r"<agent_skill\b([^>]*)>(.*?)</agent_skill>"
        items = []
        for m in re.finditer(pat, body, re.S):
            a = re.search(ATTR, m.group(1))
            label = a.group(1) if a else m.group(2).strip().splitlines()[0][:60]
            items.append((os.path.basename(os.path.dirname(label)) if label.endswith("SKILL.md") else label, m.group(0)))
    elif category == "mcp":
        # Namespaces are not closed elements: each runs until the next one.
        starts = [m for m in re.finditer(r'<namespace\s[^>]*?name="([^"]+)"', body)]
        ends = [m.start() for m in starts[1:]] + [body.find("</dynamic_tool_namespaces>") % (len(body) + 1)]
        items = [(m.group(1), body[m.start():end]) for m, end in zip(starts, ends)]
    else:
        items = []
    return items


# --------------------------------------------------------------------------
# Tree


class Tree:
    def __init__(self):
        self.nodes = []

    def add(self, nid, parent, kind, label, category, text=None, tokens=None, chars=None):
        self.nodes.append({
            "id": nid,
            "parentId": parent,
            "kind": kind,
            "label": label,
            "categoryId": category,
            "estimatedTokens": estimate(text) if tokens is None else tokens,
            "characterCount": len(text) if chars is None else chars,
        })
        return nid

    def rollup(self):
        """Fill group nodes with the sum of their children."""
        kids = {}
        for n in self.nodes:
            kids.setdefault(n["parentId"], []).append(n)

        def total(n):
            ch = kids.get(n["id"], [])
            if ch:
                n["estimatedTokens"] = sum(total(c) for c in ch)
                n["characterCount"] = sum(c["characterCount"] for c in ch)
            return n["estimatedTokens"]

        for n in kids.get(None, []):
            total(n)
        return kids


def build_tree(store, message_ids, breakdown):
    tree = Tree()
    server = {c["id"]: c["estimatedTokens"] for c in breakdown}
    for cid, label, _ in CATEGORIES:
        tree.add(f"category:{cid}", None, "category", label, cid, "", 0, 0)

    sections = {cid: 0 for cid, _ in SECTION_TAGS}
    turn, turn_count = "turn:0", 0
    tree.add(turn, "category:conversation", "group", "Before first user message", "conversation", "", 0, 0)

    for i, bid in enumerate(message_ids):
        raw = store.blob(bid)
        if raw is None:
            continue
        msg = json.loads(raw)
        role = msg.get("role")

        if role == "system":
            text = "".join(t for _, t in message_parts(msg))
            tree.add(f"msg:{i}", "category:system_prompt", "message", "System prompt", "system_prompt", text)
            continue

        for j, (label, text) in enumerate(message_parts(msg)):
            # Carve out rules/skills/subagents/mcp sections (desktop xI_).
            for tag, cid in SECTION_TAGS:
                for m in re.finditer(rf"<{tag}>(.*?)</{tag}>", text, re.S):
                    sections[tag] += 1
                    group_id = tree.add(f"section:{tag}:{i}:{sections[tag]}", f"category:{cid}", "group",
                                        tag.replace("_", " ").title(), cid, "", 0, 0)
                    children = split_section(cid, m.group(1))
                    for k, (child_label, child_text) in enumerate(children):
                        tree.add(f"{group_id}:{k}", group_id, "item", child_label, cid, child_text)
                    rest = len(m.group(0)) - sum(len(t) for _, t in children)
                    tree.add(f"{group_id}:rest", group_id, "item",
                             "(section text)" if children else tag, cid, "", round(rest / 4), rest)
                text = re.sub(rf"<{tag}>.*?</{tag}>", "", text, flags=re.S)

            if label == "User Message":
                turn_count += 1
                turn = f"turn:{turn_count}"
                tree.add(turn, "category:conversation", "group", f"Turn {turn_count}", "conversation", "", 0, 0)
            if text.strip():
                tree.add(f"msg:{i}:{j}", turn, "message", label, "conversation", text)

    kids = tree.rollup()
    # Tool definitions are not in the local store; carry the server total.
    for n in tree.nodes:
        if n["id"] == "category:tools":
            n["estimatedTokens"] = server.get("tools", 0)
            n["label"] += " (server-side, no per-tool detail locally)"
        elif n["parentId"] is None and n["categoryId"] in server:
            n["localEstimate"] = n["estimatedTokens"]
            n["estimatedTokens"] = server[n["categoryId"]]
    return tree, kids


# --------------------------------------------------------------------------
# Output


def snapshot(store):
    meta = store.meta()
    root = store.blob(meta.get("latestRootBlobId", ""))
    if root is None:
        return None
    td = decode_token_details(root)
    tree, _ = build_tree(store, td["messageIds"], td["categories"])
    used, cap = td["usedTokens"], td["maxTokens"]
    return {
        "store": store.path,
        "agentId": meta.get("agentId"),
        "name": meta.get("name"),
        "rootBlobId": meta.get("latestRootBlobId"),
        "usedTokens": used,
        "maxTokens": cap,
        "percent": round(used / cap * 100, 1) if cap else None,
        "breakdown": td["categories"],
        "tree": {"schemaVersion": 1, "nodes": tree.nodes},
    }


# Store text is attacker-controlled (tool results, web pages). Same classes as
# ctxviz's text::is_dangerous: C0 controls except \n and \t, DEL, C1 controls,
# and U+2028/U+2029. Each is replaced by U+FFFD so no escape sequence (CSI,
# OSC, DCS, APC) can reach the terminal. JSON mode needs none of this.
_TERM_UNSAFE = re.compile("[\x00-\x08\x0b-\x1f\x7f-\x9f\u2028\u2029]")


def term_safe(s):
    """Replace control characters (keeping newline and tab) with U+FFFD."""
    return _TERM_UNSAFE.sub("\ufffd", "" if s is None else str(s))


def term_safe_line(s):
    """term_safe for one-line fields: newline and tab become spaces too."""
    return term_safe(s).replace("\n", " ").replace("\t", " ")


def human(n):
    return f"{n / 1000:.1f}K" if n >= 1000 else str(n)


def render(snap, depth, color):
    def c(code, s):
        return f"\x1b[38;5;{code}m{s}\x1b[0m" if color else s

    def dim(s):
        return f"\x1b[2m{s}\x1b[0m" if color else s

    out = []
    used, cap = snap["usedTokens"], snap["maxTokens"]
    out.append(f"Context Usage · {term_safe_line(snap.get('name') or snap['agentId'])}")
    out.append(f"{snap['percent']}% Full{'':<10}~{human(used)} / {human(cap)} Tokens" if cap else "No token data yet.")

    width = 60
    bar = ""
    for cat in snap["breakdown"]:
        w = round(cat["estimatedTokens"] / cap * width) if cap else 0
        bar += c(COLORS.get(cat["id"], 250), "█" * w)
    filled = sum(round(x["estimatedTokens"] / cap * width) for x in snap["breakdown"]) if cap else 0
    out.append(bar + dim("░" * max(0, width - filled)))
    for cat in snap["breakdown"]:
        if cat["estimatedTokens"]:
            out.append(f"  {c(COLORS.get(cat['id'], 250), '■')} {term_safe_line(cat['label']):<26}{human(cat['estimatedTokens']):>8}")

    if depth > 0:
        out.append("")
        out.append("Context Explorer  (children are local ~chars/4 estimates)")
        kids = {}
        for n in snap["tree"]["nodes"]:
            kids.setdefault(n["parentId"], []).append(n)

        def walk(parent, level):
            ch = kids.get(parent, [])
            # Like the desktop: conversation stays chronological, the rest sorts by size.
            if not ch or ch[0]["categoryId"] != "conversation" or parent is None:
                ch = sorted(ch, key=lambda n: -n["estimatedTokens"])
            for n in ch:
                if not n["estimatedTokens"]:
                    continue
                count = len([k for k in kids.get(n["id"], []) if k["estimatedTokens"]])
                label = term_safe_line(n["label"]) + (f" {count}" if count else "")
                pad = "  " * level
                marker = "▾" if count and level + 1 < depth else ("▸" if count else " ")
                line = f"{pad}{marker} {label}"
                out.append(f"{line:<64}~{n['estimatedTokens']:,}")
                if level + 1 < depth:
                    walk(n["id"], level + 1)

        walk(None, 0)
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--chat", help="agent id or path to store.db (default: latest chat for --cwd)")
    ap.add_argument("--cwd", default=os.getcwd(), help="workspace whose latest chat to use")
    ap.add_argument("--latest", action="store_true",
                    help="use the most recently written chat in any workspace (overrides --cwd)")
    ap.add_argument("--json", action="store_true", help="emit JSON (NDJSON with --watch)")
    ap.add_argument("--depth", type=int, default=2, help="explorer tree depth (0 hides it)")
    ap.add_argument("--watch", action="store_true", help="stream: re-emit on every checkpoint")
    ap.add_argument("--interval", type=float, default=0.5, help="poll interval for --watch")
    args = ap.parse_args()

    color = sys.stdout.isatty() and not os.environ.get("NO_COLOR")

    def emit(snap):
        if args.json:
            print(json.dumps(snap), flush=True)
        else:
            if args.watch and color:
                print("\x1b[2J\x1b[H", end="")
            print(render(snap, args.depth, color), flush=True)

    if not args.watch:
        snap = snapshot(Store(find_store(args.chat, args.cwd, args.latest)))
        if snap is None:
            sys.exit("chat has no checkpoint yet")
        emit(snap)
        return

    last = None
    try:
        while True:
            # Follow new chats in the workspace unless one was pinned.
            store = Store(find_store(args.chat, args.cwd, args.latest))
            if last is None or store.path != last[0]:
                print(f"watching {term_safe_line(store.path)}", file=sys.stderr, flush=True)
            key = (store.path, store.meta().get("latestRootBlobId"))
            if key != last:
                snap = snapshot(store)
                if snap is not None:
                    emit(snap)
                    last = key
            store.db.close()
            time.sleep(args.interval)
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
