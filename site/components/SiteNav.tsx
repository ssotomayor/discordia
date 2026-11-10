"use client";

import { usePathname } from "next/navigation";
import { useEffect, useRef, useState } from "react";
import { GITHUB } from "@/lib/site";

const LINKS = [
  { href: "/#features", label: "Features", section: "features" },
  { href: "/#self-host", label: "Hosting", section: "self-host" },
  { href: "/#faq", label: "FAQ", section: "faq" },
  { href: GITHUB, label: "GitHub", section: null },
];

export default function SiteNav() {
  const home = usePathname() === "/";
  const [open, setOpen] = useState(false);
  const [here, setHere] = useState<string | null>(null);
  const toggle = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!home) return;
    const ids = [...LINKS.map((l) => l.section), "download"].filter((s): s is string => !!s);
    const inBand = new Set<string>();
    // A thin band across the upper middle of the viewport: the section crossing it is current.
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) inBand.add(e.target.id);
          else inBand.delete(e.target.id);
        }
        setHere(ids.find((id) => inBand.has(id)) ?? null);
      },
      { rootMargin: "-40% 0px -55% 0px" },
    );
    for (const id of ids) {
      const el = document.getElementById(id);
      if (el) io.observe(el);
    }
    return () => io.disconnect();
  }, [home]);

  useEffect(() => {
    if (!open) return;
    const close = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        toggle.current?.focus();
      }
    };
    const shrink = window.matchMedia("(min-width: 760px)");
    const onWide = () => shrink.matches && setOpen(false);
    document.addEventListener("keydown", close);
    shrink.addEventListener("change", onWide);
    document.body.style.overflow = "hidden";
    panel.current?.querySelector("a")?.focus({ preventScroll: true });
    return () => {
      document.removeEventListener("keydown", close);
      shrink.removeEventListener("change", onWide);
      document.body.style.overflow = "";
    };
  }, [open]);

  const current = (section: string | null) => (section && section === here ? { "aria-current": "location" as const } : {});

  return (
    <header className="site-header">
      <nav className="nav-pill" aria-label="Primary">
        <a className="brand heading-6" href="/">
          <img src="/icon.svg" alt="" width={24} height={24} />
          Discordia
        </a>
        <ul className="nav-links title-4">
          {LINKS.map((l) => (
            <li key={l.label}>
              <a href={l.href} {...current(l.section)}>
                {l.label}
              </a>
            </li>
          ))}
        </ul>
        <a className="btn btn-primary button-sm" href="/#download" {...current("download")}>
          Download
        </a>
        <button
          ref={toggle}
          type="button"
          className="menu-toggle"
          aria-expanded={open}
          aria-controls="site-menu"
          aria-label={open ? "Close menu" : "Open menu"}
          onClick={() => setOpen((o) => !o)}
        >
          <span />
          <span />
        </button>
      </nav>

      <div id="site-menu" ref={panel} className="menu-panel" data-open={open} inert={!open}>
        <ul className="heading-4">
          {LINKS.map((l) => (
            <li key={l.label}>
              <a href={l.href} onClick={() => setOpen(false)} {...current(l.section)}>
                {l.label}
              </a>
            </li>
          ))}
        </ul>
      </div>
    </header>
  );
}
