import {
  ArrowRight,
  CheckCircle,
  DownloadSimple,
  EnvelopeSimple,
  Export,
  Hash,
  Package,
  Robot,
  Screencast,
  SpeakerHigh,
  SquaresFour,
  Waveform,
  WifiSlash,
} from "@phosphor-icons/react/ssr";
import type { Icon } from "@phosphor-icons/react";
import ChatPeek from "@/components/ChatPeek";
import Downloads from "@/components/Downloads";
import GlobeLoader from "@/components/GlobeLoader";
import LatestRelease from "@/components/LatestRelease";
import Spotlight from "@/components/Spotlight";
import Sprite from "@/components/Sprite";
import Tagline from "@/components/Tagline";
import { GITHUB } from "@/lib/site";

type Size = "big" | "wide" | undefined;

const FEATURES: { title: string; body: string; icon: Icon; size?: Size; art?: "channels" | "cipher" | "ways" }[] = [
  {
    title: "Text and voice channels",
    body: "Guilds with roles, moderation, custom emoji, a soundboard, and levels each guild defines for itself.",
    icon: Hash,
    size: "big",
    art: "channels",
  },
  {
    title: "Screen share with the sound",
    body: "Share a window or a whole screen with its system audio, turn on a camera, or pop a stream out into its own window.",
    icon: Screencast,
  },
  {
    title: "DMs no server can read",
    body: "Direct messages are encrypted Nostr gift wraps (NIP‑17, NIP‑44) carried by relays. They never touch the gateway and follow you into any Nostr client.",
    icon: EnvelopeSimple,
    size: "big",
    art: "cipher",
  },
  {
    title: "Encrypted calls",
    body: "Voice and screen share are encrypted end to end between the people in the call. The media relay only ever forwards ciphertext.",
    icon: Waveform,
  },
  {
    title: "Bots",
    body: "A Rust bot SDK speaks the same wire protocol as the app. The server’s own integration tests drive it.",
    icon: Robot,
  },
  {
    title: "A room you rearrange",
    body: "Panels drag, resize and snap to presets like furniture. Your layout is yours and comes back next time.",
    icon: SquaresFour,
  },
  {
    title: "Take your guild with you",
    body: "Export writes a guild out with its members’ keys intact. Import brings it up under another operator with the same people in it.",
    icon: Export,
  },
  {
    title: "Works offline, at home",
    body: "Home and your DMs need no server at all. A guild appears when you connect to one.",
    icon: WifiSlash,
  },
  {
    title: "One binary, three ways",
    body: "The desktop app embeds the server. The same server ships alone for a box of your own, and as a Docker image.",
    icon: Package,
    size: "wide",
    art: "ways",
  },
];

const FAQ: { q: string; a: string }[] = [
  {
    q: "Do I need an email or a phone number?",
    a: "No. Your account is a Nostr keypair made on your device. There is no Discordia account to sign up for and nothing to verify.",
  },
  {
    q: "Who can read my messages?",
    a: "Guild channels are stored by whoever runs that server, so its operator can read them. Direct messages are encrypted for the people in them and travel through Nostr relays, never through a server. Voice and screen share are encrypted end to end.",
  },
  {
    q: "What if I lose my key?",
    a: "Restore it from the twelve word recovery phrase you saved when you made it. You can also back it up to a hidden folder in your Google Drive, with an extra password if you like. Nobody else holds a copy, so nobody else can restore it for you.",
  },
  {
    q: "What happens if the server I use shuts down?",
    a: "Your identity and your DMs are unaffected, because neither lives on the server. The guild’s channels do, which is why export and import exist: an operator can move a guild to someone else with every member’s key intact.",
  },
  {
    q: "Do I need to open ports to host a server?",
    a: "No. The rendezvous at discordia.world introduces your friends to your machine without showing them your address, punches through your NAT where it can, and relays encrypted traffic where it cannot.",
  },
  {
    q: "Which platforms does it run on?",
    a: "The app ships for macOS on Apple silicon, Windows and Android. Linux desktop builds are paused for now. The standalone server runs on Linux, macOS and Windows, and as a Docker image.",
  },
  {
    q: "Does the app collect data about me?",
    a: "No. There is no analytics and no telemetry. The only outside services it talks to are ones you choose: GIF search, and the optional Google Drive backup.",
  },
  {
    q: "Can I use the Nostr key I already have?",
    a: "Yes. Import it as an nsec and the same identity works on every Discordia server and in any other Nostr client.",
  },
  {
    q: "Is Discordia finished?",
    a: "Not yet. Every change to the main branch publishes a new prerelease, and the open issues on GitHub are the honest list of what is missing.",
  },
];

