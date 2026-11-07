# Indices — open decisions

Design: [index-design.md](index-design.md) · Plan: [index-implementation-plan.md](index-implementation-plan.md)

## INDEX-D4 — Materialized terms in forward entries

INDEX-DESIGN§2.1 lists both direct and materialized terms in an object's forward entry, with an origin bit each. Materialized terms are derivable from the direct ones and the ontology, and at a typical one to two implied ancestors per tag they double or triple the forward index (about 0.6 → 1.2–1.8 GB at 10 M objects). Options:

- **(a) List both** (current specification): deletion cleanup and de-materialisation read the forward entry alone.
- **(b) List direct terms only:** cleanup recomputes each object's implied terms from its direct tags and the ontology at the time of the change; the forward index shrinks to direct terms; an ontology change that alters implications must re-derive the affected objects' postings, which it does anyway.

Shapes [INDEX14](index-implementation-plan.md).

## Future work

- None recorded.
