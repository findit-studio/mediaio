<div align="center">
<h1>mediadecode-webcodecs</h1>
</div>
<div align="center">

WebCodecs adapter for the [`mediadecode`](../mediadecode) abstraction
layer, built on top of
[`web-sys`](https://crates.io/crates/web-sys).

[<img alt="github" src="https://img.shields.io/badge/github-findit--studio/mediaio-8da0cb?style=for-the-badge&logo=Github" height="22">][Github-url]
<img alt="LoC" src="https://img.shields.io/endpoint?url=https%3A%2F%2Fgist.githubusercontent.com%2Fal8n%2F327b2a8aef9003246e45c6e47fe63937%2Fraw%2Fmediadecode-webcodecs" height="22">
[<img alt="Build" src="https://img.shields.io/github/actions/workflow/status/findit-studio/mediaio/ci-webcodecs.yml?logo=Github-Actions&style=for-the-badge" height="22">][CI-url]
[<img alt="codecov" src="https://img.shields.io/codecov/c/gh/findit-studio/mediaio?style=for-the-badge&logo=codecov" height="22">][codecov-url]

[<img alt="docs.rs" src="https://img.shields.io/badge/docs.rs-mediadecode--webcodecs-66c2a5?style=for-the-badge&labelColor=555555" height="22">][doc-url]
[<img alt="crates.io" src="https://img.shields.io/crates/v/mediadecode-webcodecs?style=for-the-badge" height="22">][crates-url]
[<img alt="crates.io" src="https://img.shields.io/crates/d/mediadecode-webcodecs?color=critical&style=for-the-badge" height="22">][crates-url]
<img alt="license" src="https://img.shields.io/badge/License-Apache%202.0/MIT-blue.svg?style=for-the-badge">

</div>

Implements `mediadecode`'s `VideoAdapter` and `AudioAdapter` traits
(plus the matching push-style `*StreamDecoder` traits) on top of the
browser's
[WebCodecs API](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API).

## Target

This crate is **`wasm32`-only**. On non-`wasm32` targets it compiles
to an empty stub so the workspace `cargo build` / `cargo check`
continue to work in native dev loops.

> **Status:** published; the public API is still pre-1.0 and moves
> with `mediadecode`. Track the [CHANGELOG](CHANGELOG.md).

```toml
[dependencies]
mediadecode-webcodecs = "0.20"
```

Built and run via [`wasm-bindgen`](https://crates.io/crates/wasm-bindgen)
on `wasm32-unknown-unknown`. The crate intentionally does not
implement `SubtitleAdapter` — WebCodecs has no subtitle surface;
captions live in JavaScript-side parsers.

### Required cfg flag

`web-sys` gates the WebCodecs APIs behind `--cfg web_sys_unstable_apis`
because the WebIDL is not yet stable across all browsers. Add the
flag to your project's `.cargo/config.toml`:

```toml
[target.wasm32-unknown-unknown]
rustflags = ["--cfg=web_sys_unstable_apis"]
```

…or set `RUSTFLAGS="--cfg=web_sys_unstable_apis"` in your build
environment / CI. Without the flag, the crate emits an explicit
`compile_error!` pointing here.

## Bindings

There is no high-level WebCodecs wrapper crate that meets the
ergonomics bar; the crate uses
[`web-sys`](https://crates.io/crates/web-sys) directly as the FFI
layer (the same source `videocall.rs`, `livekit-rust`, and other
active WebCodecs Rust users build on) and layers an idiomatic
`mediadecode`-shaped surface on top.

## License

`mediadecode-webcodecs` is under the terms of both the MIT license and the
Apache License (Version 2.0).

See [LICENSE-APACHE](LICENSE-APACHE), [LICENSE-MIT](LICENSE-MIT) for details.

Copyright (c) 2026 FinDIT Studio authors.

[Github-url]: https://github.com/findit-studio/mediaio
[CI-url]: https://github.com/findit-studio/mediaio/actions/workflows/ci-webcodecs.yml
[codecov-url]: https://app.codecov.io/gh/findit-studio/mediaio/
[doc-url]: https://docs.rs/mediadecode-webcodecs
[crates-url]: https://crates.io/crates/mediadecode-webcodecs
