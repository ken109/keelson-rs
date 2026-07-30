# The shared test schema

Every grammar test names tables from this schema rather than inventing its own, so
the same SQL can be handed to a real engine (`--features live`), where `PREPARE`
resolves tables and columns and therefore catches mistakes a syntax parser cannot.

Kept deliberately small but shaped to exercise what the tests need: a nullable and
a non-nullable column, an integer to aggregate over, a timestamp to order by, a
one-to-many pair for joins, and a join table for many-to-many. Layers 2 and 4 will
reuse it, so it is worth keeping stable.

One file per dialect, because the types genuinely differ — that is the same reason
each dialect is hand-crafted rather than shared.
