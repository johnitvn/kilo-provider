# kilo-provider

Model provider gateway on top of the Kilo AI API gateway: proxy pool, model catalog, circuit breaker.

This is the **bootstrap scaffold**: configuration plumbing, shared state and
wellness routes only. Inference/business logic arrives in follow-up feature
PRs.

## Design contract

* Upstream: `https://api.kilo.ai/api/gateway`, accessed **anonymously** — this
  crate never sets an `Authorization` header (upstream authentication, if ever
  needed, happens via the proxy pool, not the gateway code).
* **No timeouts anywhere.** Long-lived streaming responses are expected; no
  overall request, read or connect timeout is configured (see
  `src/upstream.rs`).
* `PORT` env var is **required**. Missing/unparseable values abort startup
  with a clear error log — no default port, no silent fallback.
* `CONFIG_PATH` env var selects the configuration file (default
  `config.yaml`, relative to the working directory). It is the single
  configuration file and must include a `proxy_pool.proxies` section.
  `config.example.yaml` is the credential-free sample; the real `config.yaml`
  is gitignored.

## Configuration

See `config.example.yaml`:

| section           | purpose                                                        |
| ----------------- | -------------------------------------------------------------- |
| `upstream`        | optional `base_url` override (defaults to the Kilo gateway)     |
| `proxy_pool.proxies` | required proxy list (`name`/`url`), validated for duplicates |
| `model_catalog`   | placeholder settings (refresh/cache TTL) for a feature PR      |
| `circuit_breaker` | failure/success thresholds + open timeout (not yet wired)      |
| `log`             | `format` (`json`\|`text`) and `level`; `RUST_LOG` may override |

## Runtime

```bash
PORT=8123 CONFIG_PATH=config.yaml ./kilo-provider
```

Endpoints:

* `GET /healthz` — liveness, always `200`.
* `GET /readyz`  — readiness; JSON with upstream URL, proxy pool contents,
  catalog size and circuit-breaker state.

Graceful shutdown on `SIGINT`/`SIGTERM`.

## Development

```bash
cargo fmt --check          # formatting
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test  --workspace --all-features
cargo build --locked --release
```

## Docker

```bash
docker build -t kilo-provider:dev .
docker run --rm -p 127.0.0.1:8123:8123 \
  -e PORT=8123 \
  -e CONFIG_PATH=/etc/kilo-provider/config.yaml \
  -v "$PWD/config.example.yaml:/etc/kilo-provider/config.yaml:ro" \
  kilo-provider:dev
```

The image is multi-stage (`builder` → `debian:bookworm-slim` runtime), runs as
non-root uid 10000 (`kilo`), installs only `ca-certificates`, contains no
configuration or secrets, and builds cleanly for both `linux/amd64` and
`linux/arm64` (rustls TLS in pure Rust, no arch-specific steps).

## Releases & images

* Conventional Commits drive automerge-less Release PRs (`chore(main): release
  x.y.z`) via Release Please (manifest mode). Merging a Release PR tags
  `vX.Y.Z` and publishes `ghcr.io/<owner>/kilo-provider` images for
  `X.Y.Z`, `X.Y` (when applicable), the commit SHA, and `latest` (stable only)
  — always built from the tagged commit, never from branch HEAD.
* Only repository setting required: *Settings → Actions → General → Allow
  GitHub Actions to create and approve pull requests* (GITHUB_TOKEN cannot
  open the Release PR otherwise). Note that PRs authored by `GITHUB_TOKEN`
  do not trigger `pull_request` CI workflows.
* No secrets are required beyond the built-in `GITHUB_TOKEN`.
