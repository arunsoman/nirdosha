# Maintainers

Roles are defined in [`GOVERNANCE.md`](./GOVERNANCE.md); area
assignments are in [`AREAS.md`](./AREAS.md). This file is the source of
truth for both GitHub access and the table below — a permission change
on GitHub without a matching edit here is a bug in the process, not
just a stale doc.

## Owner

| GitHub | Name | Since |
|---|---|---|
| [@arunsoman](https://github.com/arunsoman) | Arun Soman | project start |

Repo admin. 107 of the repository's commits are the Owner's as of
2026-09-04 (`git log --format=%an \| sort \| uniq -c`) — the project is
still, in practice, primarily one person's active work, even though
write access below is broader than that.

## Maintainers

| GitHub | Areas ([`AREAS.md`](./AREAS.md)) | Write access granted | Status |
|---|---|---|---|
| [@lekshmideepu](https://github.com/lekshmideepu) | *unassigned* | yes | not yet active — no commits/reviews on record yet |
| [@maheshmindlabs](https://github.com/maheshmindlabs) | *unassigned* | yes | active — Helm chart maintainership ([`deploy/helm/nirdosha/Chart.yaml`](./deploy/helm/nirdosha/Chart.yaml)), Track G ecosystem-gap docs/example, and the 2026-09-04 adoption-barrier fixes (CI/bench/compiler-diagnostics + docs/positioning); 4 commits on record (`git log --author=maheshmindlabs --oneline`) |
| [@arulrajan123](https://github.com/arulrajan123) | *unassigned* | yes | not yet active — no commits/reviews on record yet |
| [@Baskarrajcodeflow](https://github.com/Baskarrajcodeflow) | *unassigned* | yes | not yet active — no commits/reviews on record yet |

**Read this table honestly, not optimistically.** GitHub write access
exists for all four names above (confirmed via
`gh api repos/kannamma-labs/nirdosha/collaborators/<user>/permission`,
2026-09-04) — that part of `docs/ECOSYSTEM.md` §G5's ask is already
done. What's still open is *activation*: none of the four has an
assigned area, and three have no commits or reviews on this repo yet.
Access without an assigned area and a first review doesn't move the
bus factor — the fix is each of them picking up (or being assigned) a
row in `AREAS.md`, not another access grant. Until that happens, branch
protection's "1 approving review" requirement is real but its pool of
practical reviewers is closer to 1–2 people than 4.

## Triagers

None appointed yet. See [`GOVERNANCE.md`#contributor-funnel](./GOVERNANCE.md#contributor-funnel) —
a maintainer candidate not yet ready for write access is exactly who
this role is for.

## Becoming a maintainer

1. Land a few substantive, reviewed PRs in one area from
   [`AREAS.md`](./AREAS.md) — this is what "active" in the table above
   means in practice.
2. Ask in a PR or GitHub Discussion, or get nominated by an existing
   maintainer.
3. The Owner grants GitHub write access and adds a row here and in
   `AREAS.md` in the same PR — access and documentation land together,
   never one without the other.
