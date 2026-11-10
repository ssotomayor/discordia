import { ArrowRight } from "@phosphor-icons/react/ssr";
import type { Metadata } from "next";
import Sprite from "@/components/Sprite";

export const metadata: Metadata = {
  title: "Not found · Discordia",
  robots: { index: false },
};

export default function NotFound() {
  return (
    <main id="main" className="page">
      <div className="wrap lost">
        <Sprite name="wisp" className="drift" />
        <p className="lost-code accent-text" aria-hidden="true">404</p>
        <h1 className="heading-3 lg-heading-2">This page is not here.</h1>
        <p className="body-7 muted">The link may be old, or the address mistyped. Everything on the site starts from the home page.</p>
        <div className="hero-actions">
          <a className="btn btn-primary button-md" href="/">
            Back to the home page
          </a>
          <a className="link-arrow button-md" href="/#download">
            Download Discordia
            <ArrowRight size={18} weight="bold" aria-hidden="true" />
          </a>
        </div>
      </div>
    </main>
  );
}
