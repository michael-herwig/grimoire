# Authentication

Most public skills and rules pull anonymously, but a private registry wants
to know who you are before it hands anything over. Grimoire does not invent
its own login system for that — it reads and writes the same credential
store [Docker][docker-login] and [oras][oras-login] already use, so a single
`docker login` (or `grim login`) covers every tool on the machine.

This page covers how Grimoire finds credentials when it talks to a registry,
how [`grim login`](#login) and [`grim logout`](#logout) manage them, and
where they are stored on disk.

## How credentials are resolved {#resolving}

Every registry request starts the same way: Grimoire looks up a credential
for the target registry and falls back to anonymous access when it finds
none. A missing credential is never an error — public artifacts keep
working with no setup.

The lookup reads `~/.docker/config.json` (or `$DOCKER_CONFIG/config.json`),
the [Docker config file][docker-login]. If a [credential
helper][docker-cred-helpers] is configured for the registry, Grimoire asks
the helper; otherwise it reads the base64 entry under `auths`. The registry
key is normalized first — the scheme and any `/v2/` API suffix are stripped,
and `docker.io` is mapped to its canonical `https://index.docker.io/v1/`
form — so a credential stored by `docker login` resolves the same way under
`grim`.

Credentials can also come from the environment for `GRIM_INSECURE_REGISTRIES`
and other CI setups; see [Configuration](./configuration.md#environment-variables).

## grim login {#login}

`grim login [registry]` authenticates to a registry and stores the
credential so later pulls and pushes reuse it. With no positional argument
it resolves the registry the same way every command does — `--registry`,
then the `default_registry` option, then `GRIM_DEFAULT_REGISTRY`.

The username comes from `--username`/`-u`, or an interactive prompt when
omitted on a terminal. The password is read from a hidden terminal prompt,
or from standard input with `--password-stdin`. There is intentionally **no**
`--password <value>` flag: a secret on the command line leaks through the
process list and shell history.

```sh
# Interactive: prompts for the password without echoing it.
grim login ghcr.io -u alice

# Non-interactive (CI): read the token from stdin.
echo "$GITHUB_TOKEN" | grim login ghcr.io -u alice --password-stdin
```

Where the credential lands depends on what is configured — see [Where
credentials are stored](#store). When no credential helper is configured,
Grimoire **refuses** to write a plaintext credential unless you opt in with
`--allow-insecure-store`, which stores a base64 entry (not encryption) in
`config.json`. The file is created with owner-only (`0600`) permissions.

Grimoire stores the credential without first contacting the registry, which
matches `docker login` with a credential helper. A wrong password therefore
surfaces on the next pull or push, not at login time.

## grim logout {#logout}

`grim logout [registry]` removes a stored credential. It resolves the
registry exactly like [`grim login`](#login).

Logout is idempotent: removing a credential that was never stored exits `0`,
matching [`docker logout`][docker-login] and [`oras logout`][oras-login] so a
CI cleanup step never fails on a fresh runner.

```sh
grim logout ghcr.io
```

## Where credentials are stored {#store}

Grimoire writes to the Docker-compatible config at
`$DOCKER_CONFIG/config.json`, defaulting to `~/.docker/config.json`. Set
`DOCKER_CONFIG` to point both Grimoire and Docker at an isolated directory —
useful for tests and per-job CI credentials.

The destination follows the same precedence [Docker][docker-login] uses,
highest first:

| Tier | Config key | Storage |
|------|-----------|---------|
| Per-registry helper | `credHelpers[registry]` | The named OS keychain helper. |
| Default helper | `credsStore` | The named OS keychain helper. |
| Plaintext fallback | `auths[registry]` | base64-encoded, gated by `--allow-insecure-store`. |

A credential helper is a small program named `docker-credential-<name>` on
your `PATH` that stores secrets in the OS keychain — for example
`osxkeychain`, `wincred`, or `secretservice`/`pass` on Linux. The
[docker-credential-helpers][docker-cred-helpers] project ships the common
ones. When a helper is configured, the secret never touches `config.json`;
only the helper name does.

Unlike `docker login`, Grimoire does **not** auto-detect and silently enable
a platform helper on first use. It writes only what is already configured,
or the explicit plaintext fallback — so a shared machine never gains a
sticky `credsStore` entry behind your back.

## Credentials in CI {#ci}

A headless runner usually has no terminal and no keychain. Pipe the token in
and opt into the plaintext store scoped to a per-job `DOCKER_CONFIG`:

```sh
export DOCKER_CONFIG="$RUNNER_TEMP/docker"
echo "$REGISTRY_TOKEN" | grim login "$REGISTRY" -u "$REGISTRY_USER" \
  --password-stdin --allow-insecure-store
grim release ./code-review "$REGISTRY/acme/code-review:1.2.3"
grim logout "$REGISTRY"
```

Because Grimoire shares the Docker config, a prior [`docker login`][docker-login]
step in the same job is enough on its own — `grim` reuses whatever Docker
stored.

<!-- external -->
[docker-login]: https://docs.docker.com/reference/cli/docker/login/
[docker-cred-helpers]: https://github.com/docker/docker-credential-helpers
[oras-login]: https://oras.land/docs/commands/oras_login
