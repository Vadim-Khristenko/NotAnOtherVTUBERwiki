/* ---------------------------------------------------------------------------
   The world behind the page: one fixed WebGL canvas for the whole landing,
   drawn like the art: flat toon colour with a dark edge.

   - The FilianWIKI icon by just.call.me.l, extruded. It owns the hero; past
     it, it glides to the corner above Snack Radio, where a small drawing of
     it (24 frames of it turning, rendered once at start) takes over, so no
     card can cover it and it costs nothing to keep there.
   - A swarm of snacks that rebuilds itself for every section: a vortex, rings
     round the zeroes, a ribbon under the tapes, rain behind the snack machine,
     a heart around Filian, an orbit round the recipe and the passport,
     fireworks at the end. Each snack springs toward its place, so a section
     change is a flock taking a new shape. Snacks keep out of the text.
   - Made for weak laptops too: one pass per object (the edge is drawn by the
     colour shader, not by a second copy), shaders compiled before the first
     frame, the page measured once per frame, and a resolution that follows
     the frame time down to 60% and back up when there is room.
   --------------------------------------------------------------------------- */
import {
  Box3, Color, DataTexture, DirectionalLight, ExtrudeGeometry, Group, HemisphereLight, InstancedMesh, MathUtils, Matrix4,
  Mesh, MeshToonMaterial, NearestFilter, PerspectiveCamera, Quaternion, Ray, RedFormat, Scene, SRGBColorSpace, Vector3,
  WebGLRenderer,
} from "three";
import { SVGLoader } from "three/examples/jsm/loaders/SVGLoader.js";
import { snacks } from "./snacks";
import { world as registry, type Formation } from "../lib/motion";

type Audio = { bass: number; mid: number; high: number; kick: number };
type Hooks = { boop?: () => void; dizzy?: () => void; ready?: () => void };
type Box = { x: number; y: number; w: number; h: number };
const PALETTE = ["#ff2d7a", "#a366ff", "#ffe3a3", "#7cf2c9", "#ff8fb8", "#6fb8ff", "#ffb35c"];

/** Three bands of light, like a cel: shadow, mid, lit. */
function toonRamp() {
  const tex = new DataTexture(new Uint8Array([90, 170, 255]), 3, 1, RedFormat);
  tex.minFilter = tex.magFilter = NearestFilter;
  tex.needsUpdate = true;
  return tex;
}

/** A toon material that also darkens where the surface turns away: the ink edge, in the same pass. */
function inked(params: ConstructorParameters<typeof MeshToonMaterial>[0], edge = 0.62) {
  const m = new MeshToonMaterial(params);
  m.onBeforeCompile = (shader) => {
    shader.fragmentShader = shader.fragmentShader.replace(
      "#include <dithering_fragment>",
      `float facing = abs(dot(normalize(normal), normalize(vViewPosition)));
       float ink = 1.0 - smoothstep(${(edge - 0.18).toFixed(2)}, ${edge.toFixed(2)}, facing);
       gl_FragColor.rgb = mix(gl_FragColor.rgb, vec3(0.11, 0.06, 0.18), ink * 0.92);
       #include <dithering_fragment>`,
    );
  };
  return m;
}

