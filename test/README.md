# Acceptance Tests

Pytest-based black-box acceptance tests for the `grim` CLI (the Grimoire
binary). The suite builds the release binary and runs it as an external
process, asserting only on observable behavior (stdout, stderr, exit codes).

## Prerequisites

- Rust toolchain (for building `grim`)
- Python 3.10+ with [uv](https://docs.astral.sh/uv/)

## Quick Start

```sh
cd test
task default
```

This builds `grim` (`cargo build --release --locked`), copies it to
`test/bin/grim`, points `GRIM_COMMAND` at it, and runs the suite.

Run directly with pytest (binary must already exist at `test/bin/grim` or
`GRIM_COMMAND` must be set):

```sh
cd test
uv run pytest -v
```

To run tests in parallel without rebuilding:

```sh
task quick
```

## Directory Structure

```
test/
  pyproject.toml             Python project / pytest config
  taskfile.yml               Build + test task runner
  conftest.py                Session/function fixtures (grim_binary, grim_home, grim)
  README.md
  src/                       Test support code
    runner.py                GrimRunner + platform helpers
    assertions.py            Reusable path/symlink assertions
  tests/                     Test modules
    conftest.py              Suite-local fixtures
    test_smoke.py            Universal CLI contract smoke tests
```

## Key Classes

### `GrimRunner` (`src/runner.py`)

Wraps the `grim` binary with a controlled environment. Each instance carries
its own `GRIM_HOME` plus a minimal `PATH`/`HOME` so tests never leak host
state into `grim`.

```python
grim.run("--version", check=False)        # raw CompletedProcess, allow non-zero
grim.json("info")                          # parse stdout as JSON
grim.plain("help")                         # raw CompletedProcess, no --format
```

## Fixtures

All fixtures are defined in the top-level `conftest.py`.

| Fixture       | Scope    | Description |
|---------------|----------|-------------|
| `grim_binary` | session  | Resolves the `grim` binary (via `GRIM_COMMAND` or `test/bin/grim`) |
| `grim_home`   | function | Isolated `GRIM_HOME` directory via `tmp_path` |
| `grim`        | function | `GrimRunner` wired to the isolated home |

### Test Isolation

Each test is self-contained: a unique `GRIM_HOME` per test (created via
`tmp_path`, destroyed afterwards) and a clean environment, so the suite is
safe to run in parallel (`-n auto`).

## Configuration

| Environment Variable | Default                | Description |
|----------------------|------------------------|-------------|
| `GRIM_COMMAND`       | `test/bin/grim`        | Path to the `grim` binary |
