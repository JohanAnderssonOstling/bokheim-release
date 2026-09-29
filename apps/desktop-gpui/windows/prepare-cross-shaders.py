#!/usr/bin/env python3
"""Compile GPUI Windows shaders with D3DCompile under Wine for Linux release checks.

Native Windows CI uses GPUI's normal fxc build. This prepares real optimized DXBC
for the Linux cross-build whose GPUI build script skips shader compilation.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[3]
MODULES = ['quad', 'shadow', 'path_rasterization', 'path_sprite', 'underline',
           'monochrome_sprite', 'subpixel_sprite', 'polychrome_sprite', 'emoji_rasterization']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out-dir', required=True, type=Path,
                        help='The gpui_windows OUT_DIR reported by Cargo')
    parser.add_argument('--gpui-root', type=Path, default=ROOT.parent / 'GPUI-Fork')
    args = parser.parse_args()
    out = args.out_dir.resolve(strict=True)
    source = args.gpui_root.resolve(strict=True) / 'crates/gpui_windows/src'
    compiler_source = Path(__file__).with_name('compile-shader.c')
    compiler = out / 'compile-shader.exe'
    subprocess.run(['x86_64-w64-mingw32-gcc', '-O2', str(compiler_source), '-o',
                    str(compiler), '-ld3dcompiler'], check=True)

    def windows_path(path):
        return subprocess.check_output(['winepath', '-w', str(path)],
                                       text=True, env={**os.environ, 'WINEDEBUG': '-all'}).strip()

    bindings, sources, outputs = [], {}, {}
    for module in MODULES:
        shader = source / ('color_text_raster.hlsl' if module == 'emoji_rasterization'
                           else 'shaders.hlsl')
        sources[str(shader)] = hashlib.sha256(shader.read_bytes()).hexdigest()
        for kind, profile in [('vertex', 'vs_4_1'), ('fragment', 'ps_4_1')]:
            binary = out / f'{module}_{kind}.cso'
            subprocess.run(['wine', str(compiler), windows_path(shader), f'{module}_{kind}',
                            profile, windows_path(binary)], check=True)
            data = binary.read_bytes()
            if data[:4] != b'DXBC':
                raise RuntimeError(f'Shader compiler did not emit DXBC: {binary}')
            outputs[binary.name] = hashlib.sha256(data).hexdigest()
            bindings.append(f'const {module.upper()}_{kind.upper()}_BYTES: &[u8] = '
                            f'include_bytes!({json.dumps(str(binary))});')
    # Publish only after every entry point compiled successfully.
    temporary = out / 'shaders_bytes.rs.writing'
    temporary.write_text('\n'.join(bindings) + '\n')
    temporary.replace(out / 'shaders_bytes.rs')
    (out / 'shader-provenance.json').write_text(json.dumps({
        'source_sha256': sources, 'output_sha256': outputs,
        'compiler_source_sha256': hashlib.sha256(compiler_source.read_bytes()).hexdigest(),
        'optimization': 'D3DCOMPILE_OPTIMIZATION_LEVEL3',
        'compiler': 'D3DCompileFromFile via Wine',
    }, indent=2) + '\n')
    print('Compiled all 18 optimized shader entry points.')


if __name__ == '__main__':
    main()
