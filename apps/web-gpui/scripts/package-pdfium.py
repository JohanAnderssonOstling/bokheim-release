#!/usr/bin/env python3
"""Compatibility entry point; all PDFium artifact handling lives in shared/pdfium."""
from pathlib import Path
import os
import runpy
import sys

manager = runpy.run_path(str(Path(__file__).resolve().parents[3] / "shared/pdfium/prepare.py"))
manager["prepare"]("wasm32-unknown-unknown", destination=Path(sys.argv[1]),
                   archive=os.environ.get("PDFIUM_WASM_ARCHIVE"))
