import { Hash, SpeakerHigh } from "@phosphor-icons/react/ssr";
import Sprite from "./Sprite";

// Sample people and lines for the mock; nothing here is a real account.
const MESSAGES = [
  { name: "Ada", initial: "A", sig: ["c1", "c4", "c9"], text: "voice in five? I'll share the map" },
  { name: "Kenji", initial: "K", sig: ["c6", "c2", "c11"], text: "on my way, bringing the soundboard" },
  { name: "Marisol", initial: "M", sig: ["c3", "c7", "c10"], text: "same key as on Sam's server, neat" },
];

export default function ChatPeek() {
  return (
    <div className="peek" aria-hidden="true">
      <Sprite name="keykin" className="blinks hop" />
      <div className="peek-head caption-2">
        <Hash size={16} weight="bold" />
        <b>general</b>
        <span>· Lantern Club</span>
      </div>
      {MESSAGES.map((m) => (
        <div className="peek-msg" key={m.name}>
          <span className="peek-avatar">
            {m.initial}
          </span>
          <div>
            <div className="peek-name title-4">
              {m.name}
              <span className="peek-sig">
                {m.sig.map((c) => (
                  <i key={c} className={c} />
                ))}
              </span>
            </div>
            <div className="peek-text body-6">{m.text}</div>
          </div>
        </div>
      ))}
      <div className="peek-voice body-5">
        <SpeakerHigh size={16} weight="fill" />
        <span>Lounge · 3 in voice</span>
        <span>
          <i />
          <i />
          <i />
          <i />
        </span>
      </div>
    </div>
  );
}
