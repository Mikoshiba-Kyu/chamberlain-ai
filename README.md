# Chamberlain

[![chamberlain-core](https://img.shields.io/crates/v/chamberlain-core?style=flat-square&logo=rust&label=chamberlain-core)](https://crates.io/crates/chamberlain-core)
[![create-chamberlain](https://img.shields.io/npm/v/create-chamberlain?style=flat-square&logo=npm&label=create-chamberlain)](https://www.npmjs.com/package/create-chamberlain)
[![docs.rs](https://img.shields.io/docsrs/chamberlain-core?style=flat-square&logo=docsdotrs&label=docs.rs)](https://docs.rs/chamberlain-core)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square)](#license)
![status](https://img.shields.io/badge/status-0.x%20unstable-orange?style=flat-square)
[![platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey?style=flat-square)](#quick-start)

[![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-FFC131?style=flat-square&logo=tauri&logoColor=black)](https://tauri.app/)
[![TypeScript](https://img.shields.io/badge/TypeScript-3178C6?style=flat-square&logo=typescript&logoColor=white)](https://www.typescriptlang.org/)
[![React](https://img.shields.io/badge/React-61DAFB?style=flat-square&logo=react&logoColor=black)](https://react.dev/)

**English** | [日本語](README.ja.md)

Chamberlain is a Tauri-based framework for building autonomous desktop applications that behave like a personal secretary, with minimal code on the developer's side.

A Rust core runs in the background and executes each trigger on its schedule; developers write the schedule, decision logic, and notification behavior in JavaScript / TypeScript. Agent developers can start from a scaffold with `npm create chamberlain@latest`.

> [!WARNING]
> Chamberlain is in early development. Breaking changes may be introduced even in minor versions.

## Quick start

```bash
npm create chamberlain@latest my-secretary
```

See [`docs/getting-started.md`](docs/getting-started.md) for the full walkthrough.

## Documentation

- [`docs/overview.md`](docs/overview.md) — what this framework is for (vision, what/why)
- [`docs/architecture.md`](docs/architecture.md) — current skeleton (responsibilities, contracts, decisions)
- [`docs/getting-started.md`](docs/getting-started.md) — setup and first run (for agent developers)

> [!NOTE]
> Documentation is currently Japanese-only.

## Contributing

The framework development loop, DevContainer setup, and repo synchronization rules are in [`CONTRIBUTING.md`](CONTRIBUTING.md). Active design discussions and tasks are tracked in GitHub Issues (`gh issue list`).

## License

Dual-licensed under `MIT OR Apache-2.0`. You may pick whichever you prefer (following the Rust ecosystem convention).

- [LICENSE-MIT](LICENSE-MIT)
- [LICENSE-APACHE](LICENSE-APACHE)