export async function start(host: HTMLElement, hooks: Hooks = {}) {
  const svg = await (await fetch("/assets/art/mark.svg")).text();
  const small = () => innerWidth < 760;
  const cores = navigator.hardwareConcurrency || 4;
  const count = small() ? 90 : cores <= 4 ? 130 : 170;
  const maxRatio = Math.min(devicePixelRatio || 1, 1.5);
  let ratio = maxRatio;
  // on a sharp phone screen 60% reads as pixels, so the floor sits higher there
  const minRatio = Math.min(maxRatio, (devicePixelRatio || 1) >= 2 ? 0.9 : 0.6);

  const renderer = new WebGLRenderer({ antialias: maxRatio < 1.3, alpha: true, powerPreference: "high-performance" });
  renderer.setPixelRatio(ratio);
  renderer.outputColorSpace = SRGBColorSpace;
  renderer.setClearColor(0x000000, 0);
  const canvas = renderer.domElement;
  canvas.style.opacity = "0";
  canvas.style.transition = "opacity 1.2s cubic-bezier(.16,1,.3,1)";
  host.append(canvas);

  const scene = new Scene();
  const camera = new PerspectiveCamera(32, innerWidth / innerHeight, 0.1, 120);
  camera.position.set(0, 0, 16);
  scene.add(new HemisphereLight(0xfff0f7, 0x5a3a8a, 1.35));
  const key = new DirectionalLight(0xffffff, 2.3);
  key.position.set(3, 5, 8);
  scene.add(key);
  const ramp = toonRamp();

  /* ---- the cat: a light mesh, so a weak GPU can spin it */
  const data = new SVGLoader().parse(svg);
  const shapes = data.paths.flatMap((p) => SVGLoader.createShapes(p));
  const catGeo = new ExtrudeGeometry(shapes, { depth: 16, bevelEnabled: true, bevelThickness: 2.6, bevelSize: 1.2, bevelSegments: 1, curveSegments: 4 });
  catGeo.center();
  catGeo.rotateX(Math.PI);
  catGeo.scale(0.03, 0.03, 0.03);
  catGeo.computeVertexNormals();
  catGeo.computeBoundingBox();
  const catMat = inked({ color: 0xff3d88, gradientMap: ramp, emissive: new Color(0xff2d7a), emissiveIntensity: 0.06 }, 0.5);
  const cat = new Mesh(catGeo, catMat);
  const catGroup = new Group();
  catGroup.add(cat);
  scene.add(catGroup);
  const catBox = new Box3();

  /* ---- the snacks */
  const kinds = snacks();
  const snackMat = inked({ vertexColors: true, gradientMap: ramp, emissive: new Color(0xff6fae), emissiveIntensity: 0 });
  const per = Math.ceil(count / kinds.length);
  const meshes: InstancedMesh[] = [];
  for (const [k, kind] of kinds.entries()) {
    const m = new InstancedMesh(kind.geometry, snackMat, per);
    m.frustumCulled = false;
    for (let i = 0; i < per; i++) m.setColorAt(i, kind.tinted ? new Color(PALETTE[(i * 5 + k) % PALETTE.length]) : new Color(1, 1, 1));
    if (m.instanceColor) m.instanceColor.needsUpdate = true;
    scene.add(m);
    meshes.push(m);
  }
  type P = { pos: Vector3; vel: Vector3; target: Vector3; q: Quaternion; axis: Vector3; spin: number; s: number; seed: number; delay: number };
  const parts: P[] = [];
  meshes.forEach((m, k) => {
    for (let i = 0; i < m.count; i++) {
      parts.push({
        // everything starts inside the cat and bursts out of it
        pos: new Vector3(), vel: new Vector3().randomDirection().multiplyScalar(MathUtils.randFloat(4, 11)), target: new Vector3(),
        q: new Quaternion().random(), axis: new Vector3().randomDirection(),
        spin: MathUtils.randFloat(0.25, 0.9), s: MathUtils.randFloat(0.45, 0.8) * kinds[k].scale, seed: Math.random(), delay: Math.random(),
      });
    }
  });

  /* ---- screen and world */
  let halfH = 1, halfW = 1;
  const resize = () => {
    renderer.setSize(innerWidth, innerHeight, false);
    camera.aspect = innerWidth / innerHeight;
    camera.updateProjectionMatrix();
    halfH = Math.tan(MathUtils.degToRad(camera.fov / 2)) * camera.position.z;
    halfW = halfH * camera.aspect;
  };
  addEventListener("resize", resize);
  resize();
  const wx = (px: number) => (px / innerWidth) * 2 * halfW - halfW;
  const wy = (py: number) => halfH - (py / innerHeight) * 2 * halfH;
  const boxOf = (el: Element | null | undefined): Box | null => {
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { x: wx(r.left + r.width / 2), y: wy(r.top + r.height / 2), w: (r.width / innerWidth) * halfW, h: (r.height / innerHeight) * halfH };
  };

  /* ---- the mascot: 72 frames of the cat turning, drawn once, then just a picture.
     Drawn with the same camera distance and pose as the real cat, so the moment
     one replaces the other cannot be seen: same place, same size, same turn. */
  const FRAMES = 72, CELL = 144, COLS = 8;
  const BAKE_Z = 13;
  const sheet = document.createElement("canvas");
  sheet.width = CELL * COLS;
  sheet.height = CELL * Math.ceil(FRAMES / COLS);
  const sheetCtx = sheet.getContext("2d")!;
  {
    const mScene = new Scene();
    mScene.add(new HemisphereLight(0xfff0f7, 0x5a3a8a, 1.35));
    const mKey = new DirectionalLight(0xffffff, 2.3);
    mKey.position.set(3, 5, 8);
    mScene.add(mKey);
    const mCat = new Mesh(catGeo, catMat);
    mScene.add(mCat);
    const mCam = new PerspectiveCamera(32, 1, 0.1, 50);
    mCam.position.set(0, 0, BAKE_Z);
    const was = renderer.getPixelRatio();
    renderer.setPixelRatio(1);
    renderer.setSize(CELL, CELL, false);
    for (let f = 0; f < FRAMES; f++) {
      mCat.rotation.set(0, (f / FRAMES) * Math.PI * 2, 0);
      renderer.clear();
      renderer.render(mScene, mCam);
      sheetCtx.drawImage(canvas, 0, 0, CELL, CELL, (f % COLS) * CELL, Math.floor(f / COLS) * CELL, CELL, CELL);
    }
    renderer.setPixelRatio(was);
    resize();
  }
  const mascot = document.createElement("canvas");
  mascot.className = "mascot";
  mascot.width = mascot.height = CELL;
  mascot.setAttribute("aria-hidden", "true");
  mascot.style.cssText = "position:fixed;z-index:58;left:0;top:0;opacity:0;pointer-events:none;cursor:pointer;transform-origin:50% 50%";
  document.body.append(mascot);
  const mascotCtx = mascot.getContext("2d")!;
  const cellAt = (f: number) => [(f % COLS) * CELL, Math.floor(f / COLS) * CELL] as const;
  mascot.addEventListener("click", () => boop());
  let handedFor = 0, mascotSize = 0;
  const onScreen = new Vector3();

  /* ---- measured once a frame: the section, its anchors, the text to avoid */
  let formation: Formation = "vortex";
  let section: HTMLElement | null = null;
  let heroSeen = 1, heroSmooth = 1;
  let anchors: Box[] = [];
  let anchor: Box | null = null;
  let keepOut: { x0: number; x1: number; y0: number; y1: number }[] = [];
  let deckTop = innerHeight;
  const bursts: { x: number; y: number; t: number }[] = [];
  let fireAt = 0;
  const heroEl = () => registry.sections.find((s) => s.formation === "vortex")?.el ?? null;

  function measure(frame: number) {
    const hero = heroEl()?.getBoundingClientRect();
    heroSeen = hero ? MathUtils.clamp(hero.bottom / innerHeight, 0, 1) : 0;
    const deck = document.querySelector(".deck")?.getBoundingClientRect();
    deckTop = deck && deck.width ? deck.top : innerHeight;
    if (frame % 4 !== 0) return;
    const mid = innerHeight * 0.5;
    let found: { el: HTMLElement; formation: Formation } | null = null;
    for (const s of registry.sections) {
      const r = s.el.getBoundingClientRect();
      if (r.top <= mid && r.bottom >= mid) { found = s; break; }
    }
    if (found && found.formation !== formation) { formation = found.formation; parts.forEach((p) => (p.delay = Math.random())); }
    section = found?.el ?? null;
    anchors = formation === "zeros" && section ? [...section.querySelectorAll(".window")].map((e) => boxOf(e)!).filter(Boolean) : [];
    const sel: Partial<Record<Formation, string>> = { ribbon: ".tapes", heart: ".art", orbit: ".passport, .recipe", trail: ".recipe" };
    anchor = sel[formation] && section ? boxOf(section.querySelector(sel[formation]!)) : null;
    keepOut = [];
    for (const el of document.querySelectorAll(".section-head, .hero .copy, .join .copy, .credits .outro.on")) {
      const r = el.getBoundingClientRect();
      if (r.bottom < 0 || r.top > innerHeight) continue;
      keepOut.push({ x0: wx(r.left - 36), x1: wx(r.right + 36), y0: wy(r.bottom + 30), y1: wy(r.top - 30) });
    }
  }
  function avoid(v: Vector3) {
    for (const k of keepOut) {
      if (v.x > k.x0 && v.x < k.x1 && v.y > k.y0 && v.y < k.y1) {
        const dl = v.x - k.x0, dr = k.x1 - v.x, db = v.y - k.y0, dt = k.y1 - v.y;
        const m = Math.min(dl, dr, db, dt);
        if (m === dl) v.x = k.x0 - 0.25; else if (m === dr) v.x = k.x1 + 0.25; else if (m === db) v.y = k.y0 - 0.25; else v.y = k.y1 + 0.25;
      }
    }
    return v;
  }

  const N = parts.length;
  function targetFor(i: number, p: P, t: number, a: Audio) {
    const u = i / N;
    switch (formation) {
      case "vortex": {
        const home = catGroup.position;
        const r = 2.4 + p.seed * 5;
        const ang = t * (0.12 + p.seed * 0.1) + u * Math.PI * 14;
        return p.target.set(home.x + Math.cos(ang) * r, home.y + (p.delay - 0.5) * 6 + Math.sin(t + i) * 0.2, Math.sin(ang) * r * 0.45 - 3);
      }
      case "zeros": {
        const box = anchors[i % Math.max(1, anchors.length)];
        if (!box) return p.target.set(0, 0, -4);
        const ang = t * 0.8 * (i % 2 ? 1 : -1) + u * Math.PI * 6;
        const r = Math.max(box.w, box.h) * 1.35;
        return p.target.set(box.x + Math.cos(ang) * r, box.y + Math.sin(ang) * r * 1.1, -1.5 + Math.sin(ang) * 1.2);
      }
      case "ribbon": {
        const x = (u * 2 - 1) * halfW * 1.1;
        const y = (anchor?.y ?? 0) + Math.sin(x * 0.9 + t * 1.6 + (i % 3)) * 0.9 + ((i % 3) - 1) * 0.5;
        return p.target.set(x, y, -2 - (i % 3));
      }
      case "rain": {
        const fall = ((t * (0.45 + p.seed * 0.5) + p.delay * 3) % 3) / 3;
        return p.target.set((p.seed * 2 - 1) * halfW * 1.05, halfH * 1.2 - fall * halfH * 2.4, -3 - p.delay * 3);
      }
      case "heart": {
        const th = u * Math.PI * 2;
        const beat = 1 + a.bass * 0.18 + Math.max(0, Math.sin(t * 5)) ** 8 * 0.08;
        const s = (anchor ? Math.max(anchor.w, anchor.h) * 0.075 : 0.18) * beat;
        const hx = 16 * Math.sin(th) ** 3, hy = 13 * Math.cos(th) - 5 * Math.cos(2 * th) - 2 * Math.cos(3 * th) - Math.cos(4 * th);
        return p.target.set((anchor?.x ?? -halfW * 0.4) + hx * s, (anchor?.y ?? 0) + hy * s + 0.3, -2 + (p.seed - 0.5) * 1.5);
      }
      case "trail":
      case "orbit": {
        const ang = t * 0.3 + u * Math.PI * 2;
        const rx = (anchor?.w ?? 3) * 1.25, ry = (anchor?.h ?? 2) * 0.6;
        return p.target.set((anchor?.x ?? 0) + Math.cos(ang) * rx, (anchor?.y ?? 0) + Math.sin(ang) * ry, Math.sin(ang) * 3 - 2);
      }
      case "fireworks": {
        const b = bursts[i % Math.max(1, bursts.length)];
        if (!b) return p.target.set(0, -halfH * 1.4, -4);
        const age = t - b.t;
        const g = i * 2.39996;
        return p.target.set(b.x + Math.cos(g) * age * 2.6, b.y + Math.sin(g * 1.3) * age * 2.6 - age * age * 0.6, -3 + Math.cos(i * 1.7));
      }
      default: {
        const x = (p.seed * 2 - 1) * halfW * 1.05;
        const y = (p.delay * 2 - 1) * halfH * 1.05;
        return p.target.set(x + Math.sin(t * 0.2 + i) * 0.4, y + Math.cos(t * 0.25 + i) * 0.4, -4 - p.seed * 4);
      }
    }
  }

  /* ---- pointer: push snacks, spin and boop the cat */
  const pointer = { x: 0, y: 0, wx: 999, wy: 999 };
  let spinX = 0, spinY = 0, velX = 0, velY = 0.25;
  let dragging = false, lastX = 0, lastY = 0, downX = 0, downY = 0, moved = false;
  let squash = 0, boops = 0, boopTimer = 0, dizzy = 0, lastBoop = 0;
  const interactive = (el: EventTarget | null) => el instanceof Element && !!el.closest("a, button, input, select, textarea, label, .copy, .deck");
  addEventListener("pointermove", (e) => {
    pointer.x = (e.clientX / innerWidth) * 2 - 1;
    pointer.y = -(e.clientY / innerHeight) * 2 + 1;
    pointer.wx = wx(e.clientX);
    pointer.wy = wy(e.clientY);
    if (!dragging) return;
    const dx = e.clientX - lastX, dy = e.pointerType === "touch" ? 0 : e.clientY - lastY;
    lastX = e.clientX; lastY = e.clientY;
    if (Math.hypot(e.clientX - downX, e.clientY - downY) > 6) moved = true;
    velY = dx * 0.012; velX = dy * 0.012;
    spinY += velY; spinX += velX;
  }, { passive: true });
  addEventListener("pointerdown", (e) => {
    if (interactive(e.target) || heroSeen < 0.3 || !heroEl()?.contains(e.target as Node)) return;
    dragging = true; moved = false;
    lastX = downX = e.clientX; lastY = downY = e.clientY;
  }, { passive: true });
  const ray = new Ray();
  const hitCat = (x: number, y: number) => {
    catBox.copy(catGeo.boundingBox!).applyMatrix4(cat.matrixWorld);
    const dir = new Vector3((x / innerWidth) * 2 - 1, -(y / innerHeight) * 2 + 1, 0.5).unproject(camera).sub(camera.position).normalize();
    ray.set(camera.position, dir);
    return ray.intersectsBox(catBox);
  };
  addEventListener("pointerup", (e) => {
    if (!dragging) return;
    dragging = false;
    if (!moved && hitCat(e.clientX, e.clientY)) boop();
  }, { passive: true });
  function boop() {
    const now = performance.now();
    if (now - lastBoop < 140) return;
    lastBoop = now;
    squash = 1;
    velY += 0.35 * (Math.random() < 0.5 ? -1 : 1);
    const c = catGroup.position;
    for (const p of parts) {
      const dx = p.pos.x - c.x, dy = p.pos.y - c.y, dz = p.pos.z - c.z, d = Math.hypot(dx, dy, dz) || 1;
      const f = MathUtils.randFloat(7, 12) / d;
      p.vel.x += dx * f; p.vel.y += dy * f; p.vel.z += dz * f;
    }
    boops++;
    clearTimeout(boopTimer);
    boopTimer = window.setTimeout(() => (boops = 0), 1400);
    if (boops >= 5) { boops = 0; dizzy = 2.5; hooks.dizzy?.(); } else hooks.boop?.();
  }
  registry.boop = boop;

  const audio = (): Audio => (window as unknown as { snackAudio?: Audio }).snackAudio ?? { bass: 0, mid: 0, high: 0, kick: 0 };
  const m4 = new Matrix4(), scl = new Vector3(), spinQ = new Quaternion();
  const homeHero = new Vector3(), corner = new Vector3();
  const stats = { cpu: 0, frame: 16, ratio };
  (window as unknown as { __world: typeof stats }).__world = stats;
  let last = performance.now(), t = 0, intro = 0, frame = 0, slowFor = 0, fastFor = 0;

  // compile every shader before the first frame, so nothing stutters on arrival
  await renderer.compileAsync(scene, camera);

  function tick(now: number) {
    requestAnimationFrame(tick);
    if (document.hidden) { last = now; return; }
    const began = performance.now();
    const raw = now - last;
    const dt = Math.min(raw / 1000, 1 / 20);
    last = now;
    t += dt;
    intro = Math.min(1, intro + dt * 0.55);
    const ease = 1 - Math.pow(1 - intro, 3);
    const a = audio();
    measure(frame++);

    // resolution follows the frame time: down when frames run long, back up when there is room
    stats.frame += (raw - stats.frame) * 0.05;
    if (stats.frame > 22) { slowFor += dt; fastFor = 0; } else if (stats.frame < 14) { fastFor += dt; slowFor = 0; } else { slowFor = fastFor = 0; }
    if (slowFor > 1 && ratio > minRatio) { ratio = Math.max(minRatio, ratio * 0.85); renderer.setPixelRatio(ratio); resize(); slowFor = 0; }
    if (fastFor > 3 && ratio < maxRatio) { ratio = Math.min(maxRatio, ratio * 1.1); renderer.setPixelRatio(ratio); resize(); fastFor = 0; }
    stats.ratio = ratio;

    /* the cat: in the hero, big; after it, it glides to the corner above the radio */
    heroSmooth += (heroSeen - heroSmooth) * Math.min(1, dt * 7);
    const hero = heroSmooth;
    const portrait = camera.aspect < 1;
    if (portrait) homeHero.set(0, halfH * 0.46, 0); else homeHero.set(Math.min(halfW * 0.5, 5.6), 0.5, 0);
    const cornerPx = { x: innerWidth - (portrait ? 58 : 84), y: deckTop - (portrait ? 64 : 78) };
    corner.set(wx(cornerPx.x), wy(cornerPx.y), 0);
    catGroup.position.lerpVectors(corner, homeHero, hero);
    catGroup.position.y += Math.sin(t * 1.1) * 0.15 * hero;
    const size = (portrait ? Math.min(0.85, halfW / 3.4) : Math.min(0.95, halfH / 5.4)) * MathUtils.lerp(0.24, 1, hero) * (0.6 + 0.4 * ease);
    catGroup.scale.setScalar(size);
    if (!dragging) {
      velY *= 0.965; velX *= 0.92;
      spinY += velY * dt * 6 + dt * (0.25 + (1 - hero) * 0.5);
      spinX += velX * dt * 6;
      spinX *= 0.97;
    }
    if (dizzy > 0) { dizzy -= dt; spinY += dt * 14 * dizzy; }
    squash *= 0.9;
    const beat = 1 + a.bass * 0.09 + a.kick * 0.05;
    cat.rotation.set(spinX - pointer.y * 0.18 * hero, spinY + pointer.x * 0.35 * hero, Math.sin(t * 0.7) * 0.06);
    cat.scale.set(beat * (1 + squash * 0.18), beat * (1 - squash * 0.22), beat);
    catMat.emissiveIntensity = 0.06 + a.kick * 0.4;

    // the handover: in the corner the drawing takes the cat's place, at the
    // cat's own spot, size and turn, so nothing jumps
    const handed = hero < 0.04;
    cat.visible = !handed;
    handedFor = handed ? handedFor + dt : 0;
    if (handed !== (mascot.style.opacity === "1")) {
      mascot.style.opacity = handed ? "1" : "0";
      mascot.style.pointerEvents = handed ? "auto" : "none";
    }
    if (handed) {
      // the size of a whole cell on screen: the bake camera sees 2·tan(fov/2)·BAKE_Z units
      const px = Math.round(size * (BAKE_Z / camera.position.z) * innerHeight * (Math.tan(MathUtils.degToRad(16)) * camera.position.z / halfH));
      if (px !== mascotSize) { mascotSize = px; mascot.style.width = mascot.style.height = `${px}px`; }
      onScreen.copy(catGroup.position).project(camera);
      const sx = (onScreen.x + 1) / 2 * innerWidth, sy = (1 - onScreen.y) / 2 * innerHeight;
      // a little bob that eases in after the handover, never a jump
      const bob = Math.sin(t * 2.2) * 4 * Math.min(1, handedFor / 0.8);
      const sqx = beat * (1 + squash * 0.18), sqy = beat * (1 - squash * 0.22);
      mascot.style.transform = `translate(${(sx - px / 2).toFixed(1)}px, ${(sy - px / 2 - bob).toFixed(1)}px) rotate(${(-Math.sin(t * 0.7) * 0.06).toFixed(3)}rad) scale(${sqx.toFixed(3)}, ${sqy.toFixed(3)})`;
      // between two frames, the next one shows through by how far the turn has gone
      const turn = ((spinY % (Math.PI * 2)) + Math.PI * 2) % (Math.PI * 2);
      const pos = (turn / (Math.PI * 2)) * FRAMES;
      const f = Math.floor(pos) % FRAMES, g = (f + 1) % FRAMES, k = pos - Math.floor(pos);
      mascotCtx.clearRect(0, 0, CELL, CELL);
      const [fx, fy] = cellAt(f), [gx, gy] = cellAt(g);
      mascotCtx.globalAlpha = 1;
      mascotCtx.drawImage(sheet, fx, fy, CELL, CELL, 0, 0, CELL, CELL);
      mascotCtx.globalAlpha = k;
      mascotCtx.drawImage(sheet, gx, gy, CELL, CELL, 0, 0, CELL, CELL);
      mascotCtx.globalAlpha = 1;
    }

    if (formation === "fireworks" && t > fireAt) {
      bursts.push({ x: MathUtils.randFloatSpread(halfW * 1.4), y: MathUtils.randFloat(-halfH * 0.2, halfH * 0.6), t });
      if (bursts.length > 4) bursts.shift();
      fireAt = t + 0.9 + Math.random() * 0.6;
    }

    snackMat.emissiveIntensity = a.kick * 0.3;
    // a phone screen is narrow: smaller snacks, so they read as a swarm and not as a wall
    const shrink = portrait ? 0.62 : 1;
    let idx = 0;
    for (const m of meshes) {
      for (let i = 0; i < m.count; i++, idx++) {
        const p = parts[idx];
        const target = avoid(targetFor(idx, p, t, a));
        const pull = intro > p.delay * 0.5 ? 5.5 : 0.8;
        p.vel.x += (target.x - p.pos.x) * pull * dt;
        p.vel.y += (target.y - p.pos.y) * pull * dt;
        p.vel.z += (target.z - p.pos.z) * pull * dt;
        const dx = p.pos.x - pointer.wx, dy = p.pos.y - pointer.wy, d2 = dx * dx + dy * dy;
        if (d2 < 5) { const d = Math.sqrt(d2) || 1; const f = (2.25 - d) * 16 * dt; p.vel.x += (dx / d) * f; p.vel.y += (dy / d) * f; }
        const damp = 1 - 3.4 * dt;
        p.vel.x *= damp; p.vel.y *= damp; p.vel.z *= damp;
        const sp = p.vel.length();
        if (sp > 30) p.vel.multiplyScalar(30 / sp);
        p.pos.addScaledVector(p.vel, dt);
        spinQ.setFromAxisAngle(p.axis, p.spin * dt * (1 + a.high * 2 + sp * 0.15));
        p.q.multiply(spinQ);
        const s = p.s * (1 + a.kick * 0.12) * ease * shrink;
        m.setMatrixAt(i, m4.compose(p.pos, p.q, scl.set(s, s, s)));
      }
      m.instanceMatrix.needsUpdate = true;
    }

    camera.position.x += (pointer.x * 0.5 - camera.position.x) * 0.04;
    camera.position.y += (pointer.y * 0.35 - camera.position.y) * 0.04;
    camera.lookAt(0, 0, 0);
    renderer.render(scene, camera);
    stats.cpu += (performance.now() - began - stats.cpu) * 0.1;
    if (frame === 3) { canvas.style.opacity = "1"; hooks.ready?.(); }
  }
  // the snacks start where the cat is
  const startAt = new Vector3(Math.min(halfW * 0.5, 5.6), 0.5, 0);
  if (camera.aspect < 1) startAt.set(0, halfH * 0.46, 0);
  parts.forEach((p) => p.pos.copy(startAt));
  requestAnimationFrame(tick);
}
