import { test } from "node:test";
import assert from "node:assert/strict";
import { infrastructureVersion } from "./infrastructureVersion.ts";

test("infrastructure prefers the release tag and falls back to package version plus short SHA", () => {
  assert.equal(infrastructureVersion("dev-abcdef012345", "0.1.0", "1234567890"), "vdev-abcdef012345");
  assert.equal(infrastructureVersion(null, "0.1.0", "1234567890"), "v0.1.0+1234567");
  assert.equal(infrastructureVersion("", "0.1.0"), "v0.1.0");
  assert.equal(infrastructureVersion(null, null), "версия неизвестна");
});
