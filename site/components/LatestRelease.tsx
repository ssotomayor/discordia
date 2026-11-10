"use client";

import { useEffect, useState } from "react";
import { latestRelease, releaseDate, type Release } from "@/lib/release";

export default function LatestRelease() {
  const [release, setRelease] = useState<Release | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let live = true;
    latestRelease().then(
      (r) => live && setRelease(r),
      () => live && setFailed(true),
    );
    return () => {
      live = false;
    };
  }, []);

  if (failed) return <>Builds for macOS, Windows and Android</>;
  if (!release) return <span className="skeleton" role="status" aria-label="Looking up the latest release" />;
  return (
    <>
      New build <span className="mono">{release.tag_name}</span> · {releaseDate(release)}
    </>
  );
}
