# Deploying nirdosha to Kubernetes

Three ways in, all rendering the same shape (see `../docs/KUBERNETES.md` for
the compliance matrix these implement and `../docs/KUBERNETES_ADVANTAGE.md`
for the case for nirdosha over a classic two-tier stack on k8s):

1. **Helm chart** (`helm/nirdosha/`) — for a human operator, templated,
   `values.yaml`-configurable. `helm install my-app deploy/helm/nirdosha
   -f my-values.yaml`.
2. **Kustomize** (`kustomize/base/`, `kustomize/overlays/
   postgres-multi-replica/`) — plain, pre-rendered manifests for an
   operator who'd rather not template at all. `kubectl apply -k
   deploy/kustomize/base` (single-replica, local SQLite) or `kubectl
   apply -k deploy/kustomize/overlays/postgres-multi-replica` (N
   replicas, Postgres-backed durability — create the `nirdosha-postgres`
   Secret first, see that overlay's own comment).
3. **protobox's automated path** — a project generated through
   protobox's nirdosha lane deploys via `plugins/deploy_targets/
   kubernetes.py` (`be-v2/`), which renders the identical shape
   programmatically (`render_manifests`) and additionally builds+pushes
   a per-project derived image (`FROM` the base runtime image below,
   `COPY`s in just that project's `.nir` source) before applying.

All three need the base runtime image published first — see the repo
root `Dockerfile` and `docs/KUBERNETES.md`'s P0 remediation item
("Publish `ghcr.io/protobox/nirdosha-runtime`"). Build it locally for
testing with:

```sh
docker build -t ghcr.io/protobox/nirdosha-runtime:latest .
```

## Choosing single-replica vs. multi-replica

`.nir` has no runtime env-var read — a program's `db_connect(...)`
literal, and which durability-log backend it uses, are both decided at
*generation* time, not deploy time (`docs/KUBERNETES.md`'s "State, data &
horizontal scaling" section). That's why replica count isn't just a
number in these manifests:

- **1 replica** (the default in both the Helm chart and the Kustomize
  base): a `StatefulSet` + one `PersistentVolumeClaim`, local SQLite.
  Works with nothing else — no external database, no extra config.
- **>1 replicas**: requires `--transact-log`/`--workflow-log` already
  pointed at a shared Postgres database (a real runtime capability,
  shipped) — both the Helm chart and `kubernetes.py::render_manifests`
  refuse to render a >1-replica manifest set without this, rather than
  silently producing replicas that would each hold their own divergent
  SQLite durability log. The `--db`-backed generic table browser
  (`/_nirdosha/table/<name>`) has no Postgres option yet — either don't
  use `--db` in a multi-replica deployment, or wait for that gap to
  close upstream (tracked in `docs/KUBERNETES.md`).
