# SurrealDB — deployed as its own Dokploy Application.
#
# On Dokploy: create Application → Git Repository → Dockerfile Path
#   `deploy/surrealdb/Dockerfile`
# Set the container port to 8000. Mount a persistent volume at /data.
# Set env vars: SURREAL_USER=root, SURREAL_PASS=<long random string>.
#
# On local dev: `docker compose up` uses docker-compose.yml at the repo root,
# which already runs the same surrealdb/surrealdb:v3 image.
FROM surrealdb/surrealdb:v3

# Ensure root ownership so the RocksDB engine can write to /data regardless
# of the upstream image's default user.
USER root

EXPOSE 8000

# SurrealDB reads SURREAL_USER / SURREAL_PASS from env for initial root
# credentials on a fresh datastore. Dokploy injects these at runtime.
ENTRYPOINT ["/surreal"]
CMD ["start", "--bind=0.0.0.0:8000", "rocksdb:///data/tv.db"]
