# Agent Seat independent-product governance

Status: approved GO decision, 2026-08-08. This is the G0 decision record for
the future `ZaguanLabs/agent-seat-proto` product. It authorizes E0 to create the
repository; it is not source for that product and creates no dependency from
Nobox to it.

## Decision and verified facts

The maintainer approved proceeding with the independent product under these
terms on 2026-08-08. The canonical product name is `agent-seat-proto`, and its
canonical upstream will be:

```text
https://github.com/ZaguanLabs/agent-seat-proto
```

The following GitHub state was verified immediately before this record was
approved:

- the `ZaguanLabs` organization exists;
- `kekePower` is an active organization owner and is the only listed member;
- `ZaguanLabs/agent-seat-proto` does not exist, so the canonical name is
  available;
- the organization does not currently require two-factor authentication; and
- no team or security-manager delegation is currently configured.

The last two facts are setup work for E0, not reasons to weaken the policy.
The repository must be created directly under `ZaguanLabs`, never under a
personal owner for later transfer.

## Goals

- Give corporations and individuals an unambiguous Apache-2.0 implementation
  they may use, modify, redistribute, and contribute to under one inbound
  policy.
- Keep the independent source provenance clean enough to audit without relying
  on subjective recollection later.
- Assign administration, security, contribution, review, release, and package
  ownership before accepting source.
- Preserve a strict product boundary: public protocol, generic companion, and
  standalone providers may share a repository, but authority never moves into
  the wire crate or companion.
- Keep Nobox and the independent product technically compatible while their
  source and licensing remain separate.

## Non-goals

- Moving, relicensing, extracting, translating, or mechanically rewriting
  Nobox source, history, tests, fixtures, schemas, or prose.
- Making Nobox depend on the future repository, crate releases, CI, issue
  tracker, or release schedule.
- Giving `agent-seat-proto` authority over grants, consent, input, launch, or
  window-management policy.
- Claiming Tier 1 assurances for a standalone Tier 0 X11 provider.
- Memorizing raw screen coordinates, recording user workflows, or collecting
  behavioral telemetry.
- Requiring copyright assignment or a contributor license agreement.

## License and inbound terms

- All original project source is licensed `Apache-2.0`; the repository carries
  the complete Apache License 2.0 text and uses `Apache-2.0` SPDX metadata.
- Contributions use Developer Certificate of Origin 1.1 sign-off. A pull
  request may not merge until every commit has a valid `Signed-off-by` line.
- There is no CLA and no copyright assignment. Contributors retain copyright
  while certifying that they have the right to submit their work under the
  project license.
- Dependencies and included assets need an explicit, Apache-2.0-compatible
  license and a recorded source. Vendored or generated material is refused by
  default and needs a documented exception.
- A corporate contributor follows the same DCO rule and is responsible for
  obtaining any employer authorization it needs.

## Clean-source provenance

P0's public behavior documents are requirements and prior-art references, not
source templates. Independent implementation may use published standards,
official library documentation, public process behavior, and fresh black-box
tests. It must not copy or adapt Nobox implementation text.

Every pull request must state one of these provenance classes:

1. original work written for `agent-seat-proto` from the public specification;
2. a dependency or asset with its upstream URL, exact license, and reason; or
3. a standards-derived fact with the exact public source named.

Review rejects unexplained source resemblance, copied comments or test
language, and patches derived from Nobox internals. Ideas may move between the
projects only by being restated as requirements and independently implemented.
No shared generated schema or reusable fixture is introduced later as a
shortcut around this rule.

## Responsibilities

The initial project has one accountable maintainer. A role may be delegated to
a ZaguanLabs team later, but it may never become ownerless.

| Responsibility | Initial owner | Required control |
| --- | --- | --- |
| Organization administration | `kekePower` | Keep the repository under `ZaguanLabs`; control membership, apps, and rulesets |
| Administrator recovery | `kekePower` | Maintain two independent GitHub account recovery methods and protected recovery material |
| Security intake and triage | `kekePower` | Enable GitHub private vulnerability reporting and own the response queue |
| Protocol and compatibility decisions | `kekePower` | Record incompatible changes and never infer compatibility from similar JSON |
| Contribution and provenance review | `kekePower` | Enforce DCO, license, dependency, and clean-source checks |
| Branch/ruleset administration | `kekePower` | Keep required review and status checks active after bootstrap |
| Release signing and GitHub releases | `kekePower` | Approve versions, signed tags, notes, and source releases |
| Registry/package publishing | `kekePower` | Use project-scoped credentials or trusted publishing; keep personal credentials out of CI |
| Incident recovery and credential rotation | `kekePower` | Revoke affected credentials, preserve advisories, and document recovery |

This assignment makes the current single-maintainer risk visible; it does not
claim that one person is redundancy. A second ZaguanLabs owner is desirable
before the project becomes operationally critical, but is not required for the
clean bootstrap.

## Repository controls approved for E0

E0 must configure these controls before accepting outside source:

- require organization two-factor authentication; the verified pre-E0 state
  is currently off;
- confirm the maintainer's recovery methods before making the repository
  public;
- enable private vulnerability reporting and add `SECURITY.md` with that
  canonical intake path;
- enable secret scanning and dependency alerts where GitHub makes them
  available;
- protect the default branch against deletion and force-pushes;
- require pull requests, DCO sign-off, the complete CI status check, and one
  approving review for non-maintainer changes;
- dismiss stale approvals when code changes and require conversation
  resolution;
- allow the organization owner a documented bootstrap and emergency bypass so
  a one-owner project cannot deadlock, while recording every bypass in release
  or incident notes;
- restrict GitHub release and package publication authority to `kekePower`
  until a named ZaguanLabs release team replaces that assignment; and
- give automation only the minimum per-workflow permissions, with no
  long-lived registry token committed or exposed to pull requests.

The initial skeletal commit may precede the default-branch ruleset because the
branch does not yet exist. The ruleset is applied immediately after that commit
and before any implementation pull request.

## Initial product boundary

The repository is one independent product with separable deliverables:

- `agent-seat-proto` owns bounded, display-server-neutral wire types and
  framing. It owns no policy, transport listener, desktop discovery, MCP, X11,
  or Nobox dependency.
- the generic MCP companion translates harness calls and has no authority. A
  provider revalidates every request.
- the Tier 0 X11 provider owns its socket, provider selection, grants, policy,
  EWMH realization, and failure boundary.

The Tier 0 core is observe, supported EWMH management, and controlled XDG
desktop-entry launch. Capture, input, and semantics are optional later profiles
with independent threat-model approval. Nobox retains `nobox-agent-wire` and
its integrated Tier 1 implementation on their current GPL-2.0-only development
path.

## Pre-E0 separation audit

The authorization rests on these completed Nobox milestones:

- v0.1.1 completed N0/N1 and the hardening baseline;
- P0 approved the readiness, threat-model, and Openbox acceptance package;
- v0.1.2 renamed the internal implementation to `nobox-agent-wire` without
  changing the wire; and
- v0.1.3 adopted selection-bound atomic provider ownership and generic
  discovery before the independent repository existed.

Nobox has no Cargo, CMake, test, package, or runtime dependency on
`ZaguanLabs/agent-seat-proto`. Its documentation may identify the canonical
future product and public behavioral relationship; those links are informative
and never part of a Nobox build or release gate.

## End result

G0 is GO. E0 may create `ZaguanLabs/agent-seat-proto` directly under the
organization and only under the controls above. The first independent source
must be authored in that repository after creation. If the owner, license,
inbound terms, or clean-source rule changes, implementation pauses until this
decision is superseded explicitly.
