"use client";

import type { ReactNode } from "react";
import { useEffect, useRef, useState } from "react";

const LINES = ["A key only you hold.", "A server you run.", "Friends join with a code."];

export default function Tagline({ children }: { children?: ReactNode }) {
  const root = useRef<HTMLElement>(null);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    const section = root.current;
    if (!section || window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const words = [...section.querySelectorAll<HTMLElement>(".word")];
    let frame = 0;
    const update = () => {
      frame = 0;
      const line = window.innerHeight * 0.6;
      for (const w of words) w.classList.toggle("on", w.getBoundingClientRect().top < line);
    };
    const schedule = () => {
      frame ||= requestAnimationFrame(update);
    };
    update();
    setReady(true);
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, []);

  return (
    <section ref={root} className="section tagline" aria-label="In short" data-ready={ready || undefined}>
      {children}
      <div className="wrap">
        <p className="tagline-text heading-3 lg-heading-1">
          {LINES.map((line) => (
            <span key={line} className="line">
              {line.split(" ").map((word, i) => (
                <span key={i}>
                  <span className="word">{word}</span>{" "}
                </span>
              ))}
            </span>
          ))}
        </p>
      </div>
    </section>
  );
}
