import { describe, expect, it } from "vitest";
import { currencyForZone } from "./session";

describe("currencyForZone", () => {
  it("uses CAD in Canada and USD elsewhere", () => {
    expect(currencyForZone("America/Toronto")).toBe("CAD");
    expect(currencyForZone("America/Vancouver")).toBe("CAD");
    expect(currencyForZone("America/St_Johns")).toBe("CAD");
    expect(currencyForZone("America/New_York")).toBe("USD");
    expect(currencyForZone("America/Detroit")).toBe("USD");
    expect(currencyForZone("Europe/Paris")).toBe("USD");
  });
});
