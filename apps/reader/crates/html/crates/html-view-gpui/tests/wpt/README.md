# GPUI WPT Reftests

This directory owns the GPUI reftest selection, backend-specific exclusions,
known-failure ledger, causes, and timeouts.

Set `HTML_WPT_ROOT` to the absolute path of a checkout at the upstream revision
in `revision.txt`. The unchanged WPT documents, references, fonts, and images
are read from that checkout at runtime and are not duplicated in this crate.
