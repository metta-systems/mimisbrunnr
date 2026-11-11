# Access control — design

Scope: what a caller of Mímisbrunnr may discover and touch — query visibility, tag visibility, rights on objects, and how they reach the query executor, the index, subscriptions, the Unix views and cluster sync. Identity and authentication are Mettā's.
Governing spec: SPECIFICATION-DESIGN§2.3 (query algebra), §5 (index), §11 (subscriptions), §12 (Unix interop), §9 (encryption).

Plan: [access-implementation-plan.md](access-implementation-plan.md) · Open decisions: [access-decisions.md](access-decisions.md) · Completed: [access-completed.md](access-completed.md) · Order: [operations-plan.md](operations-plan.md)

## Goal

Mettā is a capability system: a domain reaches only what it holds a capability for. A query-based filesystem works the other way round — a query discovers objects by their properties, so any caller able to query could find any object. With no directories to act as namespace and access boundary at once, **the query engine is the access boundary**, and tags are both how objects are found and something that needs protecting.

Three questions follow, for every operation the engine offers:

1. **Object access** — may this caller read, write, retag or delete object O?
2. **Query visibility** — which objects may appear in this caller's results, counts and events?
3. **Tag visibility** — may this caller learn that a tag, or a value of it, exists at all?

## Background

Four earlier systems shape the problem:

- **EROS / KeyKOS** have no filesystem at all: every object is reached through a capability, and persistence is orthogonal (a checkpoint captures all state, capabilities included). There the capability *is* the access control. Mímisbrunnr cannot stop there, because it must discover objects: no one can be handed a capability to "all photos from 2024" before those photos exist.
- **Tahoe-LAFS** gives each stored object cryptographic capabilities derived one way — write → read → verify — so a holder weakens a capability without asking any authority. Diminishment as a first-class, local operation carries over.
- **Attribute-based access control** (NIST SP 800-162) decides from attributes of subject, object, action and environment; in a tag-based filesystem tags already are the object's attributes. But ABAC is a policy framework without unforgeable tokens: it has to rest on capabilities.
- **Capsicum** puts a process into capability mode, where it loses the global namespaces and keeps only the descriptors it holds. Its lesson: a global namespace is the enemy of capability discipline — and a query interface is a global namespace, so it must be scoped.

## Selected design

- **Mettā provides identity, authentication and the capabilities that reach the filesystem.** A domain without a capability to the filesystem's endpoint issues no queries at all; Mímisbrunnr does not authenticate anyone.
- **Mímisbrunnr enforces visibility at its own boundary.** The OS capability system cannot filter which postings, facet counts, dictionary entries or subscription events a holder sees, since it never sees them; the filesystem must, on every surface that reveals objects or terms: query results and `Not`, faceted counts, dictionary and n-gram scans, term statistics, record reads, subscription events, the `/tags/` and `/paths/` views, and sync.
- How visibility is scoped, represented and granted is open: [access-decisions.md](access-decisions.md).

## Rejected alternatives

- **Access control wholly outside the filesystem** (declaring it the Mettā security model's concern alone). The security model can gate who reaches the filesystem, not what a query reveals once reached.
- **Per-object ACLs of users and groups.** Unix-style lists check access per object after discovery; they do not stop a query, a count or an enumeration from revealing what exists.

## Format specification

No on-disk structures yet; their shape follows the decisions in [access-decisions.md](access-decisions.md).
