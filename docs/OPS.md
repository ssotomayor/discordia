# Ops

## Three ways to run it, same binary

| Rung | For | How |
|---|---|---|
| One click | friend groups | "Host my own" in the client — runs the gateway in-process and launches a bundled SFU child; the rendezvous's SFU is used only when friends cannot reach this machine's media ports |
| One box | communities | `curl -fsSL https://discordia.world/install.sh \| sh` on a Linux VPS — [Setting up a box](#setting-up-a-box); `cargo run -p discordia-server` to develop |
| Cluster | giants | not built (entry: demand-gated) |

## Environment

On Windows, `Discordia.exe` includes the app icon and product/version metadata
even in plain Cargo builds. `build.rs` embeds those resources for Cargo;
`dx` owns them when `DX_RUSTC` is set, using `client/Dioxus.toml`.
Its taskbar identity is `com.discordia.app`;
the bundled SFU appears as `Discordia-RTC.exe`, with its version kept in an
internal `rtc-<digest>` directory. WebView2 remains a
Microsoft runtime with separate processes; Task Manager grouping varies by
Windows/runtime version. The installer leaves one top-level Start Menu icon and
no separate uninstall shortcut, so Uninstall lives on that icon's context menu.
Cargo is a development launcher, not a shipped service.

Every server and rendezvous variable below is also a `--flag` and a key in a
TOML file's `[server]` or `[rendezvous]` table: drop `DISCORDIA_` (or
`DISCORDIA_RENDEZVOUS_`) and lowercase (`DISCORDIA_QUIC_PORT` → `quic_port`,
`LIVEKIT_URL` → `livekit_url`). A flag beats the variable, which beats the file.
`--config`/`DISCORDIA_CONFIG` names the file, else `/etc/discordia/discordia.toml`
when it exists; a misspelt key or table is refused at start. `LIVEKIT_PORT` is a variable only.

**Server**

