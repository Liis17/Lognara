export function infrastructureVersion(
  releaseVersion: string | null | undefined,
  packageVersion: string | null | undefined,
  gitCommit?: string | null,
): string {
  const release = releaseVersion?.trim();
  if (release) return `v${release}`;
  if (!packageVersion) return "версия неизвестна";
  const commit = gitCommit?.trim().slice(0, 7);
  return `v${packageVersion}${commit ? `+${commit}` : ""}`;
}
