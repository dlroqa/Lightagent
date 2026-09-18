# Packaging Lightagent

Lightagent ships as one archive per Rust target triple. Each archive contains:

- the `lightagent` CLI;
- the compiled standalone Web UI under `web/`;
- `LICENSE` and a short archive README;
- Linux user-service examples when available.

The inference engine is deliberately absent. Lightagent talks to a configured
OpenAI-compatible provider over HTTP.

## Build an archive

```sh
npm ci --prefix frontend
npm run build --prefix frontend
cargo build --release -p lightagent
bash scripts/package-lightagent.sh
```

Pass a target triple when using a matching cross/native target build:

```sh
cargo build --release -p lightagent --target x86_64-unknown-linux-gnu
bash scripts/package-lightagent.sh x86_64-unknown-linux-gnu
```

The archive is written to `dist-cli/` as `.tar.gz` on Unix targets and `.zip`
on Windows. Verify it by building, unpacking, and running its binary:

```sh
bash scripts/smoke-lightagent-package.sh
```

## Use the archive

```sh
tar -xzf lightagent-<version>-<triple>.tar.gz
cd lightagent-<version>-<triple>
./lightagent
./lightagent serve --web-root ./web
```

Lightagent defaults to `http://127.0.0.1:11434`. Select another provider with
`lightagent setup provider` or `lightagent config set inference.base_url URL`.
Use `lightagent doctor` to check connectivity.

## Linux user service

`packaging/systemd/lightagent.service` is a user service. Copy the unit and its
environment example into the user configuration, initialize a profile, and
start it:

```sh
mkdir -p ~/.config/lightagent ~/.config/systemd/user
cp packaging/systemd/lightagent.env.example ~/.config/lightagent/env
chmod 600 ~/.config/lightagent/env
cp packaging/systemd/lightagent.service ~/.config/systemd/user/
lightagent init
systemctl --user daemon-reload
systemctl --user enable --now lightagent
```

The service defaults to loopback. Non-loopback binds require `--key-env` and an
environment variable holding the API key. Add `--web-root` to
`LIGHTAGENT_SERVE_ARGS` when the compiled Web UI is installed at a stable path.
