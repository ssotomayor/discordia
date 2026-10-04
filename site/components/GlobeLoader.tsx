"use client";

import dynamic from "next/dynamic";

// three.js stays out of the first chunk; the skeleton holds the hero's layout meanwhile.
const Globe = dynamic(() => import("./Globe"), {
  ssr: false,
  loading: () => (
    <div className="globe" aria-hidden="true">
      <div className="globe-skeleton" />
    </div>
  ),
});

export default function GlobeLoader() {
  return <Globe />;
}
