#!/usr/bin/env bun
/**
 * Sync the canonical label set to Forgejo or Gitea.
 *
 * GitHub reads .github/labels.yml through the label workflow. Forgejo does not,
 * so this script pushes the same manifest through its API.
 *
 * Usage:
 *   bun run scripts/sync-labels-forgejo.ts \
 *     --url https://git.vai-rice.space \
 *     --repo VAI_PROG/NotAnOtherVTUBERwiki \
 *     --token "$FORGEJO_TOKEN"
 *
 * Add --dry-run to print changes without writing them. Add --prune only when
 * you explicitly want labels absent from the manifest deleted.
 */

type Label = {
  name: string;
  color: string;
  description: string;
};

type ExistingLabel = Label & { id?: number };

const root = new URL("..", import.meta.url);
const manifestPath = fileURLToPath(new URL(".github/labels.yml", root));

function fileURLToPath(url: URL): string {
  return decodeURIComponent(url.pathname).replace(/^\/(\w):/, "$1:");
}

function printHelp(): void {
  console.log(`Usage: bun run scripts/sync-labels-forgejo.ts --url URL --repo OWNER/REPO --token TOKEN [options]

Options:
  --manifest PATH  Label manifest, defaults to .github/labels.yml
  --dry-run        Print changes without writing them
  --prune          Delete labels absent from the manifest
  --help           Show this help

Environment alternatives: FORGEJO_URL, FORGEJO_REPO, FORGEJO_TOKEN
`);
}

function parseArgs(): {
  url: string;
  repo: string;
  token: string;
  manifest: string;
  dryRun: boolean;
  prune: boolean;
} {
  if (Bun.argv.includes("--help") || Bun.argv.includes("-h")) {
    printHelp();
    process.exit(0);
  }

  const values = new Map<string, string>();
  let dryRun = false;
  let prune = false;

  for (let i = 2; i < Bun.argv.length; i += 1) {
    const arg = Bun.argv[i];
    if (arg === "--dry-run") {
      dryRun = true;
      continue;
    }
    if (arg === "--prune") {
      prune = true;
      continue;
    }
    if (!arg.startsWith("--")) {
      throw new Error(`unexpected argument: ${arg}`);
    }
    const key = arg.slice(2);
    const value = Bun.argv[++i];
    if (!value || value.startsWith("--")) {
      throw new Error(`missing value for --${key}`);
    }
    values.set(key, value);
  }

  const required = (key: string, env: string): string => {
    const value = values.get(key) ?? Bun.env[env];
    if (!value) throw new Error(`--${key} or ${env} is required`);
    return value;
  };

  return {
    url: required("url", "FORGEJO_URL").replace(/\/$/, ""),
    repo: required("repo", "FORGEJO_REPO"),
    token: required("token", "FORGEJO_TOKEN"),
    manifest: values.get("manifest") ?? manifestPath,
    dryRun,
    prune,
  };
}

function unquote(value: string): string {
  const trimmed = value.trim();
  if (trimmed.length >= 2 && "\"'".includes(trimmed[0]) && trimmed.at(-1) === trimmed[0]) {
    return trimmed.slice(1, -1);
  }
  return trimmed;
}

