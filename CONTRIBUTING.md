# Contributing to SODP

Thanks for your interest in contributing! This guide covers everything you need
to set up a local development environment and submit changes.

## Prerequisites

| Tool | Version | Purpose |
|------|---------|---------|
| **Rust** | 1.88+ | Server (`sodp-server`) |
| **Node.js** | 18+ | TypeScript client + React hooks |
| **Python** | 3.11+ | Python client |
| **Java** | 17+ | Java client |
| **Maven** | 3.9+ | Java build |
| **Redis** | 7+ | Only for cluster tests (optional) |
| **Docker** | 24+ | Only for container builds (optional) |

## Repository layout

```
src/           Rust server source
client-ts/     @sodp/client  (TypeScript)
react-sodp/    @sodp/react   (React hooks)
sodp-py/       sodp           (Python)
sodp-java/     io.sodp:sodp-client (Java)
demo-collab/   Collaborative editor demo
docs/          Protocol spec, guides, diagrams
proto/         Protobuf definitions (benchmarks only)
config/        Example ACL + schema files
```

## Quick setup

```bash
# Clone
git clone https://github.com/orkestri/SODP.git
cd SODP

# Rust server
cargo build
cargo test

# TypeScript client
cd client-ts && npm install && npm test && cd ..

# React hooks
cd react-sodp && npm install && npm test && cd ..

# Python client
cd sodp-py && python -m venv .venv && source .venv/bin/activate
pip install -e ".[dev]" && pytest && cd ..

# Java client
cd sodp-java && mvn test && cd ..
```

Or use the Makefile from the project root:

```bash
make build      # build server + all clients
make test       # run all unit tests
make fmt        # format all code
make lint       # lint all code
make clean      # remove build artifacts
```

## Running the server locally

```bash
# Ephemeral (in-memory only)
RUST_LOG=info cargo run --bin sodp-server -- 0.0.0.0:7777

# With persistence
RUST_LOG=info cargo run --bin sodp-server -- 0.0.0.0:7777 /tmp/sodp-log

# With schema validation
RUST_LOG=info cargo run --bin sodp-server -- 0.0.0.0:7777 /tmp/sodp-log config/schema.json.example
```

## Running integration tests

Integration tests need a running server:

```bash
# Terminal 1 — start the server
RUST_LOG=info cargo run --bin sodp-server -- 0.0.0.0:7777

# Terminal 2 — run TS integration
cd client-ts && node integration.mjs

# Terminal 3 — run Python integration
cd sodp-py && python integration.py
```

Or with `make`:

```bash
make integration   # starts server, runs all integration suites, stops server
```

## Code style

### Rust
- `cargo fmt` before committing
- `cargo clippy` must pass with no warnings
- Tests go in `#[cfg(test)] mod tests` at the bottom of each file
- Use `tracing` macros, never `println!`

### TypeScript
- Compile with `tsc --noEmit` (strict mode)
- Tests use Jest

### Python
- Format with `black` or `ruff format`
- Type hints on all public functions
- Tests use `pytest`

### Java
- Java 17 language level (no Java 21+ features like pattern matching in switch)
- Tests use JUnit 5

## Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: Add multi-key WATCH support
fix: Prevent double STATE_INIT on rapid reconnect
docs: Update clustering guide with Redis Sentinel setup
chore: Bump TypeScript client to 0.2.0
test: Add unit tests for ACL claim resolution
```

## Pull requests

1. Create a branch from `master`
2. Make your changes with tests
3. Run `make test` and `make lint`
4. Push and open a PR
5. Describe what changed and why in the PR body

Keep PRs focused. One feature or fix per PR is easier to review.

## Release process

Releases are triggered by git tags. Each package has its own tag prefix:

| Package | Tag format | Pipeline |
|---------|-----------|----------|
| Server (Docker) | `server/v0.1.1` | Build + push to GHCR |
| TypeScript | `ts/v0.1.1` | Publish to npm |
| Python | `py/v0.1.1` | Publish to PyPI |
| Java | `java/v0.1.1` | Publish to GitHub Packages |
| Demo | `demo/v0.1.1` | Build + push to GHCR |

To release:

```bash
git tag server/v0.2.0
git push origin server/v0.2.0
```

## Questions?

Open an issue at [github.com/orkestri/SODP/issues](https://github.com/orkestri/SODP/issues).
