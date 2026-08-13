# 001: Single-User, Single-Trust-Domain Product Model

Role: Architecture decision record

Status: Accepted

Date: 2026-07-11

Authority: Binding product and identity boundary for this plan

Scope: Fixed constraints 1-4 and the trust-domain/identity interpretation of constraint 5; authentication interpretation, state partitioning, Files, usage, scheduling, and HA language

Decision source: Operator instruction on 2026-07-11: `不存在多用户`.

Related: [Business context](../../../baseline/business-context.md), [Requirements](../topics/requirements-and-quality-attributes.md), [Problem catalog](../topics/problems/README.md)

## Context

Earlier analysis inferred a possible multi-user or tenant boundary from multiple request API keys, multiple credentials, and Files-compatible objects. The operator explicitly confirmed that the product has no multi-user model.

The service can still have multiple clients, API keys, Kiro credentials, external pools, and process replicas. Those are access, capacity, provider, and availability concepts within one operator-owned trust domain.

## Decision

`kiro-rs` is a single-user, single-operator, single-trust-domain product.

- Request API keys authenticate access to the same service and do not identify a user or tenant.
- Kiro credentials and external pools are operator-owned capacity resources.
- Usage, Files, caches, credentials, and configuration are not partitioned by request key or tenant.
- The architecture will not introduce `UserId`, `TenantId`, tenant repositories, tenant quotas, tenant billing, or tenant routing.
- Replica count is independent from user count. Decisions 010/014 fix multi-replica production as a supported mode inside the same trust domain, with shared operator state, one attested release generation and explicit convergence/recovery contracts; it is not conditional on a future product decision.
- External upstreams, remote URLs, PgSQL, Redis, secrets, logs, and filesystem paths remain real security and failure boundaries.

## Alternatives Considered

### Infer users from request API keys

Rejected. Current authentication produces no principal, role, ownership, or quota context, and the product requirement explicitly denies that meaning.

### Add a tenant abstraction for possible future use

Rejected. It would complicate every state key, query, cache, scheduler, and API without a current business requirement. A future multi-user product requires a new superseding decision and migration plan.

## Consequences

- Cross-tenant file and cache findings are retracted.
- Files risks are evaluated as capacity, copy cost, restart, and replica-availability issues.
- API key rotation and audit remain required, but per-key data ownership is not.
- Multi-replica Admin key/catalog convergence and release-generation membership are required production correctness issues under decisions 010/014, not a tenant boundary or optional future mode.
- Security work focuses on secret handling, SSRF, external header boundaries, diagnostics, resource limits, and control-plane exposure.

## Verification

- New domain and persistence types contain no tenant/user identity unless a superseding decision exists.
- Documentation and code review reject language that treats API keys or credentials as tenant identities.
- Any multi-replica tests use one shared operator state and do not invent tenant fixtures.

## Supersession

Only an explicit future product decision to support multiple users can supersede this record. That decision must define identity, ownership, authorization, migration, compatibility, billing/quota semantics, and data partitioning.
