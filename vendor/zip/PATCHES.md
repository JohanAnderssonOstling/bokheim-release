# Local ZIP patch

Based on the published zip 8.6.0 crate, under its included LICENSE.

ZIP64 extended information replaces only ordinary fields containing the
0xFFFFFFFF sentinel. Removed the length-based overrides in
`src/extra_fields/zip64_extended_information.rs`. This preserves valid ordinary
sizes and offsets when EPUB producers emit redundant malformed ZIP64 data.

Regression coverage lives in shared/epub/src/lib.rs (streaming_tests): redundant
ZIP64 fields, genuine sentinel replacements, partial replacements, and CRC checks.
