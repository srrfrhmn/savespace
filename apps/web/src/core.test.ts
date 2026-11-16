// Cross-platform check (design doc §8.4): the WASM build of savespace-core must
// produce exactly the strings in the golden file that the native Rust test uses
// (crates/core/tests/golden.rs).

import { readFileSync } from "node:fs";
import initCore, { calmState, formatMoney, parseMoney } from "@savespace/savespace-core-wasm";
import { beforeAll, describe, expect, it } from "vitest";
import calmGolden from "../../../crates/core/tests/golden/calm_state.json" with { type: "json" };
import golden from "../../../crates/core/tests/golden/format.json" with { type: "json" };
import parseGolden from "../../../crates/core/tests/golden/parse_input.json" with { type: "json" };

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

describe("parseMoney (WASM) matches the golden file", () => {
  for (const c of parseGolden.cases) {
    it(`${JSON.stringify(c.text)} ${c.currency} -> ${c.expected}`, () => {
      if (c.expected === null) {
        expect(() => parseMoney(c.text, c.currency)).toThrow();
      } else {
        expect(parseMoney(c.text, c.currency)).toBe(BigInt(c.expected));
      }
    });
  }
});

describe("calmState (WASM) matches the golden file", () => {
  for (const c of calmGolden.cases) {
    it(`${c.cash} ${c.bills} ${c.flex_left} -> ${c.expected}`, () => {
      expect(calmState(BigInt(c.cash), BigInt(c.bills), BigInt(c.flex_left), c.currency)).toBe(
        c.expected,
      );
    });
  }
});
