// Generates the Strawberry Agent mark.
//
// Faceted, not friendly: the berry is cut from straight planes like a gemstone,
// the calyx is a set of blades rather than leaves, and the seeds are diamonds.
// Everything is a polygon — no curves — so the silhouette stays hard-edged and
// the shape survives being scaled down to a 16px tab icon.

import { mkdirSync, writeFileSync } from "node:fs";

const CX = 256;
const TOP = 168; // shoulder line
const TIP = 470; // bottom point
const HALF = 176; // half-width at the shoulders
const NOTCH = 214; // depth of the centre notch between the shoulders

/** Outline of the berry, clockwise from the tip. */
const BODY = [
  [CX, TIP],
  [CX - 96, TIP - 62],
  [CX - HALF, 300],
  [CX - HALF + 8, TOP + 24],
  [CX - 108, TOP - 16],
  [CX - 44, TOP + 4],
  [CX, NOTCH],
  [CX + 44, TOP + 4],
  [CX + 108, TOP - 16],
  [CX + HALF - 8, TOP + 24],
  [CX + HALF, 300],
  [CX + 96, TIP - 62],
];

const poly = (pts) => pts.map(([x, y]) => `${x.toFixed(1)},${y.toFixed(1)}`).join(" ");

// Three facet planes over the body. Each is clipped to the outline, so together
// they read as one solid cut from flat surfaces lit from the upper left.
const FACET_LIGHT = [
  [CX - 44, TOP + 4],
  [CX, NOTCH],
  [CX + 6, 330],
  [CX - 60, 300],
  [CX - HALF + 8, TOP + 24],
  [CX - 108, TOP - 16],
];
const FACET_MID = [
  [CX, NOTCH],
  [CX + 44, TOP + 4],
  [CX + 108, TOP - 16],
  [CX + HALF - 8, TOP + 24],
  [CX + HALF, 300],
  [CX + 40, 356],
  [CX + 6, 330],
];
const FACET_DARK = [
  [CX + 6, 330],
  [CX + 40, 356],
  [CX + HALF, 300],
  [CX + 96, TIP - 62],
  [CX, TIP],
  [CX - 96, TIP - 62],
  [CX - 60, 300],
];

/** A calyx blade: a narrow triangle with a kink, rotated about the crown. */
function blade(angle, len, width) {
  const pts = poly([
    [0, -width],
    [len * 0.55, -width * 0.42],
    [len, 0],
    [len * 0.55, width * 0.42],
    [0, width],
  ]);
  return `<polygon points="${pts}" transform="translate(${CX} ${TOP - 6}) rotate(${angle})"/>`;
}

const BLADES = [
  blade(196, 128, 26),
  blade(232, 104, 23),
  blade(268, 96, 21),
  blade(308, 104, 23),
  blade(344, 128, 26),
].join("\n      ");

/** Half-width of the body at a given y, used to place seeds inside the shape. */
function halfWidthAt(y) {
  if (y <= TOP + 24) return HALF - 30;
  if (y <= 300) return HALF - 18;
  const t = (y - 300) / (TIP - 300);
  return (HALF - 18) * (1 - t * t * 0.92);
}

// Seeds are diamonds on an offset lattice, shrinking toward the tip.
const seeds = [];
for (let row = 0, y = TOP + 62; y < TIP - 34; y += 44, row++) {
  const hw = halfWidthAt(y) - 26;
  if (hw <= 0) continue;
  const offset = row % 2 ? 24 : 0;
  for (let x = CX - hw + offset; x <= CX + hw - 1; x += 48) {
    const nx = (x - CX) / HALF;
    const scale = 1 - Math.abs(nx) * 0.2 - Math.max(0, (y - 300) / (TIP - 300)) * 0.3;
    const r = 8.5 * scale;
    seeds.push(
      `<polygon points="${poly([
        [x, y - r],
        [x + r * 0.62, y],
        [x, y + r],
        [x - r * 0.62, y],
      ])}" transform="rotate(${(nx * 16).toFixed(1)} ${x.toFixed(1)} ${y.toFixed(1)})"/>`,
    );
  }
}

const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512" role="img" aria-label="Strawberry Agent">
  <defs>
    <linearGradient id="sa-light" x1="0" y1="0" x2="0.7" y2="1">
      <stop offset="0" stop-color="#FF6A7F"/>
      <stop offset="1" stop-color="#F5304F"/>
    </linearGradient>
    <linearGradient id="sa-mid" x1="0.2" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#E11A40"/>
      <stop offset="1" stop-color="#B80F31"/>
    </linearGradient>
    <linearGradient id="sa-dark" x1="0" y1="0" x2="0.6" y2="1">
      <stop offset="0" stop-color="#A80D2E"/>
      <stop offset="1" stop-color="#6E0620"/>
    </linearGradient>
    <linearGradient id="sa-blade" x1="0" y1="0" x2="1" y2="0.8">
      <stop offset="0" stop-color="#5BE38E"/>
      <stop offset="0.55" stop-color="#1FA85C"/>
      <stop offset="1" stop-color="#0A6236"/>
    </linearGradient>
    <clipPath id="sa-body"><polygon points="${poly(BODY)}"/></clipPath>
  </defs>

  <g fill="url(#sa-blade)">
      ${BLADES}
    <polygon points="${poly([
      [CX - 11, TOP - 8],
      [CX + 11, TOP - 8],
      [CX + 7, 92],
      [CX - 7, 92],
    ])}"/>
  </g>

  <polygon points="${poly(BODY)}" fill="url(#sa-mid)"/>
  <g clip-path="url(#sa-body)">
    <polygon points="${poly(FACET_LIGHT)}" fill="url(#sa-light)"/>
    <polygon points="${poly(FACET_MID)}" fill="url(#sa-mid)"/>
    <polygon points="${poly(FACET_DARK)}" fill="url(#sa-dark)"/>
    <g fill="#FFD98A" opacity="0.95">
      ${seeds.join("\n      ")}
    </g>
    <polygon points="${poly(BODY)}" fill="none" stroke="#3D0413" stroke-width="6" stroke-linejoin="miter"/>
  </g>
</svg>
`;

mkdirSync("src/assets", { recursive: true });
writeFileSync("src/assets/logo.svg", svg);

// The same geometry as an inline Solid component, so the app never fetches the
// mark and the two can never drift apart. Gradient ids are suffixed per
// instance because several logos can be on screen at once.
const component = `// GENERATED by tools/make-logo.mjs — do not edit by hand.
// Run \`node tools/make-logo.mjs\` after changing the generator.

import { createUniqueId } from "solid-js";

export function Logo(props: { size?: number; muted?: boolean }) {
  const uid = createUniqueId();
  return (
${svg
  .trim()
  .replace(
    /^<svg [^>]*>/,
    '    <svg\n      width={props.size ?? 20}\n      height={props.size ?? 20}\n      viewBox="0 0 512 512"\n      role="img"\n      aria-label="Strawberry Agent"\n      style={{ opacity: props.muted ? 0.55 : 1 }}\n    >',
  )
  .replace(/id="sa-([a-z-]+)"/g, "id={`sa-$1-${uid}`}")
  .replace(/"url\(#sa-([a-z-]+)\)"/g, "{`url(#sa-$1-${uid})`}")
  .replace(/^/gm, "  ")
  .replace(/ {2}<\/svg>$/, "    </svg>")}
  );
}
`;
writeFileSync("src/components/Logo.tsx", component);

console.log(
  `wrote src/assets/logo.svg (${svg.length} bytes, ${seeds.length} seeds) and src/components/Logo.tsx`,
);
