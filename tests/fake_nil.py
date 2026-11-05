#!/usr/bin/env python3
"""A strict stand-in for `nil` (the Nix language server), used by tests/lsp_sync.rs.

The real nil cannot be built or run in the test environment, so this reproduces the behaviour of
its source (crates/nil/src/{server,vfs,convert}.rs) that matters to a client:

* INCREMENTAL document sync with UTF-16 columns and *no clamping*: a range whose line or column does
  not exist makes the change fail, and the server then silently forgets the whole document ("File is
  out of sync!"); later changes for it are ignored and requests fail with "File not loaded".
* text is normalised (`\\r` removed) when it is stored
* diagnostics are published only when the set of diagnostics for a file changed
* after `initialized` it sends requests of its own (registerCapability, workDoneProgress/create,
  workspace/configuration) and expects answers

Test hooks, all driven by words in the document:
  CRASHME   the process exits at once (as if it had panicked)
  FORGETME  the document is dropped as if a change had failed to apply

argv[1] is a directory; the server writes there:
  text      the document as the server holds it (rewritten after every change)
  dropped   created when the server dropped a document
  starts    one line appended per process start
  log       protocol notes (errors it noticed)
"""
import json
import os
import re
import sys
import time

out_dir = sys.argv[1]
files = {}  # uri -> text
published = {}  # uri -> last diagnostics (as JSON text)
next_id = 1000


def path(name):
    return os.path.join(out_dir, name)


def log(msg):
    with open(path("log"), "a") as f:
        f.write(msg + "\n")


with open(path("starts"), "a") as f:
    f.write("start\n")


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


def utf16_len(ch):
    return 2 if ord(ch) > 0xFFFF else 1


class OutOfSync(Exception):
    pass


def offset(text, pos):
    """Strict LSP position -> index into text, like nil's LineMap: nothing is clamped."""
    lines = text.split("\n")
    line, col = pos["line"], pos["character"]
    if line >= len(lines):
        raise OutOfSync("line %d of %d" % (line, len(lines)))
    acc = 0
    idx = sum(len(l) + 1 for l in lines[:line])
    for ch in lines[line]:
        if acc == col:
            return idx
        if acc > col:
            raise OutOfSync("inside a surrogate pair")
        acc += utf16_len(ch)
        idx += 1
    if acc == col:
        return idx
    raise OutOfSync("column %d past the end of line %d" % (col, line))


def drop(uri, why):
    log("File is out of sync! %s: %s" % (uri, why))
    files.pop(uri, None)
    published.pop(uri, None)
    open(path("dropped"), "w").write(uri)


def dump(uri):
    if uri in files:
        with open(path("text"), "w") as f:
            f.write(files[uri])


def rng(line, a, b):
    return {"start": {"line": line, "character": a}, "end": {"line": line, "character": b}}


def diagnostics(uri):
    out = []
    for i, line in enumerate(files[uri].split("\n")):
        for m in re.finditer("BAD", line):
            # LSP columns are UTF-16 code units
            a = sum(utf16_len(c) for c in line[: m.start()])
            b = sum(utf16_len(c) for c in line[: m.end()])
            out.append({"range": rng(i, a, b), "severity": 1, "message": "bad word", "source": "nil"})
    key = json.dumps(out)
    if published.get(uri) != key:  # nil only republishes when something changed
        published[uri] = key
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": out, "version": None}})


def after_change(uri):
    text = files[uri]
    if "CRASHME" in text:
        sys.stderr.write("thread 'main' panicked at crates/nil/src/handler.rs: test crash\n")
        sys.stderr.flush()
        os._exit(101)
    if "FORGETME" in text:
        drop(uri, "told to forget")
        return
    dump(uri)
    diagnostics(uri)


def error(id_, code, msg):
    send({"jsonrpc": "2.0", "id": id_, "error": {"code": code, "message": msg}})


def word_at(text, pos):
    lines = text.split("\n")
    if pos["line"] >= len(lines):
        return None
    for m in re.finditer(r"[\w'-]+", lines[pos["line"]]):
        if m.start() <= pos["character"] <= m.end():
            return m.group(0)
    return None


