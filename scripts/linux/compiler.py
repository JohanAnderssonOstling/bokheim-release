#!/usr/bin/env python3
"""Zig C/C++ and Rust linker entry points for the Linux release SDK."""
import os
from pathlib import Path
import sys


def compiler_arguments(arguments, sdk, linker):
    result = []
    iterator = iter(arguments)
    for argument in iterator:
        if argument in ('-target', '--target'):
            next(iterator)
            continue
        if argument.startswith(('--target=', '-target=')):
            continue
        # pkg-config may report host library paths. They must not take priority
        # over the SDK's ABI-compatible libraries.
        if argument == '-L':
            path = next(iterator)
            if path in ('/usr/lib', '/usr/lib64', '/usr/lib/x86_64-linux-gnu', '/lib', '/lib64'):
                continue
            result += [argument, path]
            continue
        if argument in ('-L/usr/lib', '-L/usr/lib64', '-L/usr/lib/x86_64-linux-gnu', '-L/lib', '-L/lib64'):
            continue
        if linker and argument in ('-lstdc++', '-lgcc_s'):
            # Zig aliases -lstdc++ to its libc++; speech uses GNU's C++ ABI.
            argument = str(sdk / 'lib' / {'-lstdc++': 'libstdc++.so.6', '-lgcc_s': 'libgcc_s.so.1'}[argument])
        result.append(argument)
    if linker and '-o' in result:
        output = Path(result[result.index('-o') + 1])
        if output.name.startswith('build_script_'):
            # Cargo executes these on the build host. SDK sonames (notably
            # CentOS's libbz2.so.1) may differ from the host distribution's.
            result += ['-Wl,-rpath,' + str(sdk / 'lib')]
    return result


def main():
    # Keep the invocation path: resolving this script would lose the SDK's bin/.
    sdk = Path(os.environ.get('BOKHEIM_LINUX_SDK', Path(sys.argv[0]).absolute().parents[1]))
    mode = Path(sys.argv[0]).name
    zig = str(sdk / 'zig/zig')
    environment = {**os.environ, 'ZIG_GLOBAL_CACHE_DIR': str(sdk / 'zig-cache'),
                   'ZIG_LOCAL_CACHE_DIR': str(sdk / 'zig-local-cache')}
    arguments = compiler_arguments(sys.argv[1:], sdk, mode == 'linker')
    if mode == 'c++':
        headers = sdk / 'native/x86_64-conda-linux-gnu/include/c++/12.3.0'
        # Zig adds libc's -isystem paths before user -isystem paths. GNU's
        # include_next wrappers need the C++ headers before those libc paths.
        arguments = ['-nostdinc++', '-nostdlib++', '-I', str(headers), '-I',
                     str(headers / 'x86_64-conda-linux-gnu'), '-I',
                     str(headers / 'backward'), *arguments]
        if not any(flag in arguments for flag in ('-c', '-E', '-S', '-fsyntax-only')):
            arguments += [str(sdk / 'lib/libstdc++.so.6'), str(sdk / 'lib/libgcc_s.so.1')]
    os.execve(zig, [zig, 'c++' if mode == 'c++' else 'cc', '-target',
                   'x86_64-linux-gnu.2.17', '-fno-sanitize=all',
                   '-L' + str(sdk / 'lib'), *arguments], environment)


if __name__ == '__main__':
    main()
