# Phosphor Light icons

This directory vendors the subset of Phosphor Icons used by Bokheim's GPUI
interface. The files come from the `light` weight in `phosphor-icons/core` at
commit `2b75f3ad12b420c9504ef05df8d2564a28f8500e`.

The upstream semantic names are preserved here. Bokheim maps its existing
GPUI/Lucide-compatible asset paths to these files in
`apps/components/src/assets.rs`, keeping application call sites
independent of the selected visual icon family.

Phosphor Icons are distributed under the MIT license included in `LICENSE`.
