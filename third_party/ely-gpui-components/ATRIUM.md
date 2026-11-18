# Ely GPUI Components, as Atrium builds it

A copy of [Ely GPUI Components](https://github.com/ZacharyZhang-NY/Ely-GPUI-Components)
at revision `5d6efd46ec0c93b381ec7578561a1273c366025b` (MIT, see `LICENSE`),
used through `[patch]` in the workspace's `Cargo.toml`.

Atrium's changes, meant to go upstream:

- Chat and agent components read their copy through `crate::i18n::text`
  instead of fixed English, with English and Simplified Chinese entries in
  `src/i18n/builtin.rs`: conversation list, welcome greeting, composer,
  thinking and error rows, message actions, permission prompt, tool calls,
  file changes.

Examples and the web gallery are left out of this copy.
