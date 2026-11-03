#!/usr/bin/env python3
"""Copy the shared-library closure of a binary into an AppImage AppDir.

Graphics drivers and glibc stay with the host: bundling libGL/libEGL/libdrm
would fight the user's GPU stack, and the C library must match the host's
loader.
"""

import os
import shutil
import subprocess
import sys

EXCLUDE_EXACT = {
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libm.so.6",
    "libpthread.so.0",
    "libdl.so.2",
    "librt.so.1",
    "libresolv.so.2",
    "libutil.so.1",
    "libanl.so.1",
}

EXCLUDE_PREFIXES = (
    "ld-linux",
    "ld-2.",
    "libGL.so",
    "libGLX",
    "libGLdispatch",
    "libOpenGL",
    "libEGL",
    "libGLES",
    "libgbm",
    "libdrm",
    "libvulkan",
    "libnvidia",
    "libcuda",
    "libX11.so",
    "libxcb.so",
)


def ldd_paths(path):
    result = subprocess.run(["ldd", path], capture_output=True, text=True)
    for line in result.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        if "not found" in line:
            print("warning: missing dependency:", line, file=sys.stderr)
            continue
        if "=>" in line:
            parts = line.split()
            if len(parts) < 3:
                continue
            candidate = parts[2]
        else:
            parts = line.split()
            if not parts:
                continue
            candidate = parts[0]
        if os.path.exists(candidate):
            yield candidate


def main():
    if len(sys.argv) != 3:
        print("usage: bundle_libs.py <binary> <libdir>", file=sys.stderr)
        return 2
    binary, libdir = sys.argv[1], sys.argv[2]
    os.makedirs(libdir, exist_ok=True)

    seen = set()
    queue = [binary]
    while queue:
        current = queue.pop()
        for path in ldd_paths(current):
            real = os.path.realpath(path)
            if real in seen or not os.path.isfile(real):
                continue
            base = os.path.basename(real)
            if base in EXCLUDE_EXACT or base.startswith(EXCLUDE_PREFIXES):
                continue
            # Copy under the name the loader asks for (the soname symlink),
            # not the fully versioned real file, so DT_NEEDED entries resolve
            # inside the AppDir instead of falling back to the host.
            needed_name = os.path.basename(path)
            seen.add(real)
            shutil.copy2(real, os.path.join(libdir, needed_name))
            queue.append(real)

    print(f"bundled {len(seen)} libraries into {libdir}")


if __name__ == "__main__":
    raise SystemExit(main())
