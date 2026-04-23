# RustFS — deployed as its own Dokploy Application.
#
# On Dokploy: create Application → Git Repository → Dockerfile Path
#   `deploy/rustfs/Dockerfile`
# Set the container port to 9000. Mount a persistent volume at /data.
# Set env vars:
#   RUSTFS_ACCESS_KEY=<short id>
#   RUSTFS_SECRET_KEY=<long hex string>
FROM rustfs/rustfs:latest

USER root

# S3 API + admin console.
EXPOSE 9000 9001

# Bake in the bind addresses so we don't depend on Dokploy's UI exposing a
# command-override field. Credentials still come from env vars at runtime.
ENV RUSTFS_ADDRESS=0.0.0.0:9000 \
    RUSTFS_CONSOLE_ENABLE=true \
    RUSTFS_CONSOLE_ADDRESS=0.0.0.0:9001
