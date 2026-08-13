# Networking: who can reach you, and who can read you

Two questions this document exists to answer, because nothing else in the repo
answers either directly:

1. When I self-host, **who is actually involved** in carrying my traffic?
2. **What can each of them see?**

Read `SELF_HOSTING.md` first for how to run a server; this is about what happens
on the wire once you do.

---

## What self-hosting does today

Self-hosting is real hosting. Your client spawns the gateway in-process and
connects to it over **loopback** — `ws://127.0.0.1:{port}`, `host.rs` — never
through a rendezvous, whether or not you registered with one. Your guilds,
members and message history live in `host-data/` on your disk. `rendezvous_url`
is an `Option`: leave the checkbox off and there is no registration, no relay,
and your own server mints its own LiveKit tokens with its own key.

What a rendezvous supplies is **reachability**, not hosting:

| | Your client | LAN friend | Friend over the internet |
|---|---|---|---|
| Self-host, no rendezvous, no mapping | loopback | direct to your LAN IP (needs direct connections enabled) | **cannot reach you** |
| Self-host + rendezvous, no mapping | loopback | direct to your LAN IP | control relayed; media via the relay's SFU |
| Self-host + a port mapping | loopback | direct | **direct**, if they have your code or your address |

The bottom row is Stage 1, and it is now implemented — `client/src/portmap.rs`
asks the router for a forward, the resulting address rides on `Register`, and a
friend joining by code races it against the relay (`net::connect_preferring_direct`).
It is not universal and never can be: behind carrier-grade NAT, or on a router
with UPnP and NAT-PMP both off, there is nothing to obtain, and such a host falls
back to the middle row. **Which row you are on is shown in the host banner**, with
the reason when it is not the one you wanted.

The relay is a tunnel to your server, not a replacement for it: `run_adapter`
dials your *own loopback gateway* and bridges it outward. The rendezvous stores a
registration — a shortcode or claimed name, and a description. It never stores
messages.

---

## What each party can see

| | Control (messages, presence, tokens) | Voice / screen video |
|---|---|---|
| **Anyone on the network path** | **Readable.** The gateway is plaintext — see the caveat below | Encrypted in transit (WebRTC DTLS-SRTP) |
| **A rendezvous relay, when used** | **Readable** | **Readable** — its SFU decrypts and re-encrypts |
| **The host** | Readable | Readable |
| **A LAN friend, on your wifi** | **Readable** — `allow_lan` is plaintext too | Encrypted in transit |

**The gateway is not encrypted.** `tokio-tungstenite` is pinned with no TLS
backend, so `wss://` cannot be dialled even though the client accepts the scheme.
This is tracked in `TODO.md` under Security and is the first thing on the roadmap
below. Until it is fixed, "direct connection" and "private connection" are not
the same thing — a direct plaintext socket is still readable by every hop.

**Media is encrypted in transit but terminates at an SFU.** WebRTC always
encrypts on the wire. But an SFU is not a pipe: it decrypts, routes and
re-encrypts. When the relay's SFU carries your call, the relay operator can see
it. When your own bundled SFU carries it, you can — which is the same trust you
already place in whoever runs the guild.

**And when a rendezvous supplies an SFU, it carries *everyone* — including the
host.** `url_for_client` returns the relay's URL unconditionally, ahead of any
other logic, so it is handed to every participant rather than only to the remote
ones. Your own voice and screen share leave your machine, reach the rendezvous,
and come back to you. So on that path it is not peer-to-peer, and not even
friend-to-host: both endpoints are talking to a third machine. (The bundled
`livekit-server` at least no longer starts in that case — it used to come up,
bind 7880 and serve nobody for the whole session.) Whether a rendezvous supplies
an SFU at all is up to its operator; one configured without LiveKit credentials
supplies none, and media then uses the host's own — which works for a remote
friend only if the host has a port mapping, and otherwise hands them a LAN
address they cannot dial. That last case is confirmed in
`server/tests/rendezvous_voice.rs` and recorded in `TODO.md`.

**The host can read everything, deliberately.** Message content is stored
readable in `host-data/`, which is what keeps search, moderation, the audit log
and retention working. Encrypting content against the host would break all four
and is **not** a goal here — the threat model is third parties, not the person
whose machine the guild runs on. NIP-44 appears on the roadmap for DMs; do not
read that as implying guild channels are encrypted from the host.

---

## Three tiers of reachability

Worth stating plainly, because one constraint governs the whole design:

> **Hole punching requires a third party.** Two machines behind NAT cannot learn
> each other's public addresses unaided — someone has to observe both ends and
> tell each about the other. That is true of ICE, STUN, WebRTC and QUIC-based
> stacks alike. "Nobody else involved" and "hole punching" cannot both hold.

So there are three, and only the first involves nobody:

1. **Reachable yourself.** A port mapping (UPnP/NAT-PMP) or a manual forward
   gives you a real address. Nobody else is involved at any point. Fails behind
   carrier-grade NAT, where your ISP does not give you a public address at all.
2. **Coordinated, not carried.** A coordinator arranges a direct connection, then
   steps out. It learns that two peers connected; it never sees what they
   exchange.
