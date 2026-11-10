"use client";

import { usePathname } from "next/navigation";
import { GITHUB, RELEASES } from "@/lib/site";

const LINKS = [
  { href: GITHUB, label: "Source" },
  { href: RELEASES, label: "Releases" },
  { href: `${GITHUB}/issues`, label: "Issues" },
  { href: `${GITHUB}/blob/master/docs/OPS.md`, label: "Ops guide" },
  { href: "/privacy/", label: "Privacy" },
  { href: "/terms/", label: "Terms" },
];

export default function SiteFooter() {
  const path = usePathname();
  return (
    <footer className="wrap footer caption-2">
      <span>Discordia. A keypair is an account.</span>
      <nav className="footer-links" aria-label="Footer">
        {LINKS.map((l) => (
          <a key={l.label} href={l.href} {...(path === l.href ? { "aria-current": "page" as const } : {})}>
            {l.label}
          </a>
        ))}
      </nav>
    </footer>
  );
}
