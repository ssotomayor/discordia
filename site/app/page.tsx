import Downloads from "@/components/Downloads";
import GlobeLoader from "@/components/GlobeLoader";

const GITHUB = "https://github.com/ssotomayor/discordia";

// The colour signature a key paints under a name in the app; these twelve are illustrative.
const SIGNATURE = [
  "#5a6cff", "#73c86a", "#e8d04a", "#e89a4a", "#6fbf6a", "#d070d0",
  "#5a8cff", "#8fd86a", "#5ad0c8", "#e070b8", "#6a7cff", "#d8b070",
];

function Mark({ d }: { d: string }) {
  return (
    <svg className="mark" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d={d} />
    </svg>
  );
}

const FEATURES: { title: string; body: string; icon: string }[] = [
  {
    title: "Text and voice channels",
    body: "Guilds with roles, moderation, custom emoji, a soundboard, and levels the guild defines for itself.",
    icon: "M4 5h16v10H9l-5 4V5Zm4 4h8M8 12h5",
  },
  {
    title: "Screen share with the sound",
    body: "Share a window or a whole screen with its system audio, turn on a camera, or pop a stream out into its own window.",
    icon: "M3 5h18v11H3V5Zm5 14h8M12 16v3M17 8l2 2-2 2",
  },
  {
    title: "DMs no server can read",
    body: "Direct messages are NIP-17 gift wraps, NIP-44 encrypted, carried by Nostr relays. They never touch the gateway and follow you into any Nostr client.",
    icon: "M4 8l8 5 8-5M4 8v9h16V8M4 8l8-4 8 4",
  },
  {
    title: "Encrypted calls",
    body: "Voice and screen share are encrypted end to end between the people in the call. The media relay forwards ciphertext.",
    icon: "M12 3a4 4 0 0 1 4 4v3H8V7a4 4 0 0 1 4-4Zm-6 7h12v10H6V10Zm6 4v3",
  },
  {
    title: "Bots",
    body: "A Rust bot SDK against the same wire protocol the client speaks. The server's own integration tests drive it.",
    icon: "M8 7h8a3 3 0 0 1 3 3v6a3 3 0 0 1-3 3H8a3 3 0 0 1-3-3v-6a3 3 0 0 1 3-3Zm4-4v4M9 13h.01M15 13h.01M10 16h4",
  },
  {
    title: "A room you rearrange",
    body: "Panels drag, resize and snap to presets like furniture. Your layout is yours and comes back next time.",
    icon: "M4 4h7v7H4V4Zm9 0h7v4h-7V4Zm0 6h7v10h-7V10ZM4 13h7v7H4v-7Z",
  },
  {
    title: "Take your guild with you",
    body: "Export writes a guild out with its members' keys intact. Import brings it up under another operator with the same people in it.",
    icon: "M12 3v12m0 0 4-4m-4 4-4-4M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2",
  },
  {
    title: "Works offline, at home",
    body: "Home and your DMs need no server at all. A guild appears when you connect to one.",
    icon: "M3 11l9-7 9 7v9a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1v-9Z",
  },
  {
    title: "One binary, three ways",
    body: "The desktop app embeds the server. The same server ships alone for a box, and as a Docker image.",
    icon: "M4 6a8 3 0 0 1 16 0v12a8 3 0 0 1-16 0V6Zm0 0a8 3 0 0 0 16 0M4 12a8 3 0 0 0 16 0",
  },
];

