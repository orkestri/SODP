.PHONY: build test fmt lint clean integration server

# ── Build ────────────────────────────────────────────────────────────────────

build: build-server build-ts build-react build-py build-java

build-server:
	cargo build

build-ts:
	cd client-ts && npm install && npm run build

build-react:
	cd react-sodp && npm install && npm run build

build-py:
	cd sodp-py && pip install -e . --quiet

build-java:
	cd sodp-java && mvn compile -q

# ── Test ─────────────────────────────────────────────────────────────────────

test: test-server test-ts test-react test-py test-java

test-server:
	cargo test

test-ts:
	cd client-ts && npm test

test-react:
	cd react-sodp && npm test

test-py:
	cd sodp-py && python -m pytest tests/ -q

test-java:
	cd sodp-java && mvn test -q

# ── Format ───────────────────────────────────────────────────────────────────

fmt:
	cargo fmt
	@echo "Rust formatted"

# ── Lint ─────────────────────────────────────────────────────────────────────

lint: lint-server lint-ts lint-react

lint-server:
	cargo clippy -- -D warnings

lint-ts:
	cd client-ts && npx tsc --noEmit

lint-react:
	cd react-sodp && npx tsc --noEmit

# ── Integration ──────────────────────────────────────────────────────────────

integration:
	@echo "Building server..."
	cargo build --quiet
	@echo "Starting server on :7777..."
	RUST_LOG=warn ./target/debug/sodp-server 0.0.0.0:7777 & \
	SERVER_PID=$$!; \
	sleep 1; \
	echo "Running TS integration tests..."; \
	cd client-ts && node integration.mjs; TS_EXIT=$$?; cd ..; \
	echo "Running Python integration tests..."; \
	cd sodp-py && python integration.py; PY_EXIT=$$?; cd ..; \
	kill $$SERVER_PID 2>/dev/null; \
	exit $$(( TS_EXIT + PY_EXIT ))

# ── Server (dev) ─────────────────────────────────────────────────────────────

server:
	RUST_LOG=info cargo run --bin sodp-server -- 0.0.0.0:7777

server-persistent:
	RUST_LOG=info cargo run --bin sodp-server -- 0.0.0.0:7777 /tmp/sodp-log

# ── Docker ───────────────────────────────────────────────────────────────────

docker:
	docker compose build

docker-up:
	docker compose up -d

docker-down:
	docker compose down

# ── Clean ────────────────────────────────────────────────────────────────────

clean:
	cargo clean
	rm -rf client-ts/dist client-ts/node_modules
	rm -rf react-sodp/dist react-sodp/node_modules
	rm -rf sodp-py/dist sodp-py/src/*.egg-info
	rm -rf sodp-java/target
