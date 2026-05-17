"""PostgreSQL advisory-lock classid registry.

PostgreSQL exposes two flavours of advisory locks: the
single-argument form (one int8 namespace, cluster-wide) and the
two-argument form (two int4 namespaces, also cluster-wide but
partitioned by the first argument -- the *classid*). ELSPETH uses
the two-argument form exclusively so that each subsystem holding
advisory locks gets its own classid namespace, avoiding cross-
subsystem collision (and cross-application collision with any
other software using single-argument advisory locks on the same
cluster).

This module is the SINGLE registry of classid values. Adding a
new advisory lock anywhere in ELSPETH MUST add a new constant
here with a distinct value. Reusing a classid across subsystems
re-introduces the collision risk that splitting the namespace
was meant to eliminate.

ABI commitment
--------------
Every constant defined here is **on-the-wire ABI**. Two ELSPETH
instances on the same Postgres cluster -- including instances
running different ELSPETH versions during a rolling deploy --
MUST agree on the value of every classid in this module, or
they will not serialise against each other on the same logical
resource. A version mismatch on a classid value produces a silent
correctness violation: both instances think they hold the lock,
both execute the protected code path concurrently, and the
correctness guarantee the lock was protecting is lost.

Changing any constant here therefore requires:

1. An ADR documenting the rationale and the migration plan.
2. A coordinated deploy that drains all writers using the old
   value before any writer using the new value comes online.
3. A schema/runbook update so operators understand the
   constraint.

The 32-bit signed integer space is enormous (~4.3 billion
values); pick distinct values, never reuse a retired value
within the same major release.
"""

from __future__ import annotations

# 0x454C5350 = 1,162,629,968 -- ASCII "ELSP" big-endian. First
# classid assigned in this registry; chosen so a Postgres operator
# inspecting pg_locks sees a recognisable value rather than a random
# magic number. Used by SessionServiceImpl._acquire_session_advisory_lock
# (src/elspeth/web/sessions/service.py) for the session-scoped write
# lock that serialises persist_compose_turn / save_composition_state /
# set_active_state writers within a single Postgres cluster.
ELSPETH_SESSIONS_LOCK_CLASSID: int = 0x454C5350