const FAQ_SCHEMA = JSON.stringify({
  "@context": "https://schema.org",
  "@type": "FAQPage",
  mainEntity: FAQ.map(({ q, a }) => ({ "@type": "Question", name: q, acceptedAnswer: { "@type": "Answer", text: a } })),
}).replace(/</g, "\\u003c");

const RANK = { big: 0, undefined: 1, wide: 2 } as const;
const TILES = [...FEATURES].sort((a, b) => RANK[a.size ?? "undefined"] - RANK[b.size ?? "undefined"]);

function Art({ kind }: { kind: "channels" | "cipher" | "ways" }) {
  if (kind === "channels")
    return (
      <ul className="chan tile-art caption-2" aria-hidden="true">
        <li>
          <Hash size={14} weight="bold" />
          announcements
        </li>
        <li data-on>
          <Hash size={14} weight="bold" />
          general
          <span className="mono">3</span>
        </li>
        <li>
          <Hash size={14} weight="bold" />
          clips
        </li>
        <li>
          <SpeakerHigh size={14} weight="fill" />
          Lounge
        </li>
      </ul>
    );
  if (kind === "cipher")
    return (
      <p className="cipher tile-art" aria-hidden="true">
        <b>kind 1059</b> AqH3nVZ0x9Lw2kPq7JdR4mS8tYbE1cFgU6oXiN5vKzB3hQeWlT0aMpC9rDsGjY2uO7nVf4kL8ZwHxEiQ1bR6tSy0cJ3mPq9gA5dUoXnK2vWl7hF4sB8eZiTr1jCyN6
      </p>
    );
  return (
    <ul className="ways" aria-hidden="true">
      <li>
        <code className="caption-2">Discordia app</code>
        <span className="caption-2">server inside</span>
      </li>
      <li>
        <code className="caption-2">discordia-server</code>
        <span className="caption-2">one binary</span>
      </li>
      <li>
        <code className="caption-2">ghcr.io/…/discordia-server</code>
        <span className="caption-2">Docker</span>
      </li>
    </ul>
  );
}

function Eyebrow({ n, children }: { n: string; children: string }) {
  return (
    <p className="eyebrow caption-3">
      {n} / {children}
    </p>
  );
}

