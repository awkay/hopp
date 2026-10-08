import changelogText from "../../../CHANGELOG.md?raw";
import { compareVersions } from "@/lib/semver";

export type ChangelogSection = {
  version: string;
  /** `YYYY-MM-DD`, or "" when the heading has no date. */
  date: string;
  /** The section's Markdown, without its heading. */
  body: string;
};

// "## 1.0.35 (2026-10-08)", as the release task writes it. The date is optional.
const VERSION_HEADING = /^##\s+v?(\d+(?:\.\d+)*(?:-[0-9A-Za-z.-]+)?)(?:\s+\((\d{4}-\d{2}-\d{2})\))?$/;

/**
 * Splits `CHANGELOG.md` into one section per `## <version> (<date>)` heading, newest first.
 * The title, the intro and any `##` section that isn't a version are skipped.
 */
export function parseChangelog(text: string): ChangelogSection[] {
  const sections: ChangelogSection[] = [];
  let current: { version: string; date: string; lines: string[] } | null = null;
  let inFence = false;

  const flush = () => {
    if (current) {
      const body = current.lines
        .join("\n")
        .replace(/^\s*\n/, "")
        .trimEnd();
      sections.push({ version: current.version, date: current.date, body });
    }
    current = null;
  };

  for (const rawLine of text.replace(/^\uFEFF/, "").split(/\r?\n/)) {
    const line = rawLine.trimEnd();
    if (/^\s*(```|~~~)/.test(line)) {
      inFence = !inFence;
    } else if (!inFence && /^##\s/.test(line)) {
      flush();
      const match = VERSION_HEADING.exec(line);
      if (match?.[1]) current = { version: match[1], date: match[2] ?? "", lines: [] };
      continue;
    }
    current?.lines.push(line);
  }
  flush();

  // Stable sort: the file is already newest first, this only repairs a misplaced section.
  return sections.sort((a, b) => compareVersions(b.version, a.version));
}

export const changelogSections = parseChangelog(changelogText);
