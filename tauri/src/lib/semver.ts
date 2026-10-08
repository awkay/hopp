/** Compares dotted numeric versions ("1.0.35"): negative, 0 or positive. Prerelease suffixes are ignored. */
export function compareVersions(a: string, b: string): number {
  const pa = (a.split("-")[0] ?? "").split(".");
  const pb = (b.split("-")[0] ?? "").split(".");
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (Number(pa[i]) || 0) - (Number(pb[i]) || 0);
    if (d !== 0) return Math.sign(d);
  }
  return 0;
}
