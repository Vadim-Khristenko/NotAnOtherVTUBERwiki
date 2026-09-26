/* Snacks in the style of the art: flat toon colours and a dark edge (drawn
   by the world in the same pass as the colour). Built from
   primitives with their colours baked into the vertices, so one material
   draws them all. `tinted` snacks are white here and take a colour per copy
   (gummy bears, candies, stars); the baked goods keep theirs. */
import {
  BufferAttribute, BufferGeometry, CapsuleGeometry, Color, ConeGeometry, CylinderGeometry, DodecahedronGeometry,
  ExtrudeGeometry, LatheGeometry, Shape, SphereGeometry, TorusGeometry, Vector2,
} from "three";
import { mergeGeometries } from "three/examples/jsm/utils/BufferGeometryUtils.js";

export type Snack = { name: string; geometry: BufferGeometry; tinted: boolean; scale: number };

function paint(g: BufferGeometry, hex: string, jitter = 0) {
  const geo = g.index ? g.toNonIndexed() : g;
  const c = new Color(hex);
  const n = geo.getAttribute("position").count;
  const colors = new Float32Array(n * 3);
  for (let i = 0; i < n; i++) {
    const k = 1 + (Math.random() - 0.5) * jitter;
    colors[i * 3] = c.r * k;
    colors[i * 3 + 1] = c.g * k;
    colors[i * 3 + 2] = c.b * k;
  }
  geo.setAttribute("color", new BufferAttribute(colors, 3));
  geo.deleteAttribute("uv");
  return geo;
}
const merge = (parts: BufferGeometry[]) => {
  const g = mergeGeometries(parts, false)!;
  g.computeBoundingSphere();
  return g;
};

/** A cookie with a bite out of it and chocolate chips. */
function cookie() {
  // The rim, with any point inside a bite pushed out to the bite's edge:
  // two scallops, and no way for the outline to cross itself.
  const bites = [{ x: 0.52, y: 0.2, r: 0.17 }, { x: 0.38, y: 0.43, r: 0.15 }];
  const pts: Vector2[] = [];
  for (let i = 0; i < 96; i++) {
    const a = (i / 96) * Math.PI * 2;
    // along the ray from the centre, stop where the first bite begins
    const ux = Math.cos(a), uy = Math.sin(a);
    let t = 0.55;
    for (const b of bites) {
      const ub = ux * b.x + uy * b.y;
      const disc = ub * ub - (b.x * b.x + b.y * b.y) + b.r * b.r;
      if (disc >= 0) {
        const near = ub - Math.sqrt(disc);
        if (near > 0 && near < t && ub + Math.sqrt(disc) > 0.5) t = near;
      }
    }
    pts.push(new Vector2(ux * t, uy * t));
  }
  const s = new Shape(pts);
  const base = new ExtrudeGeometry(s, { depth: 0.12, bevelEnabled: true, bevelThickness: 0.05, bevelSize: 0.05, bevelSegments: 3, curveSegments: 22 });
  base.translate(0, 0, -0.06);
  const parts = [paint(base, "#d99a55", 0.14)];
  const chips = [[-0.25, 0.22], [0.08, -0.32], [-0.32, -0.15], [0.22, 0.05], [-0.05, 0.38], [0.3, -0.25]];
  for (const [x, y] of chips) {
    const chip = new DodecahedronGeometry(0.06 + Math.random() * 0.025);
    chip.translate(x, y, 0.1);
    parts.push(paint(chip, "#3b1f14"));
  }
  return merge(parts);
}

/** A glazed donut with drips down its side and sprinkles on top. */
function donut(icing: string) {
  const dough = new TorusGeometry(0.4, 0.19, 16, 40);
  const glaze = new TorusGeometry(0.4, 0.2, 16, 40);
  glaze.scale(1.02, 1.02, 0.6);
  glaze.translate(0, 0, 0.07);
  const parts = [paint(dough, "#e2ad6c", 0.1), paint(glaze, icing)];
  for (let k = 0; k < 7; k++) {
    const a = (k / 7) * Math.PI * 2 + 0.3;
    const drip = new CapsuleGeometry(0.045, 0.06 + Math.random() * 0.08, 4, 8);
    drip.rotateX(Math.PI / 2);
    drip.translate(Math.cos(a) * 0.59, Math.sin(a) * 0.59, -0.02);
    parts.push(paint(drip, icing));
  }
  const sprinkle = ["#ffffff", "#7cf2c9", "#ffe3a3", "#6fb8ff", "#ff2d7a"];
  for (let k = 0; k < 16; k++) {
    const a = Math.random() * Math.PI * 2;
    const r = 0.4 + (Math.random() - 0.5) * 0.22;
    const sp = new CapsuleGeometry(0.017, 0.07, 2, 6);
    sp.rotateZ(Math.random() * Math.PI);
    sp.translate(Math.cos(a) * r, Math.sin(a) * r, 0.2);
    parts.push(paint(sp, sprinkle[k % sprinkle.length]));
  }
  return merge(parts);
}

