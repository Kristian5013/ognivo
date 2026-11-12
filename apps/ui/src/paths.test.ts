import { describe, expect, it } from "vitest";

import { basename, relativeTo } from "./paths.ts";

describe("paths", () => {
  it("are relative to a Linux project inside it, and whole outside it", () => {
    const project = "/home/me/Рабочий стол/site";
    expect(relativeTo(project, "/home/me/Рабочий стол/site/components/Header.tsx")).toBe("components/Header.tsx");
    expect(relativeTo(`${project}/`, `${project}/a.ts`)).toBe("a.ts");
    expect(relativeTo(project, "/home/me/Рабочий стол/site2/a.ts")).toBe("/home/me/Рабочий стол/site2/a.ts");
    expect(relativeTo(project, "/home/me/Рабочий стол/SITE/a.ts")).toBe("/home/me/Рабочий стол/SITE/a.ts");
    expect(relativeTo("", "/a/b")).toBe("/a/b");
  });

  it("are relative to a Windows project in either separator and any case", () => {
    const project = "C:\\Users\\me\\site";
    expect(relativeTo(project, "C:\\Users\\me\\site\\components\\Header.tsx")).toBe("components\\Header.tsx");
    expect(relativeTo(project, "c:/users/ME/site/a.ts")).toBe("a.ts");
    expect(relativeTo(project, "C:\\Users\\me\\site2\\a.ts")).toBe("C:\\Users\\me\\site2\\a.ts");
    expect(relativeTo("\\\\server\\share\\site", "\\\\server\\share\\site\\a.ts")).toBe("a.ts");
  });

  it("end in their last part", () => {
    expect(basename("/home/me/site/")).toBe("site");
    expect(basename("C:\\Users\\me\\site")).toBe("site");
    expect(basename("site")).toBe("site");
  });
});
