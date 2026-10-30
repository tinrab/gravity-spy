# Native builds need the libraries from `nix-shell` (ALSA, Vulkan, X11).

# Run the game. Arguments go to the game, e.g. `just run --level level-3 --autostart`.
run *args:
    cargo run --locked -- {{ args }}

# Preview an SVG level; F5 reloads it.
edit path="assets/authoring/template.svg":
    cargo run --locked -- --edit "{{ path }}"

build:
    cargo build --locked

release:
    cargo build --release --locked

format:
    just --fmt --unstable
    cargo fmt

lint:
    cargo clippy --all-targets --all-features --locked -- -D warnings
    cargo clippy --target wasm32-unknown-unknown --lib --locked -- -D warnings

test:
    cargo test --locked

# Release executable plus runtime assets in target/dist/gravity-spy.
package: release
    #!/usr/bin/env bash
    set -euo pipefail
    destination=target/dist/gravity-spy
    rm -rf "$destination"
    mkdir -p "$destination/assets"
    cp target/release/gravity-spy "$destination/"
    cp -R assets/audio assets/fonts assets/levels assets/shaders assets/authoring "$destination/assets/"
    cp README.md ASSETS.md "$destination/"
    echo "Packaged game: $destination/gravity-spy"

# Browser build in target/web. Needs wasm-pack and the wasm32-unknown-unknown target.
web:
    #!/usr/bin/env bash
    set -euo pipefail
    rm -rf target/web
    wasm-pack build --target web --out-dir target/web/pkg --out-name gravity_spy --release -- --locked
    cp web/index.html web/style.css web/game.js target/web/
    mkdir -p target/web/assets/fonts target/web/assets/shaders
    cp assets/fonts/PressStart2P-Regular.ttf assets/fonts/OFL.txt target/web/assets/fonts/
    cp assets/shaders/light.wgsl target/web/assets/shaders/
    cp -R assets/audio target/web/assets/
    touch target/web/.nojekyll
    echo "Built target/web. Serve it with: just serve"

# Serve the browser build at http://localhost:8000/.
serve port="8000":
    npx --yes http-server target/web -p {{ port }} -c-1

clean:
    cargo clean