/** A wrapped candy: a round middle and twisted, pleated ends. */
function candy() {
  const body = new SphereGeometry(0.3, 22, 16);
  body.scale(1.25, 1, 0.95);
  const parts = [paint(body, "#ffffff")];
  for (const side of [-1, 1]) {
    // pleats: a lathe with a wavy profile
    const pts = Array.from({ length: 9 }, (_, i) => new Vector2(0.05 + (i / 8) * 0.2 + (i % 2) * 0.03, (i / 8) * 0.34));
    const end = new LatheGeometry(pts, 10);
    end.rotateZ(side * -Math.PI / 2);
    end.translate(side * 0.34, 0, 0);
    parts.push(paint(end, "#f4f0ff"));
    const knot = new TorusGeometry(0.06, 0.025, 6, 12);
    knot.rotateY(Math.PI / 2);
    knot.translate(side * 0.36, 0, 0);
    parts.push(paint(knot, "#ffe3a3"));
  }
  return merge(parts);
}

/** A biscuit stick dipped in a coat, with ridges round the coat. */
function pocky(coat: string, ring: string) {
  const stick = new CylinderGeometry(0.055, 0.06, 0.36, 10);
  stick.translate(0, -0.62, 0);
  const dipped = new CylinderGeometry(0.078, 0.078, 1.0, 12);
  dipped.translate(0, 0.07, 0);
  const tip = new SphereGeometry(0.078, 12, 6, 0, Math.PI * 2, 0, Math.PI / 2);
  tip.translate(0, 0.57, 0);
  const parts = [paint(stick, "#e9c48c"), paint(dipped, coat), paint(tip, coat)];
  for (let k = 0; k < 6; k++) {
    const r = new TorusGeometry(0.08, 0.012, 5, 16);
    r.rotateX(Math.PI / 2);
    r.rotateZ(0.35);
    r.translate(0, -0.3 + k * 0.15, 0);
    parts.push(paint(r, ring));
  }
  return merge(parts);
}

/** A gummy bear with a lighter belly and snout. */
function bear() {
  const parts: BufferGeometry[] = [];
  const blob = (r: number, x: number, y: number, z = 0, sy = 1, hex = "#ffffff") => {
    const s = new SphereGeometry(r, 16, 12);
    s.scale(1, sy, 0.8);
    s.translate(x, y, z);
    parts.push(paint(s, hex));
  };
  blob(0.3, 0, -0.12, 0, 1.15);
  blob(0.17, 0, -0.14, 0.13, 1.1, "#fff4fa");
  blob(0.24, 0, 0.3);
  blob(0.09, -0.17, 0.48); blob(0.09, 0.17, 0.48);
  blob(0.1, -0.28, -0.02); blob(0.1, 0.28, -0.02);
  blob(0.12, -0.16, -0.46); blob(0.12, 0.16, -0.46);
  const snout = new SphereGeometry(0.08, 10, 8);
  snout.scale(1.2, 0.8, 0.7);
  snout.translate(0, 0.23, 0.17);
  parts.push(paint(snout, "#fff4fa"));
  return merge(parts);
}

/** A puffy star. */
function star() {
  const shape = new Shape();
  for (let i = 0; i < 10; i++) {
    const r = i % 2 ? 0.24 : 0.52;
    const a = (i / 10) * Math.PI * 2 - Math.PI / 2;
    if (i) shape.lineTo(Math.cos(a) * r, Math.sin(a) * r);
    else shape.moveTo(Math.cos(a) * r, Math.sin(a) * r);
  }
  shape.closePath();
  const g = new ExtrudeGeometry(shape, { depth: 0.14, bevelEnabled: true, bevelThickness: 0.09, bevelSize: 0.08, bevelSegments: 4 });
  g.translate(0, 0, -0.07);
  g.rotateZ(Math.PI);
  const parts = [paint(g, "#ffffff")];
  return merge(parts);
}

/** A mochi: soft and round, with a leaf and a band of green. */
function mochi() {
  const body = new SphereGeometry(0.38, 22, 16);
  body.scale(1, 0.85, 0.75);
  const band = new CylinderGeometry(0.2, 0.2, 0.3, 16, 1, true);
  band.rotateX(Math.PI / 2);
  band.scale(1.9, 0.75, 1);
  band.translate(0, -0.18, 0);
  const parts = [paint(body, "#ffd6e7"), paint(band, "#7cc97a")];
  const leaf = new ConeGeometry(0.08, 0.16, 6);
  leaf.translate(0, 0.37, 0);
  parts.push(paint(leaf, "#4fa35a"));
  return merge(parts);
}

export function snacks(): Snack[] {
  return [
    { name: "cookie", geometry: cookie(), tinted: false, scale: 1 },
    { name: "donut", geometry: donut("#ff7fb6"), tinted: false, scale: 1 },
    { name: "donut-violet", geometry: donut("#b48bff"), tinted: false, scale: 1 },
    { name: "candy", geometry: candy(), tinted: true, scale: 1 },
    { name: "pocky", geometry: pocky("#5a2d1e", "#7a4331"), tinted: false, scale: 0.95 },
    { name: "pocky-pink", geometry: pocky("#ff8fc0", "#ffc2dc"), tinted: false, scale: 0.95 },
    { name: "bear", geometry: bear(), tinted: true, scale: 1.05 },
    { name: "star", geometry: star(), tinted: true, scale: 0.9 },
    { name: "mochi", geometry: mochi(), tinted: false, scale: 0.95 },
  ];
}
