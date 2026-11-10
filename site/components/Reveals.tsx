"use client";

import { usePathname } from "next/navigation";
import { useEffect } from "react";

export default function Reveals() {
  const path = usePathname();

  useEffect(() => {
    const items = [...document.querySelectorAll<HTMLElement>("[data-reveal]")];
    // Whatever is already on screen stays put; hiding it now would flash it out and back in.
    for (const el of items) {
      if (el.getBoundingClientRect().top < window.innerHeight) el.classList.add("is-in");
    }
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (!e.isIntersecting) continue;
          e.target.classList.add("is-in");
          io.unobserve(e.target);
        }
      },
      { rootMargin: "0px 0px -10% 0px" },
    );
    items.filter((el) => !el.classList.contains("is-in")).forEach((el) => io.observe(el));
    document.documentElement.setAttribute("data-reveal-ready", "");
    return () => io.disconnect();
  }, [path]);

  return null;
}
