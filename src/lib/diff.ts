// Line diff.
//
// Common prefix and suffix are trimmed first, which reduces almost every real
// edit to a handful of lines. Only what is left goes through an LCS table, and
// that is capped so a pathological pair of large files degrades to a coarse
// block diff instead of allocating a gigabyte.

export type Op = "equal" | "insert" | "delete";

export interface DiffLine {
  op: Op;
  text: string;
  oldNo: number | null;
  newNo: number | null;
}

export interface Hunk {
  oldStart: number;
  newStart: number;
  lines: DiffLine[];
}

const LCS_CELL_LIMIT = 4_000_000; // ~16 MB of Uint32, the point of diminishing returns

export function diffLines(before: string, after: string): DiffLine[] {
  const a = before.length ? before.split("\n") : [];
  const b = after.length ? after.split("\n") : [];

  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;

  let tail = 0;
  while (
    tail < a.length - head &&
    tail < b.length - head &&
    a[a.length - 1 - tail] === b[b.length - 1 - tail]
  )
    tail++;

  const midA = a.slice(head, a.length - tail);
  const midB = b.slice(head, b.length - tail);

  const out: DiffLine[] = [];
  for (let i = 0; i < head; i++) out.push({ op: "equal", text: a[i]!, oldNo: i + 1, newNo: i + 1 });

  const middle =
    midA.length * midB.length > LCS_CELL_LIMIT ? coarse(midA, midB, head) : lcsDiff(midA, midB, head);
  out.push(...middle);

  for (let i = 0; i < tail; i++) {
    const oldNo = a.length - tail + i + 1;
    const newNo = b.length - tail + i + 1;
    out.push({ op: "equal", text: a[oldNo - 1]!, oldNo, newNo });
  }
  return out;
}

function lcsDiff(a: string[], b: string[], offset: number): DiffLine[] {
  const n = a.length;
  const m = b.length;
  if (n === 0 && m === 0) return [];
  if (n === 0)
    return b.map((text, i) => ({ op: "insert" as const, text, oldNo: null, newNo: offset + i + 1 }));
  if (m === 0)
    return a.map((text, i) => ({ op: "delete" as const, text, oldNo: offset + i + 1, newNo: null }));

  const width = m + 1;
  const table = new Uint32Array((n + 1) * width);
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      table[i * width + j] =
        a[i] === b[j]
          ? table[(i + 1) * width + (j + 1)]! + 1
          : Math.max(table[(i + 1) * width + j]!, table[i * width + (j + 1)]!);
    }
  }

  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ op: "equal", text: a[i]!, oldNo: offset + i + 1, newNo: offset + j + 1 });
      i++;
      j++;
    } else if (table[(i + 1) * width + j]! >= table[i * width + (j + 1)]!) {
      out.push({ op: "delete", text: a[i]!, oldNo: offset + i + 1, newNo: null });
      i++;
    } else {
      out.push({ op: "insert", text: b[j]!, oldNo: null, newNo: offset + j + 1 });
      j++;
    }
  }
  while (i < n) {
    out.push({ op: "delete", text: a[i]!, oldNo: offset + i + 1, newNo: null });
    i++;
  }
  while (j < m) {
    out.push({ op: "insert", text: b[j]!, oldNo: null, newNo: offset + j + 1 });
    j++;
  }
  return out;
}

/** Fallback for very large files: one delete block followed by one insert block. */
function coarse(a: string[], b: string[], offset: number): DiffLine[] {
  return [
    ...a.map((text, i) => ({ op: "delete" as const, text, oldNo: offset + i + 1, newNo: null })),
    ...b.map((text, i) => ({ op: "insert" as const, text, oldNo: null, newNo: offset + i + 1 })),
  ];
}

/** Groups a diff into hunks with `context` unchanged lines around each change. */
export function toHunks(lines: DiffLine[], context = 3): Hunk[] {
  const changed = lines.map((l) => l.op !== "equal");
  const keep = new Array<boolean>(lines.length).fill(false);
  for (let i = 0; i < lines.length; i++) {
    if (!changed[i]) continue;
    for (let j = Math.max(0, i - context); j <= Math.min(lines.length - 1, i + context); j++) keep[j] = true;
  }

  const hunks: Hunk[] = [];
  let current: Hunk | null = null;
  for (let i = 0; i < lines.length; i++) {
    if (!keep[i]) {
      current = null;
      continue;
    }
    const line = lines[i]!;
    if (!current) {
      current = { oldStart: line.oldNo ?? 0, newStart: line.newNo ?? 0, lines: [] };
      hunks.push(current);
    }
    current.lines.push(line);
  }
  return hunks;
}

export function diffStats(lines: DiffLine[]): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const l of lines) {
    if (l.op === "insert") added++;
    else if (l.op === "delete") removed++;
  }
  return { added, removed };
}
