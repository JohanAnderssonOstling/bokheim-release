#!/usr/bin/env python3
"""Execute a command with loopback only in a fresh Linux network namespace."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inside", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("Provide a command after --")
    if not args.inside:
        os.execvp("unshare", ["unshare", "--user", "--map-root-user", "--net", sys.executable,
            str(Path(__file__).resolve()), "--inside", "--report", str(args.report.resolve()), "--", *command])
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as control:
        request = struct.pack("16sH22x", b"lo", 0)
        flags = struct.unpack("16sH22x", fcntl.ioctl(control, 0x8913, request))[1]
        fcntl.ioctl(control, 0x8914, struct.pack("16sH22x", b"lo", flags | 1))
    interfaces = sorted(name for _, name in socket.if_nameindex())
    if interfaces != ["lo"]:
        raise RuntimeError("Network namespace has a non-loopback interface")
    with socket.socket() as connection:
        connection.settimeout(1)
        if connection.connect_ex(("1.1.1.1", 443)) == 0:
            raise RuntimeError("External networking was not disabled")
    environment = dict(os.environ)
    environment.setdefault("GRADLE_USER_HOME", str(Path.home() / ".gradle"))
    environment.setdefault("CARGO_HOME", str(Path.home() / ".cargo"))
    environment.setdefault("RUSTUP_HOME", str(Path.home() / ".rustup"))
    report = {"network_namespace": os.readlink("/proc/self/ns/net"), "interfaces": interfaces,
        "external_connection_blocked": True, "command_exit_code": None}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    result = subprocess.run(command, env=environment)
    report["command_exit_code"] = result.returncode
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    sys.exit(result.returncode)


if __name__ == "__main__":
    main()
