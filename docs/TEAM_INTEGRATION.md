# Team integration and agent handoff

Owner-directed workflow for https://github.com/blockdev2/Mirage-Private.

## Pull requests only

Never update main directly. Use a focused topic branch and a pull request.
Do not auto-merge, force-push, rewrite another contributor's history, or delete
branches as part of general cleanup. The owner, blockdev2, will separately
authorize consolidation after comparing the team's submitted implementations.
An integration agent must evaluate evidence and behavior; an agent/model name,
large diff, recent timestamp, or claimed performance is not acceptance evidence.

## Discover work before changing it

Fetch without merging and enumerate remote branches, open/draft/closed PRs,
base/head SHAs, changed files, reviews and checks. Record pagination and query
failures. Inspect workflows and executable changes before running branch code.
A fetch does not reveal unpushed work. Team members may be working on overlapping
areas locally; absence of a remote branch is not evidence that an area is free.
Do not discard or overwrite a dirty checkout to make integration convenient.

At review time on 2026-11-12, before this research PR was created:

| Item | Observed state |
| --- | --- |
| main | `66cf4a9fa875a8d41e893e7382fb0bdd01e100dd` |
| dependabot/cargo/client/md-5-0.11.0 | `28af1ec165e427ce6049ae9a988e5422692782fc` |
| PR #1 | Open; md-5 0.10 to 0.11 in the core manifest and lockfile |
| Checks at that PR head | Dependency audit and history scan succeeded; Windows offline checks failed |

The two advertised remote branches and the PR list were inspected through Git
refs and the authenticated GitHub API. One detailed PR API request failed;
no cause is inferred for the Windows check failure. Refresh this inventory
before integration. This is a dated observation, not a permanent branch list.
No existing PR was merged, rejected, or modified by this review.

## What each PR should supply

- Scope, base/head revisions, affected public/wire interfaces, and overlaps.
- Actual test commands, environment/server versions, results, failures, and
  unverified behavior; include independent receiver evidence for delivery claims.
- Security and licensing impact, dependency changes, and third-party provenance.
- Migration/compatibility implications, rollback method, and documentation.
- A sanitized handoff identifying follow-up work and assumptions needing proof.

Keep local priorities, workstation configuration, raw captures and identities
out of team documentation. `AGENTS.local.md` is an optional ignored local
orientation file. It may specialize a particular checkout's assignment without
imposing that assignment on other contributors. Shared documentation should
explain contracts, verified behavior, research evidence and integration risks.

## Owner-authorized consolidation

1. Pin every candidate revision and compare overlapping implementations by
   requirements, correctness, security, maintainability and reproducible results.
2. Construct a semantic conflict map covering protocol models, identity/crypto,
   connection ownership, audio queues, bridge schema, UI mirrors, dependencies,
   workflows and documentation. Textually clean merging can still break these.
3. Select or combine the best supported parts in a new integration branch.
   Preserve attribution and document why competing approaches were superseded.
4. Run the project checks and relevant server/peer experiments on the combined
   result. Individual green PRs do not prove the combined result is correct.
5. Open an integration PR with unresolved risks and owner-review decisions.
   Merge into main only after the owner explicitly authorizes that integration.

GitHub branch protection was reported false on the two inspected branches.
PR-only handling is therefore a documented policy, not verified technical
protection. An administrator should separately consider required PR review,
required checks and restrictions on direct pushes; this review changes no
repository administrative settings.
