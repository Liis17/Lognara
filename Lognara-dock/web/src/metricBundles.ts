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

export function linuxHostDefinition(
  name: string,
  origin: "counter" | "gauge",
): PortableMetric {
  let title = name;
  let description = "Наблюдаемая метрика lognara-hwagent.";
  let unit = "";
  let kind: PortableMetric["kind"] = origin === "counter" ? "delta" : "gauge";
  let aggregation: PortableMetric["aggregation"] = origin === "counter" ? "sum" : "avg";

  const fixed: Record<string, [string, string, string]> = {
    "cpu.usage_percent": ["Загрузка CPU", "Доля занятого процессорного времени.", "%"],
    "cpu.iowait_percent": ["Ожидание ввода-вывода CPU", "Доля времени ожидания ввода-вывода.", "%"],
    "cpu.steal_percent": ["Steal CPU", "Доля времени, отданного гипервизору.", "%"],
    "cpu.cores": ["Ядра CPU", "Число процессорных ядер.", "ядер"],
    "load.1": ["Load average за 1 минуту", "Средняя нагрузка за 1 минуту.", ""],
    "load.5": ["Load average за 5 минут", "Средняя нагрузка за 5 минут.", ""],
    "load.15": ["Load average за 15 минут", "Средняя нагрузка за 15 минут.", ""],
    "memory.used_percent": ["Использование памяти", "Доля занятой оперативной памяти.", "%"],
    "memory.total_bytes": ["Всего памяти", "Общий объём оперативной памяти.", "байт"],
    "memory.available_bytes": ["Доступно памяти", "Объём доступной оперативной памяти.", "байт"],
    "swap.used_bytes": ["Использовано swap", "Объём занятого swap.", "байт"],
    "swap.used_percent": ["Использование swap", "Доля занятого swap.", "%"],
    "pressure.cpu.some_avg10": ["CPU pressure", "Время ожидания CPU за последние 10 секунд.", "%"],
    "pressure.memory.some_avg10": ["Memory pressure", "Время ожидания памяти за последние 10 секунд.", "%"],
    "pressure.io.some_avg10": ["I/O pressure", "Время ожидания ввода-вывода за последние 10 секунд.", "%"],
    "proc.count": ["Количество процессов", "Число работающих процессов.", "процессов"],
    "proc.top_cpu_percent": ["Максимальная загрузка процесса", "Загрузка CPU самым активным процессом.", "%"],
  };
  const known = fixed[name];
  if (known) [title, description, unit] = known;

  const fs = name.match(/^fs\.(.+)\.(free_bytes|used_percent)$/u);
  if (fs) {
    title = fs[2] === "free_bytes" ? `Свободное место: ${fs[1]}` : `Заполнение: ${fs[1]}`;
    description = `Метрика файловой системы ${fs[1]}.`;
    unit = fs[2] === "free_bytes" ? "байт" : "%";
  }
  const device = name.match(/^(disk|net)\.(.+)\.(read_bytes|write_bytes|rx_bytes|tx_bytes)$/u);
  if (device) {
    const direction = device[3] === "read_bytes" ? "чтение"
      : device[3] === "write_bytes" ? "запись"
        : device[3] === "rx_bytes" ? "приём" : "передача";
    title = `${direction[0].toLocaleUpperCase("ru-RU")}${direction.slice(1)} ${device[1] === "disk" ? "диска" : "сети"} ${device[2]}`;
    description = `Сумма байтов за интервалы, собранных для ${device[2]}.`;
    unit = "байт";
  }
  const gpu = name.match(/^gpu\.(.+)\.(util_percent|memory_used_bytes|memory_used_percent|temperature_c|power_w)$/u);
  if (gpu) {
    const labels: Record<string, [string, string]> = {
      util_percent: ["Загрузка", "%"],
      memory_used_bytes: ["Использовано памяти", "байт"],
      memory_used_percent: ["Использование памяти", "%"],
      temperature_c: ["Температура", "°C"],
      power_w: ["Потребление", "Вт"],
    };
    const [label, gpuUnit] = labels[gpu[2]];
    title = `${label} GPU ${gpu[1]}`;
    description = `Метрика видеокарты ${gpu[1]}.`;
    unit = gpuUnit;
  }
  if (name === "cpu.cores" || name === "proc.count") aggregation = "last";
  if (origin === "counter") description = `Счётчик hwagent; значения суммируются за интервал. ${description}`;

  return { name, title, description, unit, kind, aggregation, enabled: true, rule: null };
}
