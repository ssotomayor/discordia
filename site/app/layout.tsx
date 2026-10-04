import type { Metadata, Viewport } from "next";
import localFont from "next/font/local";
import type { ReactNode } from "react";
import "./globals.css";

const display = localFont({
  src: "./fonts/bricolage.woff2",
  variable: "--font-display",
  weight: "400 800",
  display: "swap",
});
const body = localFont({
  src: "./fonts/spacegrotesk.woff2",
  variable: "--font-body",
  weight: "300 700",
  display: "swap",
});
const mono = localFont({
  src: "./fonts/jetbrainsmono.woff2",
  variable: "--font-mono",
  weight: "400 700",
  display: "swap",
});

const description =
  "A self-hostable Discord alternative where your keypair is your account. Text and voice, screen sharing, bots, roles, and DMs no server can read.";

export const metadata: Metadata = {
  metadataBase: new URL("https://discordia.world"),
  title: "Discordia",
  description,
  openGraph: {
    title: "Discordia",
    description,
    url: "https://discordia.world",
    siteName: "Discordia",
    type: "website",
    images: [{ url: "/og.png", width: 1200, height: 630, alt: "Discordia" }],
  },
  twitter: { card: "summary_large_image", title: "Discordia", description, images: ["/og.png"] },
  icons: { icon: "/icon.svg", apple: "/apple-touch-icon.png" },
};

export const viewport: Viewport = {
  themeColor: "#121110",
  colorScheme: "dark",
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={`${display.variable} ${body.variable} ${mono.variable}`}>
      <body>{children}</body>
    </html>
  );
}
