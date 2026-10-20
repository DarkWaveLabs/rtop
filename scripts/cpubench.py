#!/usr/bin/env python3
"""Run a TUI in a pseudo-terminal for a fixed time and report what it cost.

Usage: cpubench.py [--secs N] [--warmup N] [--cols C] [--rows R] [--label L] -- cmd args...

Prints one JSON line: total CPU (wait4 rusage, all threads), steady-state CPU after the
warm-up, context switches, max RSS and the bytes written to the terminal each second.
Steady-state CPU comes from /proc/<pid>/stat on Linux and from `ps -o cputime` elsewhere.
Only the standard library is used, so it runs with the system python3 on macOS and Linux.
"""
import argparse
import fcntl
import json
import os
import pty
import select
import signal
import struct
import subprocess
import sys
import termios
import time


def cputime(pid):
    """CPU seconds of a live process: /proc on Linux (one tick), else ps (10 ms on macOS)."""
    try:
        with open(f"/proc/{pid}/stat", "rb") as f:
            fields = f.read().rsplit(b")", 1)[1].split()
        # utime and stime are fields 14 and 15; fields[0] here is field 3 (state).
        return (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    except (OSError, IndexError, ValueError):
        pass
    out = subprocess.run(["ps", "-o", "cputime=", "-p", str(pid)],
                         capture_output=True, text=True).stdout.strip()
    if not out:
        return None
    # [[dd-]hh:]mm:ss.cc
    days = 0
    if "-" in out:
        d, out = out.split("-", 1)
        days = int(d)
    parts = [float(p) for p in out.split(":")]
    secs = 0.0
    for p in parts:
        secs = secs * 60 + p
    return days * 86400 + secs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--secs", type=float, default=60)
    ap.add_argument("--warmup", type=float, default=5)
    ap.add_argument("--cols", type=int, default=120)
    ap.add_argument("--rows", type=int, default=40)
    ap.add_argument("--label", default="")
    ap.add_argument("--quit", default="q")
    ap.add_argument("cmd", nargs=argparse.REMAINDER)
    a = ap.parse_args()
    cmd = a.cmd[1:] if a.cmd and a.cmd[0] == "--" else a.cmd

    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.environ["COLUMNS"] = str(a.cols)
        os.environ["LINES"] = str(a.rows)
        os.execvp(cmd[0], cmd)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", a.rows, a.cols, 0, 0))

    start = time.monotonic()
    out_bytes = 0
    warm_cpu = None
    warm_bytes = 0
    end = start + a.secs
    while True:
        now = time.monotonic()
        if warm_cpu is None and now - start >= a.warmup:
            warm_cpu = cputime(pid)
            warm_at = now
            warm_bytes = out_bytes
        if now >= end:
            break
        r, _, _ = select.select([fd], [], [], min(0.2, end - now))
        if r:
            try:
                out_bytes += len(os.read(fd, 65536))
            except OSError:
                break
    end_cpu = cputime(pid)
    end_at = time.monotonic()
    os.write(fd, a.quit.encode())
    # Keep draining so the child is not blocked on a full pty while it exits.
    deadline = time.monotonic() + 5
    status = None
    while time.monotonic() < deadline:
        wpid, st, ru = os.wait4(pid, os.WNOHANG)
        if wpid == pid:
            status = (st, ru)
            break
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                os.read(fd, 65536)
            except OSError:
                pass
    if status is None:
        os.kill(pid, signal.SIGTERM)
        _, st, ru = os.wait4(pid, 0)
        status = (st, ru)
    _, ru = status
    wall = end_at - start
    total_cpu = ru.ru_utime + ru.ru_stime
    steady = None
    if warm_cpu is not None and end_cpu is not None:
        steady = (end_cpu - warm_cpu) / (end_at - warm_at) * 100
    result = {
        "label": a.label or " ".join(cmd),
        "wall_s": round(wall, 1),
        "cpu_total_s": round(total_cpu, 3),
        "user_s": round(ru.ru_utime, 3),
        "sys_s": round(ru.ru_stime, 3),
        "cpu_pct_total": round(total_cpu / wall * 100, 3),
        "cpu_pct_steady": None if steady is None else round(steady, 3),
        "vol_csw": ru.ru_nvcsw,
        "invol_csw": ru.ru_nivcsw,
        "csw_per_s": round((ru.ru_nvcsw + ru.ru_nivcsw) / wall, 1),
        "maxrss_mb": round(ru.ru_maxrss / (1024 * 1024 if sys.platform == "darwin" else 1024), 1),
        "tty_bytes_per_s": round((out_bytes - warm_bytes) / max(0.001, end_at - (warm_at if warm_cpu is not None else start))),
    }
    print(json.dumps(result))


if __name__ == "__main__":
    main()
