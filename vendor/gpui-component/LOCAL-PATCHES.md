# Local widget patches

Source: https://github.com/longbridge/gpui-component
Revision: 88f102d13654fe25aa2fede076274b6b751a3704
License: Apache-2.0 (LICENSE-APACHE included).

Only the UI, assets, and macros crates are included; workspace members are narrowed accordingly. Manifests explicitly use this nested workspace and the existing local GPUI fork. The unused standalone psm Git override is removed; application dependency resolution remains controlled by the main workspace.

- Input and OTP carets remain visible and stop scheduling blink/pause timers when GPUI reports reduced motion.
- Switches skip the animation and its completion timer under reduced motion.
- Scrollbars bypass timer-driven fading under reduced motion, including per-widget auto-hide overrides.
- `Button`'s `Ghost` variant reads the themed `button_hover`/`button_active` tokens for its hover and pressed fills, the same tokens `Default` already reads, instead of deriving an untheme-able tint from `secondary`. Every other variant already honored a theme's hover colors; `Ghost` silently didn't.

Kobo forces reduced motion through its GPUI platform capability. Other platforms retain the existing policy.
