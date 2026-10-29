# Coding preferences

- In Rust, prefer `use` imports for repeated types, traits, and functions rather than repeatedly writing fully qualified `::` paths inline. Keep qualified paths where they clarify ambiguous names or platform-specific code, and follow the surrounding code style.

# Build storage

- Never fall back to SSHFS network mounts for source checkouts, build caches, compiler targets, temporary build files, or build outputs.
- If local storage is insufficient, report the constraint instead of relocating builds to SSHFS.
