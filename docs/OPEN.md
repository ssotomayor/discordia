# Open work

Deferred work is **GitHub issues**, not this file:

```bash
gh issue list                      # everything open
gh issue list --label voice-audio  # one area
```

Labels mirror the areas this file used to group by. Entries 1–115 were migrated
to issues #124–#183 on 2026-09-05; `git log docs/OPEN.md` has the old list, and
each issue names the entry it came from.

What stays here is the work that is not a tracked item: where the project is,
and the decisions taken deliberately.

## Phases

| Phase | State |
|---|---|
| P1 persistence · P2 deploy artifacts · P4 safety · P5b catalog · P5a core · P6 export/import + named rendezvous | done, tested |
| P3 web/PWA client | deferred, needs a browser |
| P5a tail — delta-sync resume, 2k-connection benchmark | open |
| P6 tail — signed "guild moved" redirect, cross-instance media copy | open (#149) |
| P7 cluster mode | demand-gated |
| Postgres backend · role hierarchy + channel overwrites · level curve · Nostr zaps | direction, not scheduled |

## Accepted trade-offs — recorded, not tracked

- Windows captures the screen in the webview. WebView2 is Chromium; `getDisplayMedia` works.
- Stream audio is subscribed on publication, not on watch.
- Every Windows activation leaks its 12-byte blob, deliberately.
- The Windows blob's lifetime rule is an observation, not a contract.
- Two advisories are ignored in `.cargo/audit.toml`, each with its reason
  written beside it: `rsa` arrives through `jsonwebtoken` and never runs an RSA
  operation here, and `tract-nnef` is pinned by a model that will not load on a
  newer one. Ignores without a reason are bugs.
- A sender's clock is corrected, not trusted (trap 18), and only from a message
  that arrived once every connected relay had finished replaying. A relay that
  is down at launch and comes up later replays into a session already counted
  live; its events are only ever *samples*, so the worst that costs is a
  correction learned from a stale message, and the next live one replaces it.
  Nothing is corrected until such a message exists, which is why a conversation
  that has only ever been replayed keeps the sender's stamps.
