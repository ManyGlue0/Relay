"""Relay's guard for Python in Auto mode (see docs/design/agent-mode.md).

Loaded by Relay's sitecustomize.py before any of the program's code runs, it
installs an audit hook (PEP 578) that applies Auto's path map as the code
runs: no reading credential locations, no writing outside the folder and the
scratch areas, no deleting outside them. New processes are judged by Relay
itself (the same rules as commands), over a loopback connection with the
session's token. An audit hook can't be removed once installed.

Its settings come in RELAY_GUARD (JSON): folder, credentials, scratch,
port, token, bypass. Child processes inherit them, so their Pythons are
guarded too.
"""

import json
import os
import sys
import threading

_state = threading.local()
_installed = False


def _norm(p):
    try:
        if isinstance(p, int):
            return None
        p = os.fsdecode(p)
        return os.path.normcase(os.path.realpath(os.path.abspath(p)))
    except Exception:
        return None


def _within(p, root):
    if not p or not root:
        return False
    root = root.rstrip("\\/")
    return p == root or p.startswith(root + os.sep)


class _Guard:
    def __init__(self, cfg):
        n = lambda x: os.path.normcase(os.path.realpath(x))
        self.folder = n(cfg.get("folder") or os.getcwd())
        self.credentials = [n(c) for c in cfg.get("credentials", []) if c]
        self.scratch = [n(s) for s in cfg.get("scratch", []) if s]
        # Inside the folder, tools' own areas may be cleaned (a venv being
        # upgraded, caches).
        self.folder_scratch = [os.path.join(self.folder, d) for d in (".venv", "venv", "node_modules", ".pytest_cache", ".mypy_cache", ".ruff_cache", ".tox", "build", "dist")]
        self.port = int(cfg.get("port") or 0)
        self.token = str(cfg.get("token") or "")
        self.bypass = bool(cfg.get("bypass"))

    def zone(self, p):
        # A credential location holding the folder (a workspace inside Relay's
        # data) doesn't cover the folder itself; one inside the folder does.
        inside = _within(p, self.folder)
        if any(_within(p, c) and not (inside and _within(self.folder, c)) for c in self.credentials):
            return "credential"
        if _within(p, self.folder):
            return "folder"
        if any(_within(p, s) for s in self.scratch):
            return "scratch"
        return "protected"

    def deletable(self, p):
        z = self.zone(p)
        if z == "scratch":
            return True
        if z == "folder":
            parts = p[len(self.folder):].split(os.sep)
            return "__pycache__" in parts or any(_within(p, s) for s in self.folder_scratch)
        return False


def _deny(rule, why, instead):
    raise PermissionError(f"Blocked by Relay's Auto mode. Rule: {rule}. Why: {why} Instead: {instead}")


_WRITE_FLAGS = os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_APPEND | os.O_TRUNC


def _check_path(g, path, write):
    p = _norm(path)
    if p is None:
        return
    z = g.zone(p)
    if z == "credential":
        _deny("reading credentials", f"{p} is where keys, tokens or passwords are kept.", "work without it.")
    if write and not g.bypass and z == "protected":
        _deny("writing outside the folder", f"{p} is outside {g.folder}.", "write inside the folder.")


def _check_delete(g, path, what):
    p = _norm(path)
    if p is None or g.bypass:
        if p is not None and g.zone(p) == "credential":
            _deny("reading credentials", f"{p} is where keys, tokens or passwords are kept.", "work without it.")
        return
    if not g.deletable(p):
        _deny("deleting", f"Auto mode doesn't delete files ({what} {p}).", "leave it, and mention what could be deleted.")


def _ask(g, line, cwd):
    """Relay judges a new process's command line."""
    if g.bypass:
        return
    import socket

    try:
        with socket.create_connection(("127.0.0.1", g.port), timeout=60) as s:
            s.sendall((json.dumps({"token": g.token, "command": line, "cwd": cwd or os.getcwd()}) + "\n").encode("utf-8"))
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = s.recv(65536)
                if not chunk:
                    break
                buf += chunk
                if len(buf) > 1 << 20:
                    break
        answer = json.loads(buf.decode("utf-8") or "{}")
    except PermissionError:
        raise
    except Exception as e:
        _deny("unreadable command", f"Relay's guard couldn't be reached to check a new process ({e}).", "run the command with the shell tools instead.")
    if not answer.get("ok"):
        raise PermissionError(answer.get("text") or "Blocked by Relay's Auto mode.")


def _cmdline(path, args):
    import subprocess

    try:
        items = [os.fsdecode(path)] + [os.fsdecode(a) for a in (args or [])[1:]]
        return subprocess.list2cmdline(items)
    except Exception:
        return str(path)


def _hook(g):
    def hook(event, args):
        if getattr(_state, "busy", False):
            return
        if not (event == "open" or event.startswith(("os.", "shutil.", "subprocess."))):
            return
        _state.busy = True
        try:
            if event == "open":
                path, mode, flags = (list(args) + [None, None, None])[:3]
                write = (isinstance(mode, str) and any(c in mode for c in "wax+")) or (isinstance(flags, int) and bool(flags & _WRITE_FLAGS))
                _check_path(g, path, write)
            elif event in ("os.remove", "os.rmdir", "shutil.rmtree"):
                _check_delete(g, args[0], "it would delete")
            elif event == "os.rename":
                _check_delete(g, args[0], "moving it away would remove")
                _check_path(g, args[1], True)
            elif event in ("os.truncate", "os.mkdir", "os.symlink", "os.link", "shutil.copyfile", "shutil.copytree", "os.chmod", "os.utime"):
                target = args[1] if event in ("os.symlink", "os.link", "shutil.copyfile", "shutil.copytree") else args[0]
                _check_path(g, target, True)
            elif event == "os.listdir" or event == "os.scandir":
                _check_path(g, args[0], False)
            elif event == "subprocess.Popen":
                executable, cmd, cwd = args[0], args[1], args[2]
                line = cmd if isinstance(cmd, str) else _cmdline(executable or cmd[0], list(cmd))
                _ask(g, os.fsdecode(line) if isinstance(line, bytes) else line, cwd)
            elif event == "os.system":
                _ask(g, os.fsdecode(args[0]), None)
            elif event in ("os.exec", "os.posix_spawn"):
                _ask(g, _cmdline(args[0], list(args[1])), None)
            elif event == "os.spawn":
                _ask(g, _cmdline(args[1], list(args[2])), None)
            elif event == "os.startfile":
                _ask(g, _cmdline(args[0], [args[0]] + ([args[2]] if len(args) > 2 and args[2] else [])), None)
        finally:
            _state.busy = False

    return hook


def install():
    global _installed
    if _installed:
        return
    raw = os.environ.get("RELAY_GUARD")
    if not raw:
        return
    try:
        cfg = json.loads(raw)
    except Exception:
        return
    g = _Guard(cfg)
    sys.addaudithook(_hook(g))
    _installed = True
