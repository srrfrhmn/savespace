// Cross-platform check (design doc §8.4): the WASM build of savespace-core must
// produce exactly the strings in the golden file that the native Rust test uses
// (crates/core/tests/golden.rs).

import { readFileSync } from "node:fs";
import initCore, { formatMoney } from "@savespace/savespace-core-wasm";
import { beforeAll, describe, expect, it } from "vitest";
import golden from "../../../crates/core/tests/golden/format.json" with { type: "json" };

beforeAll(async () => {
  const wasmUrl = new URL("../../../bindings/wasm/pkg/savespace_core_bg.wasm", import.meta.url);
  await initCore({ module_or_path: readFileSync(wasmUrl) });
});

describe("formatMoney (WASM) matches the golden file", () => {
  it("has cases", () => {
    expect(golden.cases.length).toBeGreaterThan(0);
  });

  for (const c of golden.cases) {
    it(`${c.minor} ${c.currency} -> ${c.expected}`, () => {
      expect(formatMoney(BigInt(c.minor), c.currency)).toBe(c.expected);
    });
  }

  it("rejects an unsupported currency", () => {
    expect(() => formatMoney(1n, "XYZ")).toThrow(/unsupported currency/);
  });
});
