# Contributing

## Workflow

Work proceeds one phase at a time, in the order in `docs/DESIGN.md`
section 8. A phase is complete only when it compiles, passes
`cargo fmt --check`, `cargo clippy --all-targets -D warnings`, and
`cargo test --workspace`.

## Adding a codec

1. Add the parser module under `crates/codec-whatttttttttttt-core/src/`,
   named after the spec domain (`flac_frame`, `av1_obu`), never a
   generic name such as `utils` or `helpers`.
2. Reuse `BitReader` for all bit-level input. Do not write a second
   bit reader.
3. Enforce limits from `limits.rs` at the trust boundary.
4. Return the typed `Error`; never panic on untrusted input, never
   `unwrap` or `expect` in library code.
5. Add a test vector to `tests/data/` with origin and license in
   `tests/data/README.md`, plus truncated and corrupt variants.
6. Wire the codec into `Decoder`/`Encoder` and record the change in
   `CHANGELOG.md`.

## Commit format

Conventional Commits with a scope naming the module, for example:

```text
feat(flac): parse frame header
fix(ogg): reject pages with bad crc
test(wav): truncated header returns error
docs(design): record av1 before vp9 decision
```

## Style

- Comments explain why, with a spec section reference where one
  applies. Comments that restate the code are removed in review.
- Public API functions document their `# Errors` cases.
- No emoji or ASCII banners in code or documentation.
