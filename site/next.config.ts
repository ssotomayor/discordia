import type { NextConfig } from "next";

// Static export: the VPS serves `out/` from nginx and never runs Node (docs/OPS.md).
const config: NextConfig = {
  output: "export",
  images: { unoptimized: true },
  reactStrictMode: true,
  // Next 16 dev otherwise writes an AGENTS.md and CLAUDE.md here; the repo root already has the rules.
  agentRules: false,
  // SITE_PREVIEW=1 makes asset URLs relative so `out/` also runs hosted under a sub-path.
  assetPrefix: process.env.SITE_PREVIEW ? "." : undefined,
};

export default config;
