//! Normalized event streams for `tools/t3-diff`.
//!
//! A differential scenario runs once under T3 Code and once under the engine;
//! both sides are reduced to the same JSON lines here, then compared. Keep the
//! normalization independent of provider noise (ids, timings) and stable
//! across runs.
