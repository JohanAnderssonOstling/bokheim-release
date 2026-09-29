#define COBJMACROS
#include <windows.h>
#include <d3dcompiler.h>
#include <stdio.h>
int main(int argc, char **argv) {
    if (argc != 5) return 2;
    WCHAR filename[32768];
    if (!MultiByteToWideChar(CP_UTF8, 0, argv[1], -1, filename, 32768)) return 3;
    ID3DBlob *code = NULL, *errors = NULL;
    HRESULT hr = D3DCompileFromFile(filename, NULL, D3D_COMPILE_STANDARD_FILE_INCLUDE,
        argv[2], argv[3], D3DCOMPILE_OPTIMIZATION_LEVEL3, 0, &code, &errors);
    if (errors) {
        fwrite(ID3D10Blob_GetBufferPointer(errors), 1, ID3D10Blob_GetBufferSize(errors), stderr);
        ID3D10Blob_Release(errors);
    }
    if (FAILED(hr)) return 4;
    FILE *output = fopen(argv[4], "wb");
    if (!output) return 5;
    size_t size = ID3D10Blob_GetBufferSize(code);
    int ok = fwrite(ID3D10Blob_GetBufferPointer(code), 1, size, output) == size;
    fclose(output);
    ID3D10Blob_Release(code);
    return ok ? 0 : 6;
}
