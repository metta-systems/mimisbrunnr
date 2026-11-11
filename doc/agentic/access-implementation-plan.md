# Access control — implementation plan

Design: [access-design.md](access-design.md) · Decisions: [access-decisions.md](access-decisions.md) · Order: [operations-plan.md](operations-plan.md)

## Visibility

- [ ] **ACCESS1 — Visibility mask in the executor.** Every query result, `Not` and faceted count is intersected with the session's visibility, window by window, as ACCESS-D1 selects. Prerequisites: ACCESS-D1, [QUERY14](query-implementation-plan.md). Acceptance: two sessions with different visibility get results, `Not` results and facet counts that never include an object outside their own visibility; the property test of [QUERY16](query-implementation-plan.md) holds per session.
- [ ] **ACCESS2 — Existence hiding.** Lookups by id on every surface answer as ACCESS-D2 selects. Prerequisites: ACCESS-D2, ACCESS1. Acceptance: an object outside a session's visibility and a never-created id give identical answers to a record read, an open, a `/paths/` lookup and a relation-target read.
- [ ] **ACCESS3 — Tag visibility.** Hidden and name-only terms on every enumeration surface, as ACCESS-D3 selects. Prerequisites: ACCESS-D3, ACCESS1. Acceptance: a session outside a hidden tag's realm gets the same answers as if the tag did not exist from a query naming it, a dictionary scan, an n-gram `Contains`, a facet listing and the `/tags/` view.

## Sessions and rights

- [ ] **ACCESS4 — Sessions and rights.** Session and object capabilities with the rights ACCESS-D4 selects; realm changes gated by their own right. Prerequisites: ACCESS-D4, ACCESS1. Acceptance: a session without the administer right cannot change an object's realms; removing an object's last realm is refused; a diminished capability cannot regain a right.

## Subscriptions

- [ ] **ACCESS5 — Subscriptions under visibility.** Events filtered by the subscribing session's visibility as ACCESS-D6 selects. Prerequisites: ACCESS-D6, ACCESS1, [SUBSCRIPTION8](subscriptions-implementation-plan.md). Acceptance: an object entering a subscription's query outside its session's visibility produces no event; one later shared into its visibility produces `Entered`.
