# TODO

Running list of deferred work, scoped to "things we know we want but
chose not to ship in the commit that surfaced them." Newest items at
the top within each section.

## Wallet

- **Token-2022 support.** Today the wallet only lists/sends classic
  SPL tokens (`Tokenkeg…`). Token-2022 mints (`TokenzQd…`) won't show
  up. Fix: second `getTokenAccountsByOwner` RPC with the Token-2022
  program filter, plus the Token-2022 Transfer instruction variant
  (it supports transfer fees + extensions, so the data layout
  differs).

- **Auto-create recipient ATAs.** Sending an SPL token to a wallet
  that's never received that mint fails with `AccountNotFound`. Fix:
  prepend an `AssociatedTokenAccount::CreateIdempotent` instruction
  to the transfer tx. Costs the sender ~0.002 SOL in rent per
  creation, but makes "send to any pubkey" Just Work.

- **Live-session username rename.** The identity card's edit
  affordance updates the local identity + persists it, but takes
  effect only on the next Connect. Mid-session renames need a new
  protocol message (`ClientMessage::UpdateUsername`) + server-side
  member-row mutation + broadcast.
