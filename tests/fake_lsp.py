#!/usr/bin/env python3
"""A tiny scripted language server used by tests/e2e.rs (standard library only).

- keeps its own copy of every document, applying incremental changes exactly as the spec says,
  and writes that copy to the file named by argv[1] after every change
- publishes an error for each "BAD" and a warning for each "WARN"
- hover / definition / references / rename / formatting / completion on plain words
- touches argv[1] + ".exit" when it receives `exit`
"""
import json
import re
import sys

dump = sys.argv[1] if len(sys.argv) > 1 else None
text = ""
uri = None


def read_msg():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        line = line.decode().strip()
        if not line:
            break
        k, v = line.split(":", 1)
        headers[k.lower()] = v.strip()
    n = int(headers["content-length"])
    return json.loads(sys.stdin.buffer.read(n).decode("utf-8"))


def send(obj):
    b = json.dumps(obj).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(b) + b)
    sys.stdout.buffer.flush()


def offset(t, pos):
    parts = t.split("\n")
    line, ch = pos["line"], pos["character"]
    if line >= len(parts):
        return len(t)
    return min(sum(len(p) + 1 for p in parts[:line]) + ch, len(t))


def apply_change(t, ch):
    if "range" not in ch:
        return ch["text"]
    a = offset(t, ch["range"]["start"])
    b = offset(t, ch["range"]["end"])
    return t[:a] + ch["text"] + t[b:]


def save_dump():
    if dump:
        with open(dump, "w") as f:
            f.write(text)


def rng(line, a, b):
    return {"start": {"line": line, "character": a}, "end": {"line": line, "character": b}}


def diagnostics():
    out = []
    for i, line in enumerate(text.split("\n")):
        for m in re.finditer("BAD", line):
            out.append({"range": rng(i, m.start(), m.end()), "severity": 1, "message": "bad word found", "source": "fake"})
        for m in re.finditer("WARN", line):
            out.append({"range": rng(i, m.start(), m.end()), "severity": 2, "message": "careful now", "source": "fake"})
    send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": out}})


def word_at(pos):
    lines = text.split("\n")
    if pos["line"] >= len(lines):
        return None
    for m in re.finditer(r"\w+", lines[pos["line"]]):
        if m.start() <= pos["character"] < m.end() or m.end() == pos["character"]:
            return m.group(0)
    return None


def occurrences(word):
    res = []
    for i, line in enumerate(text.split("\n")):
        for m in re.finditer(r"\b%s\b" % re.escape(word), line):
            res.append(rng(i, m.start(), m.end()))
    return res


def handle(msg):
    global text, uri
    method = msg.get("method")
    mid = msg.get("id")
    p = msg.get("params", {})
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": mid, "result": {"capabilities": {
            "textDocumentSync": {"openClose": True, "change": 2},
            "hoverProvider": True,
            "completionProvider": {"triggerCharacters": ["."]},
            "definitionProvider": True,
            "referencesProvider": True,
            "renameProvider": True,
            "documentFormattingProvider": True,
        }}})
    elif method == "textDocument/didOpen":
        uri = p["textDocument"]["uri"]
        text = p["textDocument"]["text"]
        save_dump()
        diagnostics()
    elif method == "textDocument/didChange":
        for ch in p["contentChanges"]:
            text = apply_change(text, ch)
        save_dump()
        diagnostics()
    elif method == "textDocument/hover":
        w = word_at(p["position"])
        if w is None:
            send({"jsonrpc": "2.0", "id": mid, "result": None})
        else:
            send({"jsonrpc": "2.0", "id": mid, "result": {"contents": {"kind": "markdown", "value": "```\nword: %s\n```\nfake docs" % w}}})
    elif method == "textDocument/definition":
        w = word_at(p["position"])
        res = []
        for i, line in enumerate(text.split("\n")):
            m = re.search(r"def\s+(%s)\b" % re.escape(w or "\0"), line)
            if m:
                res.append({"uri": uri, "range": rng(i, m.start(1), m.end(1))})
                break
        send({"jsonrpc": "2.0", "id": mid, "result": res})
    elif method == "textDocument/references":
        w = word_at(p["position"])
        res = [{"uri": uri, "range": r} for r in occurrences(w)] if w else []
        send({"jsonrpc": "2.0", "id": mid, "result": res})
    elif method == "textDocument/rename":
        w = word_at(p["position"])
        edits = [{"range": r, "newText": p["newName"]} for r in occurrences(w)] if w else []
        send({"jsonrpc": "2.0", "id": mid, "result": {"changes": {uri: edits}}})
    elif method == "textDocument/formatting":
        edits = []
        for i, line in enumerate(text.split("\n")):
            stripped = line.rstrip()
            if stripped != line:
                edits.append({"range": rng(i, len(stripped), len(line)), "newText": ""})
        send({"jsonrpc": "2.0", "id": mid, "result": edits})
    elif method == "textDocument/completion":
        # the word before the cursor, as the range a real server (clangd) would ask to replace
        pos = p["position"]
        line = text.split("\n")[pos["line"]] if pos["line"] < len(text.split("\n")) else ""
        a = pos["character"]
        while a > 0 and (line[a - 1].isalnum() or line[a - 1] == "_"):
            a -= 1
        word = rng(pos["line"], a, pos["character"])
        # inside `#include <...` / `#include "..."` clangd offers file names, each with its own closer
        m = re.match(r'^\s*#\s*(?:include|include_next|import)\s*([<"])([^>"]*)$', line[:pos["character"]])
        if m:
            closer = ">" if m.group(1) == "<" else '"'
            part = m.group(2)
            base = pos["character"] - len(part.split("/")[-1])
            span = rng(pos["line"], base, pos["character"])
            names = ["stat.h", "types.h"] if "/" in part else ["stdio.h", "stdio_ext.h", "stdlib.h", "sys/"]
            items = [{"label": n + ("" if n.endswith("/") else closer), "kind": 19 if n.endswith("/") else 17,
                      "textEdit": {"range": span, "newText": n + ("" if n.endswith("/") else closer)}} for n in names]
            send({"jsonrpc": "2.0", "id": mid, "result": {"isIncomplete": False, "items": items}})
            return
        send({"jsonrpc": "2.0", "id": mid, "result": {"isIncomplete": False, "items": [
            {"label": "alpha_item", "kind": 3, "detail": "fn alpha_item()"},
            {"label": "alpine", "kind": 6},
            {"label": "beta_item", "kind": 3},
            # snippets (insertTextFormat 2), shaped like clangd's `#include` and function completions
            {"label": "include \"header\"", "kind": 15, "filterText": "include", "insertTextFormat": 2,
             "textEdit": {"range": word, "newText": "include \"${1:header}\""}},
            {"label": "include <header>", "kind": 15, "filterText": "include", "insertTextFormat": 2,
             "textEdit": {"range": word, "newText": "include <${1:header}>"}},
            # clangd's own form: no default text, the cursor goes between the brackets
            {"label": "include_next <header>", "kind": 15, "filterText": "include_next", "insertTextFormat": 2,
             "textEdit": {"range": word, "newText": "include_next <$0>"}},
            {"label": "snipcall(int a, int b)", "kind": 3, "filterText": "snipcall", "insertTextFormat": 2,
             "insertText": "snipcall(${1:a}, ${2:b})$0"},
        ]}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": mid, "result": None})
    elif method == "exit":
        if dump:
            open(dump + ".exit", "w").close()
        sys.exit(0)
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "result": None})


while True:
    m = read_msg()
    if m is None:
        break
    handle(m)
