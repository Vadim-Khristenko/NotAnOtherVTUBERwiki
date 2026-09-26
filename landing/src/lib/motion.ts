/* Small shared pieces: seeing a section arrive, toasts, confetti, and the
   registry the 3D world reads to know which section is on screen. */
import { onBeforeUnmount, onMounted, reactive, ref, type Ref } from "vue";

export const isBrowser = typeof window !== "undefined";
export const reducedMotion = () => isBrowser && matchMedia("(prefers-reduced-motion: reduce)").matches;
export const finePointer = () => isBrowser && matchMedia("(pointer: fine)").matches;
export const clamp = (v: number, a: number, b: number) => Math.min(b, Math.max(a, v));

/** True once the element has come into view (and stays true). */
export function useSeen(el: Ref<HTMLElement | null>, threshold = 0.2) {
  const seen = ref(false);
  let io: IntersectionObserver | null = null;
  onMounted(() => {
    if (!el.value) return;
    if (reducedMotion() || !("IntersectionObserver" in window)) { seen.value = true; return; }
    io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) { seen.value = true; io?.disconnect(); }
    }, { threshold, rootMargin: "0px 0px -8% 0px" });
    io.observe(el.value);
  });
  onBeforeUnmount(() => io?.disconnect());
  return seen;
}

/** 0 when the element's top reaches the bottom of the screen, 1 when its bottom leaves the top. */
export function useScrollProgress(el: Ref<HTMLElement | null>) {
  const p = ref(0);
  let raf = 0;
  const update = () => {
    raf = 0;
    const r = el.value?.getBoundingClientRect();
    if (!r) return;
    p.value = clamp((innerHeight - r.top) / (innerHeight + r.height), 0, 1);
  };
  const onScroll = () => { if (!raf) raf = requestAnimationFrame(update); };
  onMounted(() => { addEventListener("scroll", onScroll, { passive: true }); addEventListener("resize", onScroll); update(); });
  onBeforeUnmount(() => { removeEventListener("scroll", onScroll); removeEventListener("resize", onScroll); });
  return p;
}

/* ---- toasts */
export const toasts = reactive({ text: "", id: 0 });
let toastTimer = 0;
export function toast(text: string) {
  toasts.text = text;
  toasts.id++;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (toasts.text = ""), 2600);
}

/* ---- confetti: little drawn shapes in the page's colours, not emoji */
export type Bit = "cat" | "star" | "heart" | "paw" | "ring" | "letter" | "snack";
const BITS: Record<Exclude<Bit, "cat">, string> = {
  star: '<path d="M12 1.5l3.1 6.6 7.2.9-5.3 5 1.4 7.1L12 17.6 5.6 21.1 7 14 1.7 9l7.2-.9z" fill="currentColor"/>',
  heart: '<path d="M12 21s-8.5-5.3-8.5-11.2A4.8 4.8 0 0 1 12 6.6a4.8 4.8 0 0 1 8.5 3.2C20.5 15.7 12 21 12 21z" fill="currentColor"/>',
  paw: '<g fill="currentColor"><ellipse cx="12" cy="16" rx="5.2" ry="4.4"/><circle cx="5.2" cy="9.6" r="2.3"/><circle cx="9.3" cy="5.6" r="2.3"/><circle cx="14.7" cy="5.6" r="2.3"/><circle cx="18.8" cy="9.6" r="2.3"/></g>',
  ring: '<circle cx="12" cy="12" r="7.5" fill="none" stroke="currentColor" stroke-width="3.4"/>',
  letter: '<g fill="none" stroke="currentColor" stroke-width="2.4" stroke-linejoin="round"><rect x="2.5" y="5.5" width="19" height="13" rx="1.5"/><path d="M3 6.5l9 7 9-7"/></g>',
  snack: '<g><rect x="3" y="4" width="18" height="16" rx="3" fill="currentColor"/><path d="M3 10h18M3 15h18M9 4v16M15 4v16" stroke="#1d0f2e" stroke-opacity=".35" stroke-width="1.4"/></g>',
};
const INKS = ["var(--hot)", "var(--cream)", "var(--mint)", "var(--violet)", "#ff8fb8"];
export function confetti(kinds: Bit[], count = 22) {
  if (reducedMotion()) return;
  for (let i = 0; i < count; i++) {
    const kind = kinds[Math.floor(Math.random() * kinds.length)];
    const size = 14 + Math.random() * 16;
    let s: HTMLElement;
    if (kind === "cat") {
      const img = document.createElement("img");
      img.src = "/assets/art/mark.svg";
      img.alt = "";
      s = img;
    } else {
      s = document.createElement("span");
      s.innerHTML = `<svg viewBox="0 0 24 24" width="100%" height="100%">${BITS[kind]}</svg>`;
      s.style.color = INKS[i % INKS.length];
    }
    s.className = "confetti";
    s.setAttribute("aria-hidden", "true");
    s.style.left = `${Math.random() * 100}vw`;
    s.style.width = s.style.height = `${size}px`;
    document.body.append(s);
    const sway = (Math.random() - 0.5) * 30;
    const turn = (Math.random() - 0.5) * 540;
    s.animate(
      [
        { transform: "translate(0, 0) rotate(0deg)", opacity: 1 },
        { transform: `translate(${sway * 0.6}vw, 55vh) rotate(${turn * 0.5}deg)`, opacity: 1, offset: 0.55 },
        { transform: `translate(${sway}vw, ${108 + Math.random() * 14}vh) rotate(${turn}deg)`, opacity: 0 },
      ],
      { duration: 2000 + Math.random() * 1400, easing: "cubic-bezier(.25,.1,.5,1)" },
    ).onfinish = () => s.remove();
  }
}

/* ---- what the 3D world listens to */
export type Formation = "vortex" | "zeros" | "ribbon" | "rain" | "heart" | "trail" | "orbit" | "drift" | "wave" | "fireworks";
type Entry = { el: HTMLElement; formation: Formation; anchors?: () => HTMLElement[] };
export const world = {
  sections: [] as Entry[],
  /** Whatever the snacks should gather around right now, in screen pixels. */
  focus: { x: 0, y: 0 },
  boop: null as null | (() => void),
};

/** Tells the world this element is a section, and what shape the snacks take there. */
export function useFormation(el: Ref<HTMLElement | null>, formation: Formation, anchors?: () => HTMLElement[]) {
  onMounted(() => { if (el.value) world.sections.push({ el: el.value, formation, anchors }); });
  onBeforeUnmount(() => { world.sections = world.sections.filter((s) => s.el !== el.value); });
}

/** Buttons that lean toward the pointer. */
export function magnetic(node: HTMLElement) {
  if (!finePointer()) return;
  node.addEventListener("pointermove", (e) => {
    const r = node.getBoundingClientRect();
    node.style.setProperty("--mx", `${clamp((e.clientX - (r.left + r.width / 2)) * 0.25, -12, 12)}px`);
    node.style.setProperty("--my", `${clamp((e.clientY - (r.top + r.height / 2)) * 0.35, -9, 9)}px`);
  });
  node.addEventListener("pointerleave", () => { node.style.setProperty("--mx", "0px"); node.style.setProperty("--my", "0px"); });
}
export const vMagnetic = { mounted: magnetic };