export default function Page() {
  return (
    <main id="main">
      <section className="hero">
        <div className="hero-bg" aria-hidden="true" />
        <div className="wrap hero-grid">
          <div className="hero-copy">
            <a className="chip caption-2" href="#download">
              <span className="live-dot" aria-hidden="true" />
              <LatestRelease />
            </a>
            <h1 className="hero-title heading-2 lg-display-1">
              Chat you
              <br />
              <span className="accent-text">actually own.</span>
            </h1>
            <p className="hero-lede body-7 lg-body-8">
              Discordia is a Discord alternative you host yourself, and your keypair is your account. Text and voice,
              screen sharing, bots, roles, and DMs no server can read. Run it from your own machine and friends join
              with a code.
            </p>
            <div className="hero-actions">
              <a className="btn btn-primary btn-lg button-md" href="#download">
                <DownloadSimple size={20} weight="bold" aria-hidden="true" />
                Download Discordia
              </a>
              <a className="link-arrow button-md" href="#self-host">
                Or host a server for friends
                <ArrowRight size={18} weight="bold" aria-hidden="true" />
              </a>
            </div>
            <ul className="proof caption-2">
              <li>
                <CheckCircle size={16} weight="fill" aria-hidden="true" />
                Free, no account to sign up for
              </li>
              <li>
                <CheckCircle size={16} weight="fill" aria-hidden="true" />
                Signed builds from public CI
              </li>
              <li>
                <CheckCircle size={16} weight="fill" aria-hidden="true" />
                No analytics, no telemetry
              </li>
            </ul>
          </div>
          <div className="hero-visual">
            <GlobeLoader />
            <ChatPeek />
          </div>
        </div>
      </section>

      <section className="section" id="identity">
        <div className="wrap two-col" data-reveal>
          <div className="section-head flush">
            <Eyebrow n="01">Identity</Eyebrow>
            <h2 className="heading-4 lg-heading-2">
              Your keys are <span className="accent-text">your account.</span>
            </h2>
            <p className="body-7">
              No email, no phone number, no company in the middle. Your identity is a Nostr keypair you hold. Create
              one and save a twelve word recovery phrase, or import the key you already use in another Nostr app. The
              same identity works on every Discordia server and in any Nostr client, and no operator can take it from
              you.
            </p>
          </div>
          <div className="idcard" aria-label="Example account card">
            <div className="idcard-avatar heading-6" aria-hidden="true">
              A
            </div>
            <div className="idcard-body">
              <div className="idcard-name title-2">
                Ada <span className="idcard-tag caption-2">#7f2c</span>
              </div>
              <div className="idcard-npub caption-2">npub1qy3m8kx…v7f2c</div>
              <div className="sig" aria-hidden="true">
                {Array.from({ length: 12 }, (_, i) => (
                  <span key={i} />
                ))}
              </div>
              <p className="idcard-note caption-2">
                This colour signature is derived from the public key. Nobody else has it.
              </p>
            </div>
          </div>
        </div>
      </section>

      <section className="section" id="yours">
        <div className="wrap" data-reveal>
          <div className="section-head">
            <Eyebrow n="02">Ownership</Eyebrow>
            <h2 className="heading-4 lg-heading-2">What is actually yours</h2>
            <p className="body-7">
              Hosting it yourself is not the same as decentralised, and Discordia is deliberate about that. The
              decentralisation is portable identity plus independent servers. A community&apos;s data lives with its
              operator, on purpose.
            </p>
          </div>
          <table className="own">
            <tbody>
              <tr>
                <th scope="row" className="heading-6">
                  Identity
                </th>
                <td className="who body-5">You</td>
                <td className="how">A keypair. Importable as an nsec, revocable by nobody.</td>
              </tr>
              <tr>
                <th scope="row" className="heading-6">
                  Direct messages
                </th>
                <td className="who body-5">Your key</td>
                <td className="how">
                  Encrypted on Nostr relays, never through a gateway. They follow you to another server or to any Nostr
                  client.
                </td>
              </tr>
              <tr>
                <th scope="row" className="heading-6">
                  Guild data
                </th>
                <td className="who body-5">Whoever runs the server</td>
                <td className="how">Export and import move it between operators with everyone&apos;s keys intact.</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section className="section" id="features">
        <div className="wrap" data-reveal>
          <div className="section-head">
            <Eyebrow n="03">Features</Eyebrow>
            <h2 className="heading-4 lg-heading-2">Everything a server needs, in a native app.</h2>
            <p className="body-7">
              Written in Rust, not a web page in a frame. Voice, video and the mixer run natively, and nothing in it
              phones home.
            </p>
          </div>
          <div className="bento-wrap">
            <Sprite name="botto" className="blinks bob" />
            <ul className="bento" id="bento">
              {TILES.map(({ title, body, icon: Mark, size, art }) => (
                <li key={title} className={size ? `tile tile-${size}` : "tile"}>
                  <div>
                    <span className="tile-icon">
                      <Mark size={24} aria-hidden="true" />
                    </span>
                    <h3 className="heading-6">{title}</h3>
                    <p>{body}</p>
                  </div>
                  {art ? <Art kind={art} /> : null}
                </li>
              ))}
            </ul>
          </div>
          <Spotlight target="#bento" />
        </div>
      </section>

      <Tagline>
        <Sprite name="pip" />
      </Tagline>

      <section className="section" id="self-host">
        <div className="wrap" data-reveal>
          <div className="section-head">
            <Eyebrow n="04">Hosting</Eyebrow>
            <h2 className="heading-4 lg-heading-2">Run it from the app.</h2>
            <p className="body-7">
              Hosting is a button, not a weekend. The desktop app starts a gateway and a bundled voice server inside
              itself, and the rendezvous at discordia.world introduces your friends to it without showing them your
              address.
            </p>
          </div>
          <ol className="steps">
            <li>
              <h3 className="heading-6">Create a server</h3>
              <p>Open Discordia, choose Create a server, and give it a name. Pick a spot on the globe or leave it off.</p>
            </li>
            <li>
              <h3 className="heading-6">Launch</h3>
              <p>
                The gateway and the voice server start inside the app. Your data lands in your own config directory,
                and you are the operator.
              </p>
            </li>
            <li>
              <h3 className="heading-6">Share the code</h3>
              <p>
                Friends type it into their app. The connection is QUIC keyed to your host, punched through your NAT or
                carried by the relay as ciphertext when it cannot be.
              </p>
            </li>
          </ol>
          <div className="share">
            <p className="caption-2">What a share string looks like, with the host&apos;s key in it:</p>
            <div className="perch">
              <Sprite name="squish" />
              <code className="codeblock">
                <b>quic://</b>c0ffee…9a2b<b>@</b>203.0.113.7:9001,192.168.1.20:9001
              </code>
            </div>
            <p className="caption-2">
              Prefer a box of your own? The same server ships as a standalone binary and a Docker image. The{" "}
              <a href={`${GITHUB}/blob/master/docs/OPS.md`}>ops guide</a> covers public hosts, reachability and TLS.
            </p>
          </div>
        </div>
      </section>

      <section className="section" id="faq">
        <div className="wrap faq-wrap" data-reveal>
          <Sprite name="wisp" className="drift" />
          <div className="section-head">
            <Eyebrow n="05">Questions</Eyebrow>
            <h2 className="heading-4 lg-heading-2">Questions people ask first</h2>
          </div>
          <dl className="faq">
            {FAQ.map(({ q, a }) => (
              <div key={q}>
                <dt className="heading-6">{q}</dt>
                <dd>{a}</dd>
              </div>
            ))}
          </dl>
          <script type="application/ld+json" dangerouslySetInnerHTML={{ __html: FAQ_SCHEMA }} />
        </div>
      </section>

      <section className="section" id="download">
        <div className="wrap" data-reveal>
          <div className="cta">
            <div className="party">
              <Sprite name="keykin" className="blinks hop" />
              <Sprite name="squish" />
              <Sprite name="botto" className="blinks bob" />
            </div>
            <div className="section-head">
              <Eyebrow n="06">Download</Eyebrow>
              <h2 className="heading-4 lg-heading-1">
                Get Discordia <span className="accent-text">tonight.</span>
              </h2>
              <p className="body-7">
                Every build comes from a public GitHub Actions run and is signed. Nothing in the app reports back to
                anyone.
              </p>
            </div>
            <Downloads />
          </div>
          <div className="verify">
            <h3 className="heading-6">Verify a download</h3>
            <p>
              Releases are signed with minisign. The public key lives in the repository as{" "}
              <code>release-signing.pub</code>, and every file has a <code>.minisig</code> beside it.
            </p>
            <pre className="codeblock">
              <b>minisign -Vm</b> Discordia-macos-arm64.dmg <b>-P</b>{" "}
              RWRC4u05WSE3oLs4o2ZKddADDC0QrOba58PqVBVYxog366q5Hk3wrTdM
            </pre>
          </div>
        </div>
      </section>
    </main>
  );
}
