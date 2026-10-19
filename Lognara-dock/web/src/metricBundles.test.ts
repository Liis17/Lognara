import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { bundleOf, exampleBundle, nginxTemplates } from "./metricBundles.ts";

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
