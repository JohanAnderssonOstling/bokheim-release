#!/usr/bin/env python3
"""Keep system GUI headers while adding the browser's isolated library search path."""
import os
import subprocess
import sys


def main():
    result = subprocess.run([os.environ['BOKHEIM_BROWSER_PKG_CONFIG'], *sys.argv[1:]], stdout=subprocess.PIPE)
    sys.stdout.buffer.write(result.stdout)
    if result.returncode == 0 and any(argument in ('--libs', '--libs-only-L') for argument in sys.argv[1:]):
        path = os.environ['BOKHEIM_BROWSER_LIBRARY_PATH']
        # Match pkg-config's backslash escaping, including paths containing spaces.
        escaped = ''.join('\\' + character if character.isspace() or character == '\\' else character for character in path)
        sys.stdout.write(' -L' + escaped + '\n')
    return result.returncode


if __name__ == '__main__':
    sys.exit(main())