3. **Relayed.** The direct attempt failed, and a relay carries the data. This is
   what happens today, for everything.

**Settings map onto these, and say which they are.** *Publish to rendezvous*
gives you the directory, join codes and tier 3. *Accept direct connections* is
tier 1: it binds the gateway off loopback and lets the port mapping be
attempted — a forward to a loopback-bound port lands on nothing, so the two
cannot be separated. A further setting will allow a coordinator for tier 2, kept
separate because a coordinator contacted silently is exactly the surprise this
design exists to remove. With rendezvous off you get tier 1 or nothing, and the
host banner says which, rather than leaving it to be inferred from a friend
failing to connect.

---

## Roadmap

### Stage 1 — port mapping · **done**

The only tier-1 answer, self-contained, and the only stage that helps **voice**,
since media is LiveKit on its own path and no gateway transport will carry it.

What it does, in order (`client/src/host.rs::start_self_host`):

1. **Bind the gateway first**, so the port to map and advertise is the port we
   actually got — `bind_with_fallback` does not always give back the one asked
   for. `dioxusfun_server::spawn_on` then serves on that same listener.
2. **Ask the router** (`client/src/portmap.rs`): UPnP-IGD via `igd-next`, then
   NAT-PMP. Four ports — the gateway, LiveKit's signalling and ICE/TCP ports,
   and its UDP mux. A one-hour lease renewed at half, withdrawn on shutdown.
   Failure is normal and never fatal.
3. **Check for hairpin NAT** by connecting to our own new public address. This
   decides whether LiveKit may advertise it, because LiveKit *replaces* its LAN
   ICE candidate with the advertised one rather than adding to it — advertising
   blind would trade the LAN path for the remote one.
4. **Advertise** the address on `Register`; the rendezvous returns it from
   `/discover` and from `/resolve/{code}`.
5. **Race it** against the relay on the joining side, keeping whichever answers
   and preferring direct. Both are dialled at once because the interesting
   failure — a mapping that has since expired — is a black hole rather than a
   refusal, and would otherwise cost a minute of OS timeout.

**The ports were narrowed first.** `livekit_bundle.rs` wrote a 100-port UDP
range — a hundred mappings to request — and now writes LiveKit's single-port UDP
mux instead. External advertising is `node_ip` rather than `use_external_ip:
true`, because the latter resolves over STUN during config validation and *exits
the process* when that fails; the router already told us the address, so there is
nothing to look up.

Not verified anywhere in CI: that a mapping appears on a real router at all. See
`TODO.md`.

### Stage 2 — an encrypted, key-authenticated transport

Two jobs, only one of them reachability.

**Encryption**, needed regardless of anything else here. The obstacle is that a
home-IP host has no domain and no CA, so ordinary TLS means a self-signed
certificate pinned to the host's Nostr key — and a custom certificate verifier,
whose failure mode is the silent one where accepting everything looks like
working. Preferring a reviewed library over hand-rolled verification is the whole
argument for a QUIC transport (`iroh` is the candidate) where authentication is
by public key by construction.

**Coordinated hole punching** for tier 2, and dial-by-public-key, so a join code
becomes the host's node key: no IP, no domain, no port forwarding.

The WebSocket stays — `SessionMode::Remote`, LAN, and any future browser client
use it, and a domain-hosted server behind a reverse proxy already has real TLS.
This is a second transport, not a replacement.

**"Coordinator, never carrier" must be enforced rather than assumed.** A relay
that coordinates a punch will also happily carry the data when the punch fails.
Honouring the setting means checking the connection type and **refusing** a
relayed connection, reporting the host unreachable instead. That is the entire
difference between tier 2 and tier 3.

### Stage 3 — end-to-end encryption for relayed media

Only relevant when the relay carries media, which is the one case a third party
sees it. LiveKit supports E2EE on both paths in use here, so the SFU keeps
routing frames it can no longer decrypt.

The work is key distribution, not cryptography: a per-channel key every member
holds and no server does, **rekeyed on kick and ban** — otherwise a removed
member keeps decrypting whatever they can still capture. It has to reach the
webview (camera, screen video) and both native rooms, and one user holds several
identities in the screen room.

This supersedes an earlier idea of replacing the relay's SFU with a TURN server:
E2EE needs no new service and defends against a *malicious* relay rather than
merely an honest one running less software.

---

## Trade-offs worth knowing before turning any of this on

**A direct connection exposes your home IP** to everyone who joins — and, since
the address rides on the listing, to anyone who can read the listing or guess a
shortcode. The relay hides it, which is a feature for a guild open to strangers,
not an accident. As shipped, a host that obtains an address advertises it
whether or not it is publicly listed; a host that would rather stay hidden turns
off direct connections. Splitting those two is open work.

**Port mapping asks your router to open a hole.** UPnP is widely enabled and
widely criticised for exactly that reason. It is a per-host choice, and one an
operator should make knowingly — which is why *Accept direct connections* says
so in as many words, rather than the mapping happening quietly behind a checkbox
about the LAN.

**Tier 1 is not universal.** Behind CGNAT there is no address to map, and no
amount of local configuration changes that. Such a host needs tier 2 or 3, or is
LAN-only — and should be told which, rather than left to guess from a failure.
