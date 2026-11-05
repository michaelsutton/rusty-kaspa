# Covenant-ID UTXO notifications: guarantees and limits

**Status:** Draft.

## Feature and mechanism

Add RPC subscriptions for UTXO changes filtered by a set of covenant IDs. The node
filters its existing UTXO-change batches by each entry's `covenantId` and sends
matching additions and removals through the existing `{ added, removed }`
notification payload. Clients can subscribe and unsubscribe by covenant ID,
without knowing the covenant outputs' scripts or addresses.

Clients query the covenant index for the initial live set, then apply notifications
to track changes.

## Scope

State follows the node's virtual chain and can change on reorgs.

Each UTXO's outpoint and accepting DAA score (`blockDaaScore`) allow retrieval of its
source transaction through `getTransactionsByAcceptingBlock`, including populated input
UTXOs and their scripts. Retrieval and backward traversal require the transactions
to remain within the node's retained history and the accepting context to be valid.

## History limits

- A removal identifies the removed output, not the transaction that spent it.
  A visible successor in the covenant exposes that transaction through its own
  outpoint. Without such a successor, learning the spender is not guaranteed.
- Notifications describe net changes per batch. An output created and consumed
  within the same batch may never appear, even if it temporarily continued the
  covenant.
- Backward traversal from surviving outputs can recover their retained ancestry.
  It does not guarantee discovery of every covenant transaction or branch.
- DAA-bounded index queries select current UTXOs by creation score. They do not
  replay spends or provide a complete suffix of activity. Re-querying after a
  disconnection restores current state, not necessarily the missed history.

## Combined example

Initially, covenant output `U0` is live. In one notification batch:

```text
T1: U0 -> U1       (U1 belongs to the same covenant)
T2: U1 -> none     (no successor in that covenant)

Notification: removed = [U0], added = []
```

`U1` never appears in the notification. The delta identifies neither `T1` nor
`T2`; `U0`'s outpoint identifies only its earlier source transaction.

The reliability contract is visibility of current live covenant UTXOs and access
to their retained source transactions. Complete transaction-history visibility
is not guaranteed.

## Appendix: transition from scan to notifications

The index and notifications describe the same sequence of UTXO states. Each page
reads under the index lock; an update completes before its notification is emitted.
With complete, ordered notification coverage, an overlapping scan can therefore
be reconciled without a frozen snapshot: unchanged outputs are found by the scan,
and changed outputs are corrected by their latest notification. Outputs created
and consumed within one accumulated batch are invisible to both paths.

The client flow, for either addresses or covenant IDs, is:

1. Install the handler and buffer notifications before requesting the subscription.
2. Scan all pages for the fixed scope into a map keyed by outpoint.
3. Replay buffered deltas in order, removing before inserting/replacing within
   each batch. Keep removals even for outputs not yet encountered by the scan.
4. Switch atomically from replay to live processing through the same serial handler.
5. Restart reconciliation after a disconnect or UTXO-set override.

The buffer can store the latest operation per outpoint, including removal
tombstones. Notifications must retain source order through reorg removals and
re-additions. Until the stream catches up, the merged state can lag or temporarily
regress relative to scan results.

Current subscription calls can return while upstream registration is still queued;
parallel broadcaster workers also do not establish per-client source ordering by
construction. Thus the flow above is conditional, not yet a gap-free handoff
contract. A missed removal between an early page and subscription activation can
leave a stale output indefinitely.

A proposed server addition is an ordered UTXO notification **fence**:

- All preceding subscription changes have taken effect throughout the pipeline.
- A marker passes through the notification stream after preceding index updates
  and their matching deltas; later deltas cannot overtake it.
- The client dispatcher observes preceding deltas before resolving the fence.

The client would await a READY fence before scanning, then a CAUGHT-UP fence after
the last page, reconcile through that marker, and continue live. This requires
notification ordering and pipeline coordination, but no pinned database snapshot
or historical replay log. A DAA score alone is not such a fence. The fence is a
separate proposal, not part of the first covenant-notification implementation.
