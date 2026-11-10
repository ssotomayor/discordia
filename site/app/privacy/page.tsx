import type { Metadata } from "next";

import { GITHUB } from "@/lib/site";

export const metadata: Metadata = {
  title: "Privacy · Discordia",
  description: "What Discordia does with your data, including Google Drive backups.",
  alternates: { canonical: "/privacy/" },
};

export default function Privacy() {
  return (
    <>

      <main id="main" className="page">
        <div className="wrap prose">
          <h1 className="heading-3 lg-heading-2">Privacy</h1>
          <p className="caption-2 muted">Last updated 5 November 2026.</p>

          <p>
            Discordia is an open-source app. There is no Discordia account, no analytics and no telemetry: the
            people who make Discordia receive nothing about you from the app. Your identity is a key pair kept on
            your device.
          </p>

          <h2 className="heading-5">Servers and relays you connect to</h2>
          <p>
            A Discordia server stores what you send to it (messages, uploads, your profile) and is run by its
            operator, not by us. Direct messages travel through the Nostr relays you choose, encrypted so that the
            relays cannot read them. GIF searches go from your device to GIPHY.
          </p>

          <h2 className="heading-5">Google Drive backup and &ldquo;Sign in with Google&rdquo;</h2>
          <p>
            These are optional. When you use them, the app on your device talks to Google directly; nothing passes
            through any Discordia server.
          </p>
          <ul>
            <li>
              <b>What we ask for.</b> Only the <code>drive.appdata</code> permission: a hidden folder in your
              Google Drive that only Discordia can read or write. Discordia cannot see your other files, your
              email or your contacts.
            </li>
            <li>
              <b>What is stored.</b> One small file per identity you back up, holding your display name, public key
              and private key. If you choose &ldquo;Add extra password protection&rdquo;, the private key and
              recovery phrase are encrypted with your password (NIP-49) before they leave your device, and Google
              cannot read them. Without it, they are stored as they are, and anyone who gets into your Google
              account can use the identity.
            </li>
            <li>
              <b>What is kept on your device.</b> Nothing about your Google account. The sign-in is used for the
              one backup or restore you asked for and then discarded.
            </li>
            <li>
              <b>Deleting it.</b> In Google Drive, open Settings → Manage apps → Discordia → Options → Delete
              hidden app data. You can remove Discordia&apos;s access at{" "}
              <a href="https://myaccount.google.com/permissions">myaccount.google.com/permissions</a>.
            </li>
          </ul>
          <p>
            Discordia&apos;s use and transfer of information received from Google APIs adheres to the{" "}
            <a href="https://developers.google.com/terms/api-services-user-data-policy">
              Google API Services User Data Policy
            </a>
            , including the Limited Use requirements. That information is used only to save and restore your
            identity, and is never sold, shared or used for advertising.
          </p>

          <h2 className="heading-5">Contact</h2>
          <p>
            Questions or concerns: open an issue at <a href={`${GITHUB}/issues`}>{GITHUB.replace("https://", "")}</a>
            .
          </p>
        </div>
      </main>
    </>
  );
}
