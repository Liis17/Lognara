import assert from "node:assert/strict";
import test from "node:test";
import { filterServiceOptions, serviceMatch, type ServiceOption } from "./components/serviceComboboxLogic.ts";

const options: ServiceOption[] = [
  { value: "billing" },
  { value: "gateway", secondary: "server-a" },
  { value: "", label: "Пустое имя" },
];

test("service suggestions match substrings case-insensitively in primary and secondary labels", () => {
  assert.deepEqual(filterServiceOptions(options, "ILL"), [options[0]]);
  assert.deepEqual(filterServiceOptions(options, "SERVER-A"), [options[1]]);
});

test("service match splits the label for highlighted rendering", () => {
  assert.deepEqual(serviceMatch("BarkCloud.Configuration", "cloud"), {
    before: "Bark", match: "Cloud", after: ".Configuration",
  });
  assert.equal(serviceMatch("billing", "missing"), null);
  assert.deepEqual(serviceMatch("server-a", "server"), { before: "", match: "server", after: "-a" });
});

