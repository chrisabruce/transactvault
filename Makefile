.PHONY: dev build run fmt check clippy test services services-down db-surql clean

dev:
	RUST_LOG=info,transactvault=debug cargo run

build:
	cargo build --release

run:
	./target/release/transactvault

fmt:
	cargo fmt --all

check:
	cargo check --all-targets

clippy:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test

services:
	docker compose up -d

services-down:
	docker compose down

db-surql:
	surreal sql --endpoint http://localhost:8000 --user root --pass root \
		--ns transactvault --db app

clean:
	cargo clean
	rm -rf uploads/* surreal-data/
