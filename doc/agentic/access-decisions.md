# Access control — open decisions

Design: [access-design.md](access-design.md) · Plan: [access-implementation-plan.md](access-implementation-plan.md)

## ACCESS-D1 — The visibility scope: realms

What scopes visibility. The candidate from earlier design work is the **realm**: a flat, named scope; every object belongs to one or more realms, every session to a set of them. Names such as `team/engineering` are flat labels — the `/` is cosmetic, not a path.

- **Representation:** realms as system labels (ONTOLOGY-DESIGN "Tags and attribute keys"), so a realm's members are a posting list like any term, and a session's visibility is one precomputed mask — the union of its realms' postings — intersected with every result, window by window. Alternatives: a realm id in the object record (one realm per object, no posting); or no realms, only per-object capabilities handed out by applications (no discovery beyond what one already holds).
- **`Not`:** today `Not(q) = _object − q` (SPECIFICATION-DESIGN§2.3); under realms it must become `visible − q`, or it reveals what lies outside the caller's realms.
- **Creation:** a new object joins the creating session's realms, or a subset it names; it is always in at least one.
- **Changing an object's realms** — sharing into another realm, removing it from one — needs a right of its own (ACCESS-D4) and membership in the realm concerned; removing the last realm is refused.
- **Containment:** whether a realm may contain another (an organisation-wide realm seeing every team's objects). Expressible with `Implies` rules on realm labels, which would let whoever installs rules widen visibility — see ACCESS-D5.

## ACCESS-D2 — Existence hiding

Whether an object outside the caller's visibility is indistinguishable from one that does not exist: every lookup by id — record reads, opens, `/paths/` and FUSE `lookup`, relation targets, subscription events — answers `NotFound`, never `AccessDenied`. Candidate: yes, on every surface; decide also what a write naming an invisible object as a relation target does.

## ACCESS-D3 — Tag visibility

Whether tags and values can be hidden, per realm. The candidate levels:

- **Open** — names and values visible to all.
- **NameOnly** — the key is visible; its values only through objects the caller can see.
- **Hidden** — outside its realm the tag does not exist: a query naming it matches nothing (no error), and no enumeration lists it.

Every enumeration surface must apply it: dictionary and prefix scans, n-gram `Contains` lookups over distinct values (INDEX-DESIGN§4.2), faceted refinement counts (SPECIFICATION-DESIGN§5.6), term cardinalities and other statistics, the `/tags/` view, and SQL `GROUP BY` over keys. Candidate policy shape: each realm declares a default visibility for its tags, plus overrides per key — `diagnosis` and `patient` hidden in realm `medical` while the realm's other keys stay open. A query naming a key resolves it against the policies of the realms that govern it: a hidden key is usable only by sessions holding one of those realms, and resolves to an empty set for everyone else.

Decide the levels, where a policy is declared (per realm, per key, in the ontology), and whether hidden values must also be encrypted at rest — query hiding does not protect a stolen disk (ACCESS-D7).

## ACCESS-D4 — Sessions and rights

What a session is and what it may do. The candidate:

- A session is obtained through a Mettā capability to the filesystem and carries a realm set and a maximum set of rights; the session's capability encodes them.
- **Rights:** read data, write data, delete, read terms, write terms, share (derive capabilities for others), administer (change an object's realms). Changing realm membership is separate from writing terms, or anyone able to tag could reassign visibility.
- **Diminishment:** a holder can derive a weaker session or object capability (fewer realms or rights) without asking anyone.
- **Opening an object** checks visibility, then rights, and returns an object capability for the data path; data access bypasses the query engine after that.

**Candidate mapping onto Mettā's capability types:**

- **Reaching the filesystem:** a domain obtains an `EndpointKey` to the filesystem from KeyMaster. Without it, no queries.
- **Opening a session:** the domain asks for a session with a realm set and rights. The filesystem checks that the domain holds a **RealmKey** for each realm and that the rights are within each realm's maximum for it, and returns a **SessionKey**: a badged `EndpointKey` whose badge encodes the realm set and rights. Queries carry the session in the badge.
- **RealmKey:** a badged `EndpointKey` to the filesystem's realm manager, its badge encoding realm id and rights. A weaker RealmKey is derived by re-badging with fewer rights — admin → read-write → read-only — never the reverse.
- **Opening an object:** the filesystem checks visibility (ACCESS-D2 on failure), then rights against the session's maximum, and mints a **`BufferKey`** with the granted rights; data access is then zero-copy through it, bypassing the query engine.
- **Subscriptions:** signalled through an **`EventCountKey`** the filesystem advances when events are pending.
- **The filesystem's own authority:** it runs as an ordinary domain holding only what it needs — its listening endpoint, buffers for its index and ontology data and its storage, the RealmKeys of the realms it manages, endpoints to the storage driver and to KeyMaster, and its notification keys — and nothing outside the realms it manages.
- **Revocation chain:** KeyMaster revokes a RealmKey → the filesystem invalidates every session that includes the realm → the outstanding `BufferKey`s for objects of that realm fail on their next use. The window this leaves open is ACCESS-D8.

Decide the rights set, whether this mapping holds, whether capabilities can be time- or use-limited, and the error each refusal returns (ACCESS-D2).

## ACCESS-D5 — Who may change the ontology

The ontology decides what terms exist and what rules derive (ONTOLOGY-DESIGN); a rule can widen what a realm label covers (ACCESS-D1 containment) or derive a hidden tag on visible objects. Decide who may install, upgrade and remove modules and create tags — a capability of its own — and whether rules may name realm labels or hidden tags at all.

## ACCESS-D6 — Subscriptions under visibility

A subscription delivers events only for objects visible to its session; decide whether visibility is fixed at subscribe time or follows the session's current realms, what happens to pending events when an object leaves the subscriber's visibility, and how this meets subscription ownership ([subscriptions-decisions.md](subscriptions-decisions.md) Future work, "Ownership and delivery").

## ACCESS-D7 — Realms and encryption keys

Whether visibility is backed by encryption: a key per realm (`PerRealm`, objects of a realm share a key derived from the realm's root) or per object, held by the realm's key manager, so that holding a realm's capability is what lets a domain obtain the key. Candidate answer to [ENCRYPTION-D5](encryption-decisions.md) (per-tag keys) and part of [ENCRYPTION-D6](encryption-decisions.md). Earlier work claimed sharing an object into another realm needs no re-encryption; that holds only if the other realm's members are given the first realm's key, which defeats per-realm keys — decide whether sharing re-encrypts. Deduplicated content is a further leak: shared pieces are encrypted under their hash (ENCRYPTION-DESIGN§1), so pieces shared across realms reveal to anyone who can read the stored bytes that objects in different realms hold equal content; decide whether shared pieces are confined to one realm (a dedup domain per realm or key).

## ACCESS-D8 — Visibility across nodes

How realm definitions, memberships and policies propagate in cluster sync (CLUSTER-DESIGN§1), in what order against the changes they govern, and the window during which a node may still serve access a removal has revoked elsewhere. Ties to [CLUSTER-D2](cluster-decisions.md) and [CLUSTER-D9](cluster-decisions.md).

## ACCESS-D9 — Auditing access

Whether reads and access decisions (queries, opens) are recorded, and where, without turning every read into a metadata write — the same tension as access time ([METADATA-D7](metadata-decisions.md)); decide them together. The pool event log ([POOL-D5](pool-decisions.md)) is one candidate home.

## ACCESS-D10 — Unix permissions

`unix-mode`, `unix-uid` and `unix-gid` (UNIX-DESIGN "Attribute keys") are interop metadata in a capability system. Decide whether the FUSE bridge enforces them (`default_permissions`) or presents them only, and how a FUSE mount maps to a session's visibility (a mount shows what its session may see).

## Future work

- Storage quotas per realm, through [POOL-D2](pool-decisions.md).
