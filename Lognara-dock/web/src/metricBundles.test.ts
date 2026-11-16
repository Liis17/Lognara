import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bundleOf, exampleBundle, linuxHostDefinition, nginxTemplates } from "./metricBundles.ts";

test("each nginx template matches its published example", () => {
  for (const template of nginxTemplates) {
    const regex = new RegExp(
      template.metric.rule!.pattern.replaceAll("(?P<", "(?<"),
    );
    assert.ok(regex.test(template.example), template.metric.name);
  }
});

test("downloadable bundles match the examples and templates used by Dock", () => {
  const read = (name: string) =>
    JSON.parse(
      readFileSync(new URL("../public/" + name, import.meta.url), "utf8"),
    );
  assert.deepEqual(read("metrics-example.json"), exampleBundle);
  assert.deepEqual(
    read("metrics-nginx.json"),
    bundleOf(nginxTemplates.map((t) => t.metric)),
  );
  const schema = read("metrics-schema.json");
  assert.equal(schema.properties.format.const, exampleBundle.format);
  assert.equal(schema.properties.version.const, exampleBundle.version);
});

test("Linux host definitions are rule-free and describe observed gauge/counter rows", () => {
  const gauge = linuxHostDefinition("cpu.usage_percent", "gauge");
  assert.deepEqual(
    { title: gauge.title, unit: gauge.unit, kind: gauge.kind, aggregation: gauge.aggregation, rule: gauge.rule },
    { title: "Загрузка CPU", unit: "%", kind: "gauge", aggregation: "avg", rule: null },
  );
  const counter = linuxHostDefinition("disk.sda.read_bytes", "counter");
  assert.deepEqual(
    { title: counter.title, unit: counter.unit, kind: counter.kind, aggregation: counter.aggregation, rule: counter.rule },
    { title: "Чтение диска sda", unit: "байт", kind: "delta", aggregation: "sum", rule: null },
  );
});