export default function Page() {
  return (
    <>
      <header className="hero">
        <div className="hero-bg" aria-hidden="true" />
        <div className="wrap">
          <nav className="nav" aria-label="Primary">
            <a className="brand" href="/">
              <img src="icon.svg" alt="" width={28} height={28} />
              Discordia
            </a>
            <div className="nav-links">
              <a href="#features">Features</a>
              <a href="#self-host">Self-host</a>
              <a href={GITHUB}>GitHub</a>
              <a className="btn btn-ghost" href="#download">
                Download
              </a>
            </div>
          </nav>

          <div className="hero-grid">
            <div className="hero-copy">
              <h1>Chat you actually own.</h1>
              <p className="lede">
                Discordia is a self-hostable Discord alternative where your keypair is your account. Text and
                voice, screen sharing, bots, roles, and DMs no server can read. Run it from your own machine
                and friends join with a code.
              </p>
              <div className="hero-actions">
                <a className="btn btn-primary" href="#download">
                  Download Discordia
                </a>
                <a className="btn btn-ghost" href="#self-host">
                  Run your own server
                </a>
              </div>
              <p className="hero-note">
                Pre-release, for macOS, Windows and Linux. The{" "}
                <a href={`${GITHUB}/issues`}>open issues</a> are the honest list.
              </p>
            </div>
            <GlobeLoader />
          </div>
        </div>
      </header>

      <main>
        <section className="section" id="identity">
          <div className="wrap two-col">
            <div className="section-head" style={{ marginBottom: 0 }}>
              <h2>Your keys are your account.</h2>
              <p className="lede">
                No email, no phone number, no company in the middle. Your identity is a Nostr keypair you hold.
                Create one and save a twelve-word recovery phrase, or import the key you already use in another
                Nostr app. The same identity works on every Discordia server and in any Nostr client, and no
                operator can take it from you.
              </p>
            </div>
            <div className="idcard" aria-label="Example account card">
              <div className="idcard-avatar" aria-hidden="true">
                A
              </div>
              <div className="idcard-body">
                <div className="idcard-name">
                  Ada <span className="idcard-tag">#7f2c</span>
                </div>
                <div className="idcard-npub">npub1qy3m8kx…v7f2c</div>
                <div className="sig" aria-hidden="true">
                  {SIGNATURE.map((c, i) => (
                    <span key={i} style={{ backgroundColor: c }} />
                  ))}
                </div>
                <p className="idcard-note">This colour signature is derived from the public key. Nobody else has it.</p>
              </div>
            </div>
          </div>
        </section>

        <section className="section" id="yours">
          <div className="wrap">
            <div className="section-head">
              <h2>What is actually yours</h2>
              <p className="lede">
                Self-hosted is not decentralised, and Discordia is deliberate about that. The decentralisation is
                portable identity plus independent servers. A community&apos;s data lives with its operator, on
                purpose.
              </p>
            </div>
            <table className="own">
              <tbody>
                <tr>
                  <th scope="row">Identity</th>
                  <td className="who">You</td>
                  <td className="how">A keypair. Importable as an nsec, revocable by nobody.</td>
                </tr>
                <tr>
                  <th scope="row">Direct messages</th>
                  <td className="who">Your key</td>
                  <td className="how">
                    Encrypted on Nostr relays, never through a gateway. They follow you to another server or to
                    any Nostr client.
                  </td>
                </tr>
                <tr>
                  <th scope="row">Guild data</th>
                  <td className="who">Whoever runs the server</td>
                  <td className="how">Export and import move it between operators with everyone&apos;s keys intact.</td>
                </tr>
              </tbody>
            </table>
          </div>
        </section>

        <section className="section" id="features">
          <div className="wrap">
            <div className="section-head">
              <h2>Everything a server needs, in a native app.</h2>
              <p className="lede">
                A Rust desktop client, not a web page in a frame. Voice, video and the mixer run natively, and
                nothing in it phones home.
              </p>
            </div>
            <ul className="features">
              {FEATURES.map((f) => (
                <li key={f.title}>
                  <Mark d={f.icon} />
                  <h3>{f.title}</h3>
                  <p>{f.body}</p>
                </li>
              ))}
            </ul>
          </div>
        </section>

        <section className="section" id="self-host">
          <div className="wrap">
            <div className="section-head">
              <h2>Run it from the app.</h2>
              <p className="lede">
                Hosting is a button, not a weekend. The desktop app starts a gateway and a bundled voice server
                in-process, and the rendezvous at discordia.world introduces your friends to it without showing
                them your address.
              </p>
            </div>
            <ol className="steps">
              <li>
                <h3>Create a server</h3>
                <p>Open Discordia, choose Create a server, and give it a name. Pick a spot on the globe or leave it off.</p>
              </li>
              <li>
                <h3>Launch</h3>
                <p>
                  The gateway and the voice server start inside the app. Your data lands in your own config
                  directory, and you are the operator.
                </p>
              </li>
              <li>
                <h3>Share the code</h3>
                <p>
                  Friends type it into their app. The connection is QUIC keyed to your host, punched through
                  your NAT or carried by the relay as ciphertext when it cannot be.
                </p>
              </li>
            </ol>
            <div className="share">
              <p className="muted">What a share string looks like, with the host&apos;s key in it:</p>
              <code>
                <b>quic://</b>c0ffee…9a2b<b>@</b>203.0.113.7:9001,192.168.1.20:9001
              </code>
              <p className="dim" style={{ fontSize: "0.95rem" }}>
                Prefer a box of your own? The same server ships as a standalone binary and a Docker image. The{" "}
                <a href={`${GITHUB}/blob/master/docs/OPS.md`}>ops guide</a> covers public hosts, reachability and
                TLS.
              </p>
            </div>
          </div>
        </section>

        <section className="section" id="download">
          <div className="wrap">
            <div className="section-head">
              <h2>Download</h2>
              <p className="lede">
                Every build comes from a public GitHub Actions run and is signed. Nothing in the app reports back
                to anyone.
              </p>
            </div>
            <Downloads />
            <div className="verify">
              <h3>Verify a download</h3>
              <p className="muted">
                Releases are signed with minisign. The public key lives in the repository as{" "}
                <code>release-signing.pub</code>, and every file has a <code>.minisig</code> beside it.
              </p>
              <pre>
                <b>minisign -Vm</b> Discordia-macos-arm64.dmg <b>-P</b>{" "}
                RWRC4u05WSE3oLs4o2ZKddADDC0QrOba58PqVBVYxog366q5Hk3wrTdM
              </pre>
            </div>
          </div>
        </section>
      </main>

      <footer className="wrap footer">
        <span>Discordia. A keypair is an account.</span>
        <nav className="footer-links" aria-label="Footer">
          <a href={GITHUB}>Source</a>
          <a href={`${GITHUB}/releases`}>Releases</a>
          <a href={`${GITHUB}/issues`}>Issues</a>
          <a href={`${GITHUB}/blob/master/docs/OPS.md`}>Ops guide</a>
        </nav>
      </footer>
    </>
  );
}
