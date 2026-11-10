"use client";

import { useEffect } from "react";

// One listener for the whole grid: each tile's light follows the pointer through --mx/--my.
export default function Spotlight({ target }: { target: string }) {
  useEffect(() => {
    const grid = document.querySelector<HTMLElement>(target);
    if (!grid || !window.matchMedia("(hover: hover)").matches) return;
    const move = (e: PointerEvent) => {
      const tile = (e.target as HTMLElement).closest<HTMLElement>(".tile");
      if (!tile) return;
      const r = tile.getBoundingClientRect();
      tile.style.setProperty("--mx", `${e.clientX - r.left}px`);
      tile.style.setProperty("--my", `${e.clientY - r.top}px`);
    };
    grid.addEventListener("pointermove", move);
    return () => grid.removeEventListener("pointermove", move);
  }, [target]);
  return null;
}