| Var | Default | Meaning |
|---|---|---|
| `DISCORDIA_PUBLIC_ADDRESS` | — | IP or DNS name friends reach the box at: first in the share string, and what the bundled SFU advertises and hands callers. Without it, a box behind 1:1 NAT (most clouds) hands callers a private address |
| `DISCORDIA_ADDR` | `0.0.0.0:9000` | plaintext gateway bind — for loopback and a TLS proxy; clients refuse `ws://` to anything else |
| `DISCORDIA_QUIC_PORT` | `9001` | UDP port the QUIC endpoint binds — the one to publish and open; `0` takes a random free one |
| `DISCORDIA_RELAY_URL` | — | an iroh relay (a rendezvous's `/config` names one) that introduces friends behind NAT and carries ciphertext when a punch fails |
| `DISCORDIA_DATA_DIR` | `./discordia-data` | SQLite, media blobs, `livekit-keys`, `quic-secret` (the key in the share string; back it up or friends re-add you) |
| `DISCORDIA_MEDIA_MAX_BYTES` | `2 GiB` | cap on `<data dir>/media`; uploads are refused past it |
| `DISCORDIA_OPERATORS` | — | comma-separated hex pubkeys who own the system guilds, the seeded Lobby among them: every owner right but transfer and leave, and delete only while another guild is the server default. `setup` requires one for a server |
| `DISCORDIA_PUBLIC_HOSTS` | — | comma-separated `host`, `host:port` or URLs clients dial (a DNS name, a reverse proxy). Loopback and every interface IP are always accepted; a login signed for any other address is refused |
| `LIVEKIT_URL` | derived per-connection | SFU URL handed to clients |
| `LIVEKIT_API_KEY` / `_SECRET` | generated into `<data dir>/livekit-keys` on first run | set only for an external SFU (`LIVEKIT_URL`); must match it |
| `LIVEKIT_PORT` | `7880` | port used when deriving the URL (the bundled SFU always binds 7880) |
| `DISCORDIA_LIVEKIT_AUTOSPAWN` | `1` | `0` when LiveKit runs separately |
| `DISCORDIA_ICE_SERVERS` | — | JSON list of `{urls, username, credential}` handed to every voice client; for a box behind NAT with its own TURN |
| `DISCORDIA_PUBLISH_TO` | — | comma-separated `ws://`/`wss://` rendezvous to register on, so friends join by name or code; unset publishes nowhere. Retried with backoff while one is down |
| `DISCORDIA_PUBLISH_NAME` | random code | the name friends type; held by a key derived from `quic-secret`, so a restored backup keeps it |
| `DISCORDIA_PUBLISH_DESCRIPTION` | — | one line beside the name |
| `DISCORDIA_PUBLISH_PUBLIC` | `false` | also list it in each rendezvous's `/discover`; otherwise only the name or code finds it |
| `DISCORDIA_PUBLISH_LOCATION` | — | `lat,lon` for the globe; self-declared, rounded to a tenth of a degree |

**Rendezvous**

| Var | Default | Meaning |
|---|---|---|
| `DISCORDIA_RENDEZVOUS_ADDR` | `0.0.0.0:7700` | HTTP bind: `/control`, `/discover`, `/resolve`, `/config`, `/voice-token`, `/voice-evict` |
| `DISCORDIA_RENDEZVOUS_DATA_DIR` | `./rendezvous-data` | name reservations, `reservations.db` (SQLite) |
| `DISCORDIA_RENDEZVOUS_RELAY_ADDR` | `0.0.0.0:7701` | iroh relay bind, the one that carries ciphertext for hosts behind NAT |
| `DISCORDIA_RENDEZVOUS_RELAY_URL` | — | how clients reach that relay; handed out in `/config` and every entry |
| `DISCORDIA_RENDEZVOUS_TURN_URL` | — | enables the TURN relay: what clients dial, e.g. `turn:rendezvous.example:7702`. Hosts behind NAT keep their calls on their own SFU; friends who cannot reach it are relayed here, ciphertext only |
| `DISCORDIA_RENDEZVOUS_TURN_ADDR` | `0.0.0.0:7702` | UDP bind for the relay's control socket |
| `DISCORDIA_RENDEZVOUS_TURN_PORTS` | `7710-7809` | UDP range relayed media is allocated from; open it with the bind. Roughly eight ports per relayed friend, so the default seats a dozen |
| `DISCORDIA_RENDEZVOUS_TURN_RELAY_IP` | resolved from the URL | the public IPv4 handed out in allocations; set it when the URL's name does not resolve to this machine |
| `DISCORDIA_RENDEZVOUS_TURN_SECRET` | random per start | HMAC secret behind the time-limited credentials; hosts renew theirs over the control stream, so a restart costs nothing |
| `LIVEKIT_URL` / `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | — | a shared SFU for hosts whose media ports nobody outside can reach *and* that have no relay; the rendezvous mints their tokens so no host holds the secret. A host that maps its ports, or has a relay, carries its own calls and ignores this |

Deploy the coordinator with `/voice-evict` alongside these clients. A host's
registration grant permits removing seats only from its own room namespace;
an older coordinator cannot enforce shared-SFU kicks.

Serve it on the address it binds, not behind a reverse proxy: the per-address
limits and the check that a host's advertised address is its own both read the
peer's IP, and a proxy makes every host the same peer.

**Client**

| Var | Default | Meaning |
|---|---|---|
| `DISCORDIA_CONFIG_DIR` | OS config dir | identity, settings, release log; a pre-rename `dioxusfun` folder is moved here once |
| `DISCORDIA_WEBVIEW_DEBUG_PORT` | off | Windows Debug only: opt-in loopback WebView diagnostics on a nonzero TCP port; release builds ignore it |
| `DISCORDIA_VAULT` | chosen on first run | `keychain` or `file`: where the passphrase that locks key files lives; changing it later means re-importing every key |
| `DISCORDIA_RENDEZVOUS_URL` | `ws://rendezvous.discordia.world:7700` | presets the rendezvous; plain `ws://` because the rendezvous is never behind TLS |
| `DISCORDIA_DM_ICE_SERVERS` | `[{"urls":["stun:stun.cloudflare.com:3478"]}]` | JSON list of STUN/TURN servers for private DM calls; each entry accepts `urls`, `username`, `password`. Credentials stay local and are never sent through Nostr |
| `DISCORDIA_DM_RELAY_ONLY` | off | `1` requires a configured TURN server and excludes direct media connections |
| `DISCORDIA_E2EE` | on | `0`/`off` disables media encryption |
| `DISCORDIA_E2EE_KEY` | — | passphrase shared by hand; developer path |
| `DISCORDIA_E2EE_OVERLAP` | off | overlap voice keys across a rekey — **unverified** |
| `DISCORDIA_VOICE_RECORD` | off | a directory: start Diagnostics → *Record calls* at launch, writing there instead of `<config dir>/voice-recordings` |

DM calls wait up to 10 s for ICE gathering before trying the candidates already collected. No candidates produces an explicit connection error; a partial list does not guarantee connectivity through NAT. The default is STUN only: networks requiring a relay still need a configured TURN server.

Guild migration: `cargo run -p discordia-server -- export --guild <uuid> f.json`
then `import f.json` on the target. Fresh ids, pubkeys preserved.

## Community file attachments

| Setting | Default | Where |
|---|---|---|
| File size | 2 MB | Chat → Attach file → Save file limits (Manage guild); 1–2000 KB |
| File lifetime | 7 days | Same controls; 1–365 days, applied to new uploads |
| Total media storage | 2 GiB | `DISCORDIA_MEDIA_MAX_BYTES`; shared by files, pictures and sounds |

Expired files cannot be downloaded; the message retains its name and size. The
host checks every minute and removes blobs with no live reference, including
on restart. A live second attachment keeps shared bytes until it expires.
Files remain opaque `.bin` blobs; neither host nor client opens them. After a
successful download the client opens its destination folder, never the file. Windows
marks stored blobs and saved downloads as Internet files. There is no malware
scanner. Expiry cannot remove copies someone already downloaded. Profile and
community artwork, stickers and inline chat pictures follow their existing
reference/retention rules; file expiry applies to **Attach file** uploads.

## DM voice calls

| Property | Behavior |
|---|---|
| Scope | One-to-one voice, both clients open; no guild server or LiveKit room required |
| Signaling | Discordia-specific versioned kind 24133 rumor inside NIP-59 kind 1059; NIP-40 expiration and authenticated 60 s freshness checks; not interoperable with other clients' call drafts |
| Identity | Nostr-authenticated SDP binds the WebRTC DTLS fingerprint to the contact; relays carry encrypted signaling, never audio |
| Media | Native WebRTC, DTLS-SRTP; existing Rust microphone DSP, echo reference and playback mixer |
| Consent | Incoming panel with Accept/Decline; microphone opens only after acceptance |
| Devices | First accepted device receives the offer; other incoming panels are dismissed |
| Connectivity | Default STUN attempts direct media; restrictive NAT/firewalls require TURN. Direct calls expose peer network addresses to the other participant |
| Teardown | End call closes the peer connection, capture, playback and tasks; network interruption gets a 15 s recovery window |

TURN is a separate service (for example self-hosted coturn), not bundled or
automatically deployed. Configure its public address, authentication and media
ports, then supply credentials to both clients. The iroh relay cannot replace it.

PowerShell example, before launching Discordia:

```powershell
$env:DISCORDIA_DM_ICE_SERVERS = '[{"urls":["stun:turn.example.org:3478"]},{"urls":["turn:turn.example.org:3478?transport=udp","turns:turn.example.org:5349?transport=tcp"],"username":"alice","password":"YOUR_TURN_PASSWORD"}]'
$env:DISCORDIA_DM_RELAY_ONLY = '1'
```

Verify two identities on different networks, then repeat with relay-only mode.
Check decline, timeout, hangup, mute/deafen, volume and application exit. Automated
peer tests use synthesized PCM and real Opus decoding without audio hardware.

## Published names

| Situation | Rendezvous behavior |
|---|---|
| Host quits or is killed | Socket closes; live listing and its voice grant go at once |
| Host loses its link or sleeps | Dropped after 60 s without a pong (pinged every 20 s) |
| Name not live for 1 day | Reservation expires (`ReservationCaps::ttl_secs`); checked on every claim and by the hourly `sweep`, which also refreshes live names so a restart cannot expire them |
| Coordinator restarts | Reloads reservations from disk; hosts register again |
| Same identity registers a name it holds | Accepted, even while an earlier session lingers: that one gets `Superseded` and its client stops re-registering. Hexadecimal key casing is ignored |
| Another identity requests a reserved name | `already taken — reserved by a different identity` |
| Owner sends signed `ListNames` | Its names, live or with seconds until expiry — the Create tab's "Your names here" |
| Owner sends signed `ReleaseName` | Removes the reservation only after the live session has ended |
| Operator frees a name | `sqlite3 reservations.db "DELETE FROM reservations WHERE slug = '<code>'"` while it runs; the table is the only copy, read on every claim |

Name-handling fixes require updating the rendezvous service; updating only the
desktop client does not change the remote registry.

## Reachability

Every connection that leaves the machine is QUIC, authenticated by the key in
the `quic://key@addrs` share string the host banner copies, or in the
rendezvous entry. The plaintext gateway binds loopback only.

| Setup | You | LAN friend | Friend over the internet |
|---|---|---|---|
| Self-host, nothing else | loopback | share string with the LAN address ("Accept direct connections" starts enabled for new hosts) | direct with global IPv6 and an open firewall; otherwise unreachable |
| + rendezvous | loopback | punched, or carried by the relay | gateway introduced or relayed; voice tries usable local routes before the shared SFU |
| + port mapping (UPnP/NAT-PMP/PCP or manual) | loopback | direct | direct on the forwarded QUIC UDP port; voice on this machine's SFU when media ports are usable |
| Community server | loopback or `wss://` proxy | QUIC by share string | QUIC by share string or by the name it publishes (`DISCORDIA_PUBLISH_TO`), or `wss://` through a TLS proxy (`DISCORDIA_PUBLIC_HOSTS`) |

Port mapping failure is the normal case and never stops hosting. It also
measures hairpin NAT for diagnostics. LiveKit keeps LAN candidates alongside the
advertised public address, so hairpin is not required. The check uses a temporary
challenge responder on the mapped media TCP port before LiveKit starts; a
successful probe releases that port before starting the bundled SFU.
The bundle pins LiveKit 1.13.9; overriding `LIVEKIT_BUNDLE_VERSION` with a version
before 1.13.6 loses internal candidates with an explicit public `node_ip`.
SFU readiness requires an authenticated room-list request and a live child,
not just an open TCP port. Startup failures, including occupied media ports,
use the existing rendezvous SFU fallback when available.

Automatic mapping asks UPnP-IGD, NAT-PMP and PCP for a router at once, while
the QUIC endpoint binds, then maps through each router that answered in that
order, continuing after one refuses or renumbers media ports. Only methods
whose router answered get a second round. UDP and ICE/TCP grants are
kept independently; grants on the same public IP combine across methods.
Signaling can use the gateway if its public port is unavailable. PCP retains its mapping
nonce and renews against the granted lifetime. The first successful QUIC
address and granted port survive selection of a voice mapping from another
method, even when their public IPs differ. An unverified manual address cannot
replace a router-granted QUIC mapping or usable automatic voice mapping.
Under "I already forwarded the ports", a public IPv4
address selects fixed QUIC UDP 9001; the form lists the configured media ports.
Manual IPv4 is offered as unverified; callers verify media connectivity. Global IPv6 is tried without
port mapping, but requires the host firewall and the caller's IPv6 connectivity.

Voice tokens offer interleaved IPv4/IPv6 addresses of the same local SFU.
For literal IP endpoints without an HTTP(S) proxy, the client races up to two
unauthenticated TCP probes, staggered by 250 ms, within a 1.5 s total budget.
It prefers the first reachable endpoint but keeps every alternative: TCP
reachability does not prove media connectivity. Authenticated LiveKit joins
remain sequential to avoid duplicate-identity eviction. DNS endpoints and
proxy configurations retain the ordinary connection attempts. Before any remote caller
confirms local voice, exhausting those addresses can select the shared SFU
once for the whole host session, reissuing every caller's tokens. Loopback/LAN
reports cannot select it; after remote local success, a later failure cannot
move existing calls. Without a shared offer, local failures remain errors.
The banner tooltip reports the attempted route or fallback reason.
A caller none of those addresses are routable from (IPv4-only, say, against an
IPv6-only host) still reaches the SFU's signaling: the gateway serves it at
`/sfu/*`, the client dials that through a loopback listener riding its gateway
connection (direct QUIC or the relay, ciphertext either way), and tries it last.
Media then goes by ICE, through the rendezvous's TURN when nothing direct works.
Release builds log to `<config dir>/logs/discordia.log` (macOS
`~/Library/Application Support/discordia`, Windows `%APPDATA%\discordia`);
`grep 'voice route:'` there, on the host and on a caller, shows why a call
landed where it did. Credentials never appear in those lines.

The workspace shows separate Server and Voice indicators on hosts and guests.
Server follows the selected QUIC path, including relay-to-direct changes.
Voice uses selected WebRTC candidate-pair stats for send/receive, independently
of the signaling URL; missing stats remain unknown. Click Voice for local
endpoints and route reports from current callers, refreshed every five seconds.
Only route labels are shared, only within that channel; reports are client telemetry.

## Setting up a box

On a Linux x86_64 VPS:

```bash
curl -fsSL https://discordia.world/install.sh | sh              # systemd, the default
curl -fsSL https://discordia.world/install.sh | sh -s -- --docker
sudo discordia-server setup --dry-run                             # every file and command, nothing done
```

The installer downloads the latest `discordia-server-linux`, checks it against
`release-signing.pub` with minisign, installs it in `/usr/local/bin` and runs
`setup`. Rerun it to upgrade: with a config present and no arguments it only
replaces the binary and restarts. `setup` asks what runs here (server,
rendezvous or both), the public address (detected: an interface's public IPv4,
else STUN), operator keys (at least one for a server, or nobody owns the Lobby), and whether to run TURN and lend the SFU to other
hosts. A server is asked which rendezvous to publish on (none by default), and
then its name, description and whether to list it publicly. Every question is a
flag, and `--yes` takes the defaults (`setup --help`). A rerun reads the existing
file as its defaults and keeps keys it does not manage; `--publish-to none` stops publishing.

| | systemd (default) | `--docker` |
|---|---|---|
| Config | `/etc/discordia/discordia.toml`, 0640 root:discordia | `/opt/discordia/discordia.toml` (`--dir`), mounted read-only |
| Runs | `discordia-server.service`, `discordia-rendezvous.service`, user `discordia`, hardened, state in `/var/lib/discordia` | published images at `--image-tag`; compose `name: discordia` pins the project |
| SFU | bundled child of the server, advertising the public address on 7880–7882 | `livekit` container, host network, `deploy/livekit.yaml`, keys in `.env` |
| Plaintext gateway | `127.0.0.1:9000` | published on `127.0.0.1:9000` |
| Firewall | `ufw` or `firewalld` when active, else the port list is printed | same |
| Share string | printed at the end; later `sudo discordia-server share` | `docker compose -f … exec discordia discordia-server share` |

Back up `quic-secret` in the server's data directory: it is the key in the
share string, and the published name's owner. TLS is not set up (`wss://` through a proxy, `public_hosts`); QUIC needs none.

A rendezvous lists only addresses matching the IP the registration came from, so
the advert carries `public_address` resolved to an IP. An IPv4 box registering
over IPv6 (a rendezvous with an AAAA record) is listed without a direct address.

## Deploying the test box

CI builds the two images on every push to `master` (`deploy-images` in
`ci.yml`, gated on `test`) and pushes them to
`ghcr.io/<owner>/discordia-{server,rendezvous}`, tagged `latest` and the short
sha. **Never build on the deployment box**: the server crate needs more RAM than
a small VPS has, and the OOM killer picks among the containers already running.
The `buildcache` tag beside them is BuildKit's layer cache, not an image; the
dependencies are a `cargo-chef` layer, rebuilt only when a manifest changes.

Updating is then a pull, from a directory holding only `docker-compose.yml`,
`deploy/livekit.yaml` and `.env`:

```bash
cd /opt/discordia
docker compose pull && docker compose -p discordia up -d
```

`deploy/update-rendezvous.sh` does the rendezvous half from any machine with
SSH to the box: it writes a `docker-compose.override.yml` that pins the image
and turns on the TURN relay, opens the UDP ports in ufw, restarts the one
service and probes the relay from outside. `IMAGE_TAG=<short sha>` pins,
`DRY_RUN=1` prints the remote steps.

Rolling back is the same command against an older sha in the image tag. Keep
the deploy directory off any checkout an agent or a timer writes to — a compose
file read out of a working tree is whatever revision that tree last held.

| Trap | Why |
|---|---|
| `-p discordia` | volumes are project-scoped; a different project name orphans `discordia_*` and the box comes up empty |
| `.env` is required | compose uses `${LIVEKIT_API_KEY:?}`, so a missing key fails at start rather than falling back to LiveKit's public `devkey` |
| All three restart together | the gateway, the rendezvous and the SFU must agree on the LiveKit pair |
| `deploy/livekit.yaml` carries no `keys:` | `LIVEKIT_KEYS` supplies them, so a copied file never carries a secret |
| Off loopback the gateway needs QUIC reachable | `9001/udp` published and open (`DISCORDIA_QUIC_PORT`); `DISCORDIA_RELAY_URL` so a blocked UDP port still connects |

## The website

`discordia.world` is a Next.js static export in `site/`, served by the VPS's
nginx from `/var/www/discordia.world`. The VPS never runs Node: it has no
toolchain and cannot build, so `out/` is produced elsewhere and synced in.

| Name | Serves | How |
|---|---|---|
| `discordia.world`, `www` | the site, HTTPS | nginx vhost `discordia.world`, root `/var/www/discordia.world`, certbot cert, `error_page 404 /404.html`, `/_next/static/` immutable for a year |
| `app.discordia.world` | `wss://` gateway proxy to 127.0.0.1:9000 | nginx vhost `app.discordia.world`, WebSocket upgrade, 1 h timeouts |
| `rendezvous.discordia.world` | plain `ws://…:7700`, no proxy, no TLS | bare A record; the rendezvous must see real peer addresses |

| Path | What happens |
|---|---|
| Push to `master` touching `site/**` | `site.yml` builds; if the `SITE_DEPLOY_KEY` secret is set it rsyncs `out/` to `SITE_HOST:SITE_ROOT` (repo variables, default `root@151.243.137.35:/var/www/discordia.world`) |
| Pull request touching `site/**` | `site.yml` builds only |
| By hand | `site/deploy.sh` from a machine holding the VPS key; same rsync |

The deploy key is an ed25519 keypair made for this: the private half in the
secret, the public half in `authorized_keys` on the VPS. Every DNS record stays
unproxied in Cloudflare; a proxied record broke the rendezvous, QUIC and the
`wss://` login at once. `Downloads.tsx` asks the GitHub releases API at page
load, so a new release needs no site deploy; the site has no analytics.

## Devcontainer

A Linux box holding nothing but this repo, for agent sessions run with
`--dangerously-skip-permissions`. `.devcontainer/`, one comment per decision.

```bash
devcontainer up --workspace-folder .
devcontainer exec --workspace-folder . bash     # then: claude --dangerously-skip-permissions
devcontainer up --workspace-folder . --remove-existing-container   # after editing .devcontainer/
```

| Thing | How | Note |
|---|---|---|
| Egress | `init-firewall.sh`, re-applied every start | default deny; a new upstream host needs a line **and an image rebuild** |
| Commit + push | the host's SSH agent forwarded to `/ssh-agent` | no key crosses; commits are SSH-signed, not OpenPGP |
| `.git/config`, `.git/hooks`, `.cargo` | read-only binds | so a hostile `build.rs` cannot make *host* git run code. Costs `git push -u` — push with `git push origin HEAD` |
| Claude skills, hooks, memory, transcripts | bound from `~/.claude` | credentials are **not** — `claude /login` in the container |
| `/effort` | fails with `EBUSY` | `~/.claude/settings.json` is a read-only *single-file* bind. `rename()` over a mountpoint is EBUSY whatever the mode, so `:rw` would not help — the `:ro` is what stops the agent disarming its own deny list and guard hook. Use `claude --effort <level>`, or `effortLevel` in `.claude/settings.local.json` |
| `target/` | named volume | the host's is 50GB of Mach-O |
| `dx` | pinned to `DIOXUS_CLI_VERSION` in `ci.yml` | bump both, plus the literal in `windows-release.yml` |
| Client | compiles, never runs | no display for wry, no audio device, screen capture is Windows/macOS-only |
| SFU | `LIVEKIT_BUNDLE_SKIP=1`, autospawn off | unset to build a server that embeds one |
| Site | `cd site && npm run dev` on 3100, `npm run build && npm run preview` on 8787 | the only ports compose publishes to the host, loopback only; recreate the container after editing `.devcontainer/` |

## Android release APK

`ci.yml` builds ARM64 Android on PRs (unsigned artifact) and on pushes to master (signed release asset). `publish-prerelease` waits for Android and publishes eight binaries plus their eight minisign signatures. The Android certificate is separate from minisign; its public SHA-256 is `client/android/release-signing.sha256`.

| Repository secret | Value |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | Base64 of the release JKS; alias `discordia` |
| `ANDROID_KEYSTORE_PASSWORD` | JKS password |
| `ANDROID_KEY_PASSWORD` | Private-key password |

Keep the JKS and passwords backed up **outside the repository**; no private key belongs in an artifact, cache or release. Reuse that key for every update. `scripts/sign-android.py` aligns before signing and verifies the APK and committed certificate before publishing the output. Release builds validate package, debuggable flag and version metadata; versionCode follows the CI run number. DEX validation requires class definitions, including WebRTC's `livekit.org.jni_zero` bridge; a referenced class name alone does not prove R8 preserved it. `discordia.pro` retains that bridge and WebRTC methods used only through JNI.

```sh
python scripts/build-android.py --release --version-code 862 --version-name v0.1.0-pre.862 --output target/android-release/unsigned.apk
python scripts/sign-android.py --input target/android-release/unsigned.apk --output target/android-release/Discordia-android-arm64.apk --keystore /private/release.jks --build-tools "$ANDROID_HOME/build-tools/34.0.0"
```

Signing reads passwords from the two environment variables named above. SDK 34/build-tools 34.0.0, NDK 27.2.12479018, Java 21, Rust target `aarch64-linux-android` and dx pinned in CI are required; Windows setup is `scripts/android-env.ps1`.

| Variant | Launcher name | Application ID |
|---|---|---|
| Release | Discordia | `com.discordia.app` |
| Debug | Debug Discordia | `com.discordia.app.debug` |

Android release builds check GitHub using the installed versionCode. Settings → Updates downloads the ARM64 APK to private storage with streaming minisign verification (256 MiB limit). Install update waits for calls to finish and ordered session teardown, then Android asks for source permission and installation confirmation. Cancelling preserves the verified download for retry. The installer rejects a different package, certificate, or unexpected/non-newer version. Install the first updater-capable APK manually; future releases can use this flow. Physical permission/cancel/install testing is pending.

Debug and release coexist with independent data; debug never offers release installation. Older debug builds used the release application ID and cannot be updated with the release key; export the recovery phrase before replacing one. The new debug package does not migrate those older installs. Never uninstall as part of CI or packaging.

## Who can read what

| Party | Control | Voice / video |
|---|---|---|
| Network path | encrypted (QUIC, keyed to the host) | encrypted (DTLS-SRTP) |
| Rendezvous relay, when used | ciphertext only — it sees the two keys and the timing | **readable** by its SFU, unless E2EE is on |
| TLS proxy in front of a community server | **readable** by the proxy's operator | encrypted |
| Host | readable | readable |
| LAN friend | encrypted | encrypted |
| Bots | as a client: QUIC by share string, or `ws://` on loopback and `wss://` through the TLS proxy | — |

DMs are in none of these rows: they are Nostr gift wraps on relays, and a relay
learns only that somebody messaged you.

## Release profile comparison

| Command | Scope |
|---|---|
| `cargo test -p discordia-protocol --release` | Existing release baseline |
| `cargo test -p discordia-protocol --profile release-lto` | Optional thin-LTO candidate; same optimization level and panic behavior |
| `cargo build -p discordia --profile release-lto` | Full-client candidate build, not measured in this iteration |

The protocol test executable on Windows was 2,562,048 bytes with release and 2,538,496 with release-lto. Both ran all 59 tests successfully. This does not establish a client-wide speed or size improvement; release automation continues using its existing profile. [Cargo profile settings](https://doc.rust-lang.org/cargo/reference/profiles.html).
