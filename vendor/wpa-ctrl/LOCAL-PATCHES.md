# Local changes to wpa-ctrl 0.4.0

Source: https://github.com/DoumanAsh/wpa-ctrl, crates.io release 0.4.0.
License: Boost Software License 1.0; see LICENSE.

The Kobo backend uses this path dependency for command transmission and reception.
The upstream source is retained with these small changes:

- `WpaController::from_socket` accepts an owned socket and cleanup path. This lets
  Kobo retain its mode-0700 private directory and actual read/write timeouts.
  Upstream 0.4.0's builder exposes a read timeout but does not apply it when opening.
- `set_nonblocking` allows the independent event monitor to drain without waiting.
- `recv` removes only newline terminators, preserving SSID spaces and empty final
  scan columns. Upstream `trim()` loses these bytes.
- `recv` rejects a response filling the caller's buffer, since it may be truncated.

Kobo supplies a 64-KiB response buffer and checks the 127-byte command limit before
calling `WpaControlReq::raw`. It retains its SSID/security parsers and redacted errors;
upstream high-level join errors can include credentials. Regression tests live in
apps/kobo-device/src/wifi. Revisit these patches when upgrading upstream.