def handle(msg):
    global next_id
    method = msg.get("method")
    id_ = msg.get("id")
    p = msg.get("params", {})
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": id_, "result": {
            "capabilities": {
                "textDocumentSync": {"openClose": True, "change": 2},
                "completionProvider": {"triggerCharacters": [".", "?"]},
                "hoverProvider": True,
                "definitionProvider": True,
                "referencesProvider": True,
                "renameProvider": {"prepareProvider": True},
                "documentFormattingProvider": True,
            },
            "serverInfo": {"name": "fake-nil"},
        }})
    elif method == "initialized":
        # nil's habits: ask for configuration, register capabilities, report progress
        send({"jsonrpc": "2.0", "id": "reg-1", "method": "client/registerCapability", "params": {"registrations": []}})
        send({"jsonrpc": "2.0", "id": "prog-1", "method": "window/workDoneProgress/create", "params": {"token": "t1"}})
        send({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "t1", "value": {"kind": "begin", "title": "Loading"}}})
        send({"jsonrpc": "2.0", "id": "cfg-1", "method": "workspace/configuration", "params": {"items": [{"section": "nil"}]}})
        send({"jsonrpc": "2.0", "method": "$/progress", "params": {"token": "t1", "value": {"kind": "end"}}})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": id_, "result": None})
    elif method == "exit":
        open(path("exit"), "w").write("exit")
        sys.exit(0)
    elif method == "textDocument/didOpen":
        d = p["textDocument"]
        files[d["uri"]] = d["text"].replace("\r", "")
        published.pop(d["uri"], None)
        after_change(d["uri"])
    elif method == "textDocument/didClose":
        files.pop(p["textDocument"]["uri"], None)
    elif method == "textDocument/didChange":
        uri = p["textDocument"]["uri"]
        if uri not in files:
            return  # "Ignore files not maintained in Vfs"
        for ch in p["contentChanges"]:
            try:
                if "range" in ch:
                    a = offset(files[uri], ch["range"]["start"])
                    b = offset(files[uri], ch["range"]["end"])
                    if a > b:
                        raise OutOfSync("inverted range")
                    files[uri] = files[uri][:a] + ch["text"].replace("\r", "") + files[uri][b:]
                else:
                    files[uri] = ch["text"].replace("\r", "")
            except OutOfSync as e:
                drop(uri, str(e))
                return
        after_change(uri)
    elif method in ("textDocument/hover", "textDocument/definition", "textDocument/references", "textDocument/completion",
                    "textDocument/formatting", "textDocument/rename"):
        uri = p["textDocument"]["uri"]
        if uri not in files:
            error(id_, -32603, "File not loaded: %s" % uri)
            return
        text = files[uri]
        if method == "textDocument/hover":
            w = word_at(text, p["position"])
            send({"jsonrpc": "2.0", "id": id_, "result": {"contents": {"kind": "markdown", "value": "word: %s" % w}} if w else None})
        elif method == "textDocument/completion":
            send({"jsonrpc": "2.0", "id": id_, "result": {"isIncomplete": False, "items": [
                {"label": "mkIf", "kind": 3, "textEdit": {"range": rng(p["position"]["line"], p["position"]["character"], p["position"]["character"]), "newText": "mkIf"}},
                {"label": "home-manager", "kind": 6, "textEdit": {"range": rng(p["position"]["line"], max(0, p["position"]["character"] - 4), p["position"]["character"]), "newText": "home-manager"}},
            ]}})
        elif method == "textDocument/formatting":
            time.sleep(0.2)
            # "format": upper-case the first character of the first line
            first = text.split("\n")[0]
            if first:
                send({"jsonrpc": "2.0", "id": id_, "result": [{"range": rng(0, 0, 1), "newText": first[0].upper()}]})
            else:
                send({"jsonrpc": "2.0", "id": id_, "result": []})
        elif method == "textDocument/rename":
            time.sleep(0.2)
            send({"jsonrpc": "2.0", "id": id_, "result": {"changes": {uri: [{"range": rng(0, 0, 1), "newText": "R"}]}}})
        else:
            send({"jsonrpc": "2.0", "id": id_, "result": []})
    elif id_ is not None and method is None:
        # an answer to one of our requests
        log("answer %s" % id_)
    elif id_ is not None:
        error(id_, -32601, "method not found")


def main():
    while True:
        msg = read_msg()
        if msg is None:
            return
        handle(msg)


if __name__ == "__main__":
    main()