async function parseManifest(path: string): Promise<Label[]> {
  const source = Bun.file(path);
  const lines = await source.text();
  if (!lines.trim()) throw new Error(`manifest not found or empty: ${path}`);
  const labels: Label[] = [];
  let current: Partial<Label> = {};

  const flush = () => {
    if (!current.name && !current.color && !current.description) return;
    if (!current.name || !current.color) {
      throw new Error(`label entry is missing name or color: ${JSON.stringify(current)}`);
    }
    if (!/^[0-9a-fA-F]{6}$/.test(current.color)) {
      throw new Error(`label color must be six hex digits: ${current.name}`);
    }
    labels.push({
      name: current.name,
      color: current.color,
      description: current.description ?? "",
    });
    current = {};
  };

  for (const raw of lines.split(/\r?\n/)) {
    const line = raw.trim();
    if (!line || line.startsWith("#") || line === "---") continue;
    if (line.startsWith("- ")) {
      flush();
      const first = line.slice(2).trim();
      if (first) {
        const separator = first.indexOf(":");
        if (separator < 0) throw new Error(`invalid manifest line: ${raw}`);
        const key = first.slice(0, separator).trim() as keyof Label;
        current[key] = unquote(first.slice(separator + 1)) as never;
      }
      continue;
    }
    const separator = line.indexOf(":");
    if (separator < 0) continue;
    const key = line.slice(0, separator).trim() as keyof Label;
    current[key] = unquote(line.slice(separator + 1)) as never;
  }
  flush();

  const names = new Set<string>();
  for (const label of labels) {
    if (names.has(label.name)) throw new Error(`duplicate label: ${label.name}`);
    names.add(label.name);
  }
  return labels;
}

class ForgejoClient {
  constructor(private readonly base: string, private readonly token: string) {}

  private async request<T>(method: string, path: string, body?: unknown): Promise<T> {
    const response = await fetch(`${this.base}/api/v1/${path.replace(/^\//, "")}`, {
      method,
      headers: {
        Accept: "application/json",
        Authorization: `token ${this.token}`,
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    const text = await response.text();
    if (!response.ok) {
      throw new Error(`${method} ${path} failed: ${response.status} ${text.slice(0, 400)}`);
    }
    return (text ? JSON.parse(text) : undefined) as T;
  }

  async listLabels(repo: string): Promise<ExistingLabel[]> {
    const result: ExistingLabel[] = [];
    for (let page = 1; ; page += 1) {
      const batch = await this.request<ExistingLabel[]>(
        "GET",
        `repos/${repo}/labels?page=${page}`,
      );
      if (!batch.length) return result;
      result.push(...batch);
      if (batch.length < 50) return result;
    }
  }

  create(repo: string, label: Label): Promise<unknown> {
    return this.request("POST", `repos/${repo}/labels`, label);
  }

  update(repo: string, name: string, label: Label): Promise<unknown> {
    return this.request("PATCH", `repos/${repo}/labels/${encodeURIComponent(name)}`, label);
  }

  delete(repo: string, name: string): Promise<unknown> {
    return this.request("DELETE", `repos/${repo}/labels/${encodeURIComponent(name)}`);
  }
}

async function main(): Promise<void> {
  const args = parseArgs();
  const wanted = await parseManifest(args.manifest);
  const client = new ForgejoClient(args.url, args.token);
  const existing = new Map((await client.listLabels(args.repo)).map((label) => [label.name, label]));

  let created = 0;
  let updated = 0;
  let deleted = 0;
  let unchanged = 0;

  for (const label of wanted) {
    const current = existing.get(label.name);
    if (!current) {
      console.log(`create  ${label.name.padEnd(18)} ${label.color}`);
      if (!args.dryRun) await client.create(args.repo, label);
      created += 1;
      continue;
    }

    const same = current.color.toLowerCase() === label.color.toLowerCase()
      && (current.description ?? "") === label.description;
    if (same) {
      unchanged += 1;
      continue;
    }

    console.log(`update  ${label.name.padEnd(18)} ${label.color}`);
    if (!args.dryRun) await client.update(args.repo, label.name, label);
    updated += 1;
  }

  if (args.prune) {
    const wantedNames = new Set(wanted.map((label) => label.name));
    for (const name of [...existing.keys()].sort()) {
      if (wantedNames.has(name)) continue;
      console.log(`delete  ${name}`);
      if (!args.dryRun) await client.delete(args.repo, name);
      deleted += 1;
    }
  }

  const prefix = args.dryRun ? "dry run: " : "";
  console.log(`${prefix}created ${created}, updated ${updated}, deleted ${deleted}, unchanged ${unchanged}`);
}

main().catch((error: unknown) => {
  console.error(`error: ${error instanceof Error ? error.message : String(error)}`);
  process.exit(1);
});
