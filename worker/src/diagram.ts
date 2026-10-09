// Diagram rendering: Markdown fences to SVG, off the request path.
//
// mermaid lays text out with a real browser, so it runs in a headless Chrome
// through Bun.WebView; graphviz is WebAssembly and runs in process. The engine
// sanitizes whatever comes back, so this file only has to produce a drawing,
// not a safe one. The browser still gets no network: every host resolves to
// nothing, and mermaid itself is injected from node_modules.

import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { instance as vizInstance } from "@viz-js/viz";

export type Lang = "mermaid" | "dot";
export type Theme = "light" | "dark";

export interface Drawing {
  svg: string;
}

export class DiagramError extends Error {}

/// Largest diagram source accepted, in bytes.
export const SOURCE_MAX = 50_000;
/// Largest drawing returned, in bytes.
export const SVG_MAX = 2_000_000;
/// A render that takes longer is abandoned and its browser restarted.
const RENDER_TIMEOUT_MS = 15_000;

const MERMAID_JS = new URL("../node_modules/mermaid/dist/mermaid.min.js", import.meta.url);

export async function render(lang: Lang, theme: Theme, source: string): Promise<Drawing> {
  if (new TextEncoder().encode(source).length > SOURCE_MAX) {
    throw new DiagramError(`the diagram is longer than ${SOURCE_MAX} bytes`);
  }
  const svg = lang === "mermaid" ? await mermaid.render(theme, source) : await dot(theme, source);
  if (svg.length > SVG_MAX) {
    throw new DiagramError("the drawing is too large");
  }
  return { svg };
}

/// Whether a browser for mermaid could be started, for /health.
export function mermaidReady(): boolean {
  return mermaid.ready;
}

// ---------------------------------------------------------------- graphviz

let viz: Awaited<ReturnType<typeof vizInstance>> | undefined;

async function dot(theme: Theme, source: string): Promise<string> {
  viz ??= await vizInstance();
  const ink = theme === "dark" ? "#e8e6f0" : "#1f1d2b";
  const result = viz.render(source, {
    format: "svg",
    engine: "dot",
    graphAttributes: { bgcolor: "transparent", color: ink, fontcolor: ink, fontname: "sans-serif" },
    nodeAttributes: { color: ink, fontcolor: ink, fontname: "sans-serif" },
    edgeAttributes: { color: ink, fontcolor: ink, fontname: "sans-serif" },
  });
  if (result.status !== "success") {
    const message = result.errors.map((e) => e.message).join("; ") || "graphviz could not draw it";
    throw new DiagramError(message);
  }
  return result.output;
}

// ----------------------------------------------------------------- mermaid

class Mermaid {
  private view: Bun.WebView | undefined;
  private queue: Promise<unknown> = Promise.resolve();
  private counter = 0;
  ready = false;

  /// Renders one diagram. Calls run one at a time: a view takes one evaluate.
  render(theme: Theme, source: string): Promise<string> {
    const run = this.queue.then(() => this.renderNow(theme, source));
    this.queue = run.catch(() => {});
    return run;
  }

  private async renderNow(theme: Theme, source: string): Promise<string> {
    const view = await this.open();
    const id = `d${++this.counter}`;
    const script = `(async () => {
      const config = {
        startOnLoad: false,
        securityLevel: "strict",
        theme: ${JSON.stringify(theme === "dark" ? "dark" : "default")},
        htmlLabels: false,
        flowchart: { htmlLabels: false },
        maxTextSize: ${SOURCE_MAX},
        maxEdges: 500,
        fontFamily: "sans-serif",
        themeVariables: { background: "transparent" },
      };
      mermaid.initialize(config);
      try {
        const { svg } = await mermaid.render(${JSON.stringify(id)}, ${JSON.stringify(source)});
        return { svg };
      } catch (err) {
        return { error: String(err && err.message ? err.message : err) };
      } finally {
        document.getElementById(${JSON.stringify(id)})?.remove();
        document.getElementById(${JSON.stringify("d" + id)})?.remove();
      }
    })()`;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new DiagramError("the diagram took too long to draw")), RENDER_TIMEOUT_MS);
    });
    try {
      const result = (await Promise.race([view.evaluate(script), timeout])) as
        | { svg: string }
        | { error: string };
      if ("error" in result) {
        throw new DiagramError(result.error.slice(0, 500));
      }
      return result.svg;
    } catch (err) {
      if (!(err instanceof DiagramError) || err.message.includes("too long")) {
        // A hung or crashed page: start over with a fresh browser.
        this.close();
      }
      throw err;
    } finally {
      clearTimeout(timer);
    }
  }

  private async open(): Promise<Bun.WebView> {
    if (this.view) {
      return this.view;
    }
    const url = await this.chrome.start();
    const view = new Bun.WebView({
      width: 1200,
      height: 800,
      backend: { type: "chrome", url },
    });
    try {
      await view.navigate("about:blank");
      const code = await Bun.file(MERMAID_JS).text();
      const loaded = await view.evaluate(`(() => {
        const s = document.createElement("script");
        s.textContent = ${JSON.stringify(code)};
        document.head.appendChild(s);
        return typeof mermaid;
      })()`);
      if (loaded !== "object") {
        throw new Error(`mermaid did not load (${String(loaded)})`);
      }
    } catch (err) {
      view.close();
      this.ready = false;
      throw err;
    }
    this.view = view;
    this.ready = true;
    return view;
  }

  private close() {
    try {
      this.view?.close();
    } catch {}
    this.view = undefined;
    this.ready = false;
    this.chrome.stop();
  }

  private chrome = new Chrome();
}

