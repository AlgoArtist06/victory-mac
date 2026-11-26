"""Keeps `extern "system"` on functions handed to Windows APIs.

Title-facing functions use `extern "win64"` (the Windows x64 convention on
every OS). Windows API callbacks keep `extern "system"`, which is a different
type to rustc. This rebuilds for Windows, finds each "expected system fn,
found win64 fn" error, and changes that function back, until none are left.
Usage: python3 -I mac/fix-abi.py (from the repository root, with the
Windows target's environment set up).
"""
import re
import subprocess
import sys

CHECK = ["cargo", "check", "--target", "x86_64-pc-windows-gnullvm", "-p", "victory",
         "--features", "diagnostics", "--message-format", "short"]
ERROR = re.compile(r'^(\S+?):(\d+):(\d+): error\[E0308\]: mismatched types: expected "system" fn, found "win64" fn')


def names_at(path, line, col):
    text = open(path).read().split("\n")[line - 1][col - 1:]
    return re.findall(r"[A-Za-z_][A-Za-z0-9_]*", text)[:6]


def revert(name):
    pattern = re.compile(r'extern "win64" fn ' + re.escape(name) + r"\b")
    hits = subprocess.run(["grep", "-rl", "--include=*.rs", f'extern "win64" fn {name}', "crates", "apps"],
                          capture_output=True, text=True).stdout.split()
    for path in hits:
        text = open(path).read()
        new = pattern.sub(f'extern "system" fn {name}', text)
        if new != text:
            open(path, "w").write(new)
            return path
    return None


for round in range(50):
    out = subprocess.run(CHECK, capture_output=True, text=True).stderr.splitlines()
    errors = [m.groups() for m in map(ERROR.match, out) if m]
    if not errors:
        other = [l for l in out if ": error" in l]
        print("\n".join(other) if other else "clean")
        sys.exit(1 if other else 0)
    changed = 0
    for path, line, col in errors:
        for name in names_at(path, int(line), int(col)):
            where = revert(name)
            if where:
                print(f"system: {name} ({where})")
                changed += 1
                break
        else:
            print(f"unresolved: {path}:{line}:{col}")
    if not changed:
        sys.exit(1)
