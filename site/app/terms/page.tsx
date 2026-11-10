import type { Metadata } from "next";

import { GITHUB } from "@/lib/site";

export const metadata: Metadata = {
  title: "Terms · Discordia",
  description: "The terms for using the Discordia app and this website.",
  alternates: { canonical: "/terms/" },
};

export default function Terms() {
  return (
    <>

      <main id="main" className="page">
        <div className="wrap prose">
          <h1 className="heading-3 lg-heading-2">Terms</h1>
          <p className="caption-2 muted">Last updated 5 November 2026.</p>

          <p>
            Discordia is free software with public source. Using the app or this website means you accept these terms.
          </p>

          <h2 className="heading-5">The software</h2>
          <p>
            The app&apos;s source is public in its <a href={GITHUB}>repository</a>. It is provided as is,
            without warranty of any kind, and the people
            who make it are not liable for any loss arising from its use, to the extent the law allows.
          </p>

          <h2 className="heading-5">Your identity is yours to keep</h2>
          <p>
            Your account is a key pair held on your device. Nobody else can recover it for you: keep your recovery
            phrase, or a backup, somewhere safe. A Google Drive backup without a password can be used by anyone who
            gets into that Google account.
          </p>

          <h2 className="heading-5">Servers are run by their operators</h2>
          <p>
            Each Discordia server is run by whoever hosts it, who sets its rules and holds its data. We do not run,
            moderate or control servers other than our own, and are not responsible for what happens on them.
          </p>

          <h2 className="heading-5">Acceptable use</h2>
          <p>
            Do not use Discordia, or any server or relay it connects to, to break the law, to harm others, or to
            attack or overload the services it relies on.
          </p>

          <h2 className="heading-5">Third-party services</h2>
          <p>
            Some features use services we do not operate, such as Google Drive, GIPHY and Nostr relays. Their own
            terms apply when you use them. See the <a href="/privacy/">privacy page</a> for what is shared with
            them.
          </p>

          <h2 className="heading-5">Changes and contact</h2>
          <p>
            These terms may change; the date above says when. Questions: open an issue at{" "}
            <a href={`${GITHUB}/issues`}>{GITHUB.replace("https://", "")}</a>.
          </p>
        </div>
      </main>
    </>
  );
}