/// A headless Chrome this worker starts and owns. Bun.WebView connects to it
/// over its DevTools socket: Bun's own pipe launch does not work on Windows,
/// and owning the process means the flags are ours and a restart is a kill.
class Chrome {
  private proc: ReturnType<typeof Bun.spawn> | undefined;
  private dir: string | undefined;

  /// Starts Chrome and returns its DevTools URL. On Windows a first launch
  /// sometimes exits at once with status 0, so an early exit is retried.
  async start(): Promise<string> {
    const binary = chromePath();
    for (let attempt = 0; attempt < 3; attempt++) {
      const url = await this.launch(binary);
      if (url) {
        return url;
      }
    }
    throw new Error(`Chrome did not start (${binary})`);
  }

  private async launch(binary: string): Promise<string | undefined> {
    this.stop();
    const dir = mkdtempSync(join(tmpdir(), "naw-chrome-"));
    this.dir = dir;
    this.proc = Bun.spawn(
      [
        binary,
        "--headless=new",
        "--remote-debugging-port=0",
        `--user-data-dir=${dir}`,
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-gpu",
        "--disable-extensions",
        "--disable-background-networking",
        "--disable-sync",
        "--mute-audio",
        // No network at all: every name resolves to nothing.
        "--host-resolver-rules=MAP * ~NOTFOUND",
        // Chrome refuses its sandbox as root, which is how a container runs.
        ...(process.getuid?.() === 0 ? ["--no-sandbox"] : []),
        "about:blank",
      ],
      { stdout: "ignore", stderr: "ignore" },
    );
    const portFile = join(dir, "DevToolsActivePort");
    for (let i = 0; i < 150; i++) {
      if (existsSync(portFile)) {
        const [port, path] = readFileSync(portFile, "utf8").trim().split("\n");
        if (port && path) {
          return `ws://127.0.0.1:${port}${path}`;
        }
      }
      if (this.proc.exitCode !== null) {
        break;
      }
      await Bun.sleep(100);
    }
    this.stop();
    return undefined;
  }

  stop() {
    this.proc?.kill();
    this.proc = undefined;
    if (this.dir) {
      const dir = this.dir;
      this.dir = undefined;
      // Chrome holds its profile for a moment after the kill.
      const remove = (tries: number) => {
        try {
          rmSync(dir, { recursive: true, force: true });
        } catch {
          if (tries > 0) setTimeout(() => remove(tries - 1), 2000);
        }
      };
      setTimeout(() => remove(5), 2000);
    }
  }
}

/// `CHROME_PATH`, else the usual places on Linux, macOS and Windows.
function chromePath(): string {
  const fromEnv = process.env.CHROME_PATH;
  if (fromEnv) {
    return fromEnv;
  }
  const candidates =
    process.platform === "win32"
      ? [
          "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
          "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
        ]
      : process.platform === "darwin"
        ? ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome", "/Applications/Chromium.app/Contents/MacOS/Chromium"]
        : [];
  for (const path of candidates) {
    if (existsSync(path)) {
      return path;
    }
  }
  for (const name of ["chromium", "chromium-browser", "google-chrome-stable", "google-chrome"]) {
    const found = Bun.which(name);
    if (found) {
      return found;
    }
  }
  throw new Error("no Chrome or Chromium found; set CHROME_PATH");
}

const mermaid = new Mermaid();
