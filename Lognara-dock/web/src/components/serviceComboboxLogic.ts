export interface ServiceOption {
  value: string;
  label?: string;
  secondary?: string;
}

export function serviceOptionLabel(option: ServiceOption): string {
  return option.label ?? (option.value === "" ? "Пустое имя" : option.value);
}

export function filterServiceOptions(options: ServiceOption[], query: string): ServiceOption[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return options;
  return options.filter((option) =>
    `${serviceOptionLabel(option)} ${option.secondary ?? ""}`.toLocaleLowerCase().includes(needle),
  );
}

export function serviceMatch(label: string, query: string): { before: string; match: string; after: string } | null {
  const needle = query.trim();
  if (!needle) return null;
  const index = label.toLocaleLowerCase().indexOf(needle.toLocaleLowerCase());
  return index < 0 ? null : {
    before: label.slice(0, index),
    match: label.slice(index, index + needle.length),
    after: label.slice(index + needle.length),
  };
}
