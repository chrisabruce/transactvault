.PHONY: dev build run fmt check clippy test services services-down db-surql backup restore clean

# SurrealDB connection for CLI tooling. Override on the command line,
# e.g. `make backup SURREAL_ENDPOINT=http://localhost:37422`.
SURREAL_ENDPOINT ?= http://localhost:8000
SURREAL_USER ?= root
SURREAL_PASS ?= root
SURREAL_NS ?= transactvault
SURREAL_DB ?= app
BACKUP_FILE ?= backup-$(shell date +%Y%m%d-%H%M%S).surql

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
	surreal sql --endpoint $(SURREAL_ENDPOINT) --user $(SURREAL_USER) --pass $(SURREAL_PASS) \
		--ns $(SURREAL_NS) --db $(SURREAL_DB)

# Full-database backup. `surreal export` dumps every table's definition
# AND rows — brokerages, users, transactions, tiers, the whole forms
# engine (form_set / form_group / form + edges), audit log, everything —
# so new tables are captured automatically with no list to maintain.
# NOTE: this is the SurrealDB layer only. Uploaded documents live in
# object storage (RustFS/S3); snapshot the bucket separately for a
# complete disaster-recovery backup.
backup:
	surreal export --endpoint $(SURREAL_ENDPOINT) --user $(SURREAL_USER) --pass $(SURREAL_PASS) \
		--ns $(SURREAL_NS) --db $(SURREAL_DB) $(BACKUP_FILE)
	@echo "Wrote full database backup to $(BACKUP_FILE)"

# Restore from a backup file: `make restore FILE=backup-YYYYMMDD-HHMMSS.surql`.
restore:
	@test -n "$(FILE)" || (echo "Usage: make restore FILE=backup-XXXX.surql"; exit 1)
	surreal import --endpoint $(SURREAL_ENDPOINT) --user $(SURREAL_USER) --pass $(SURREAL_PASS) \
		--ns $(SURREAL_NS) --db $(SURREAL_DB) $(FILE)
	@echo "Restored database from $(FILE)"

clean:
	cargo clean
	rm -rf uploads/* surreal-data/
