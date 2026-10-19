import type { DefinitionInput } from "./api";

export type PortableMetric = Omit<DefinitionInput, "server" | "service">;
export interface MetricBundle {
  format: "lognara-metrics";
  version: 1;
  metrics: PortableMetric[];
}
export interface MetricTemplate {
  metric: PortableMetric;
  example: string;
}
export type ImportAction =
  | { action: "create" | "skip" | "replace" }
  | { action: "rename"; name: string };

const access = String.raw`^\S+ - \S+ \[[^\]\r\n]+\] "(?:[^"\\\r\n]|\\.)*" `;
const tail = String.raw` (?:\d+|-) "(?:[^"\\\r\n]|\\.)*" "(?:[^"\\\r\n]|\\.)*"$`;
const accessExample =
  '2001:db8::1 - - [19/Oct/2026:12:00:00 +0000] "GET / HTTP/1.1" 200 512 "-" "curl/8.0"';

function template(
  name: string,
  title: string,
  description: string,
  pattern: string,
  unit = "запросов",
  value: string | null = null,
  labels: string[] = [],
): MetricTemplate {
  return {
    example:
      name === "nginx.responses_4xx"
        ? accessExample.replace(" 200 ", " 404 ")
        : name === "nginx.responses_5xx"
          ? accessExample.replace(" 200 ", " 500 ")
          : accessExample,
    metric: {
      name,
      title,
      description,
      unit,
      kind: "delta",
      aggregation: "sum",
      enabled: true,
      rule: {
        environment: null,
        contains: '] "',
        pattern,
        value_group: value,
        label_groups: labels,
      },
    },
  };
}

export const nginxTemplates: MetricTemplate[] = [
  template(
    "nginx.requests",
    "Запросы nginx",
    "Количество записей access-лога в стандартном формате combined.",
    access + String.raw`[1-5]\d{2}` + tail,
  ),
  template(
    "nginx.requests_by_status",
    "HTTP-статусы",
    "Количество запросов с разбиением по HTTP-статусу.",
    access + String.raw`(?P<status>[1-5]\d{2})` + tail,
    "запросов",
    null,
    ["status"],
  ),
  template(
    "nginx.responses_4xx",
    "Ошибки HTTP 4xx",
    "Запросы с клиентскими ошибками 400–499.",
    access + String.raw`4\d{2}` + tail,
  ),
  template(
    "nginx.responses_5xx",
    "Ошибки HTTP 5xx",
    "Запросы с серверными ошибками 500–599.",
    access + String.raw`5\d{2}` + tail,
  ),
  template(
    "nginx.body_bytes_sent",
    "Объём ответов",
    "Суммарный размер тела ответов body_bytes_sent, без HTTP-заголовков.",
    access +
      String.raw`[1-5]\d{2} (?P<bytes>\d+) "(?:[^"\\\r\n]|\\.)*" "(?:[^"\\\r\n]|\\.)*"$`,
    "байт",
    "bytes",
  ),
  {
    example: "2026/10/19 12:00:00 [error] 123#123: *42 upstream timed out",
    metric: {
      name: "nginx.error_log",
      title: "Записи error-лога",
      description:
        "Количество записей стандартного error-лога по уровню severity; доступность уровней зависит от настройки error_log.",
      unit: "записей",
      kind: "delta",
      aggregation: "sum",
      enabled: true,
      rule: {
        environment: null,
        contains: "] ",
        pattern: String.raw`^\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2} \[(?P<severity>emerg|alert|crit|error|warn|notice|info|debug)\] \d+#\d+: .+$`,
        value_group: null,
        label_groups: ["severity"],
      },
    },
  },
];

export const exampleBundle: MetricBundle = {
  format: "lognara-metrics",
  version: 1,
  metrics: [
    {
      name: "requests.completed",
      title: "Завершённые запросы",
      description: "Число сообщений request completed по HTTP-статусу.",
      unit: "запросов",
      kind: "delta",
      aggregation: "sum",
      enabled: true,
      rule: {
        environment: null,
        contains: "request completed",
        pattern: String.raw`request completed status=(?P<status>[1-5]\d{2})`,
        value_group: null,
        label_groups: ["status"],
      },
    },
  ],
};

export function bundleOf(metrics: PortableMetric[]): MetricBundle {
  return { format: "lognara-metrics", version: 1, metrics };
}
