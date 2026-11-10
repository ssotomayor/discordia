import { PALETTE, SPRITES, type SpriteName } from "@/lib/sprites";

type Run = { x: number; y: number; w: number; fill: string };

// Rows merge into horizontal runs so a 16x16 frame is a few dozen rects, not 256.
function runs(frame: string[]): Run[] {
  const out: Run[] = [];
  frame.forEach((row, y) => {
    let x = 0;
    while (x < row.length) {
      const c = row[x];
      let end = x + 1;
      while (end < row.length && row[end] === c) end++;
      if (c !== ".") out.push({ x, y, w: end - x, fill: PALETTE[c] });
      x = end;
    }
  });
  return out;
}

const CACHE = new Map<SpriteName, Run[][]>();

export default function Sprite({ name, className }: { name: SpriteName; className?: string }) {
  const frames = SPRITES[name];
  let drawn = CACHE.get(name);
  if (!drawn) CACHE.set(name, (drawn = frames.map(runs)));
  const w = frames[0][0].length;
  const h = frames[0].length;
  return (
    <span className={`sprite sprite-${name}${className ? ` ${className}` : ""}`} aria-hidden="true">
      <svg viewBox={`0 0 ${w} ${h}`} width={w} height={h} shapeRendering="crispEdges">
        {drawn.map((frame, i) => (
          <g key={i} className={`f f${i + 1}`}>
            {frame.map((r) => (
              <rect key={`${r.x},${r.y}`} x={r.x} y={r.y} width={r.w} height={1} fill={r.fill} />
            ))}
          </g>
        ))}
      </svg>
    </span>
  );
}
