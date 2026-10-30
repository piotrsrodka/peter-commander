#!/usr/bin/env python3
"""Run a command in a pty and pass keystrokes through in coalesced chunks.

vhs has no F-keys, so docs/demo.tape types the escape sequences itself
("Escape" then "OR" for F3). vhs delivers those as separate writes, which
the app reads as a lone Esc followed by plain text. This proxy waits a
moment after each read and forwards everything that arrived as one write,
the way a real terminal sends an F-key.

Usage: keyproxy.py <command> [args...]
"""

import fcntl
import os
import pty
import select
import sys
import termios
import tty

GATHER_SECONDS = 0.15


def main() -> int:
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(sys.argv[1], sys.argv[1:])

    size = fcntl.ioctl(sys.stdin.fileno(), termios.TIOCGWINSZ, b"\0" * 8)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, size)

    saved = termios.tcgetattr(sys.stdin.fileno())
    tty.setraw(sys.stdin.fileno())
    try:
        while True:
            ready, _, _ = select.select([sys.stdin, fd], [], [])
            if fd in ready:
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    break
                if not data:
                    break
                os.write(sys.stdout.fileno(), data)
            if sys.stdin in ready:
                chunk = os.read(sys.stdin.fileno(), 1024)
                while select.select([sys.stdin], [], [], GATHER_SECONDS)[0]:
                    chunk += os.read(sys.stdin.fileno(), 1024)
                os.write(fd, chunk)
    finally:
        termios.tcsetattr(sys.stdin.fileno(), termios.TCSADRAIN, saved)
    _, status = os.waitpid(pid, 0)
    return os.waitstatus_to_exitcode(status)


if __name__ == "__main__":
    sys.exit(main())
