#!/usr/bin/env python3
"""Run read-only shell commands on a Pluto over its serial debug console
(the board's FTDI port, at 115200), for when it is not
reachable on the network. Logs in if a login prompt appears,
runs each command, and prints what the console returns. Standard library
only (termios); no pyserial.

Usage: tools/pluto-console.py --tty TTY --user USER --password PW CMD [CMD ...]
  The login has no default.

It only sends the commands it is given; keep them read-only unless a
change is intended.
"""

import argparse
import os
import select
import sys
import termios
import time


def open_tty(path: str) -> int:
    fd = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    a = termios.tcgetattr(fd)
    a[0] = 0  # iflag: raw
    a[1] = 0  # oflag
    a[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
    a[3] = 0  # lflag: no echo, non-canonical
    a[4] = a[5] = termios.B115200
    a[6][termios.VMIN] = 0
    a[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, a)
    termios.tcflush(fd, termios.TCIOFLUSH)
    return fd


def read_until(fd: int, marks: tuple[bytes, ...], timeout: float) -> bytes:
    """Read until one of `marks` ends the output, or `timeout` passes."""
    out = b""
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                out += os.read(fd, 4096)
            except BlockingIOError:
                pass
            if any(out.rstrip().endswith(m) for m in marks):
                break
    return out


PROMPTS = (b"#", b"$")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--tty", required=True)
    p.add_argument("--user", required=True)
    p.add_argument("--password", required=True)
    p.add_argument("--timeout", type=float, default=10.0, help="seconds per command")
    p.add_argument("cmds", nargs="+")
    a = p.parse_args()
    fd = open_tty(a.tty)
    os.write(fd, b"\r")
    got = read_until(fd, (b"login:",) + PROMPTS, 5)
    if got.rstrip().endswith(b"login:"):
        os.write(fd, a.user.encode() + b"\r")
        read_until(fd, (b"assword:",), 5)
        os.write(fd, a.password.encode() + b"\r")
        got = read_until(fd, PROMPTS + (b"incorrect",), 10)
        if not got.rstrip().endswith(PROMPTS):
            sys.exit("login failed: " + got.decode(errors="replace")[-200:])
    for c in a.cmds:
        os.write(fd, c.encode() + b"\r")
        out = read_until(fd, PROMPTS, a.timeout).decode(errors="replace")
        # drop the echoed command line and the trailing prompt
        lines = out.replace("\r", "").split("\n")
        body = [l for l in lines[1:] if not l.rstrip().endswith(("#", "$"))]
        print(f"$ {c}")
        print("\n".join(body).rstrip())
    os.close(fd)


if __name__ == "__main__":
    main()
