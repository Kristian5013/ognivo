import { describe, expect, it } from "vitest";

import { code, literal, printable, respelled, samePath, spellingsOf, within } from "./text.ts";

describe("paths as one spelling", () => {
  it("takes a drive's root in one case, and what is under it as inside it", () => {
    expect(samePath("C:\\")).toBe(samePath("c:\\"));
    expect(samePath("C:")).toBe("c:");
    expect(within("D:\\a.ts", "D:\\")).toBe(true);
    expect(within("d:\\src\\a.ts", "D:\\")).toBe(true);
    expect(within("D:\\", "d:\\")).toBe(true);
    expect(within("E:\\a.ts", "D:\\")).toBe(false);
    // a POSIX path keeps its case
    expect(samePath("/Home/U/")).toBe("/Home/U");
  });
});

describe("text that came from the model or a tool, in a terminal", () => {
  it("shows control characters instead of obeying them, as the CLI does", () => {
    // an OSC 52 clipboard write and a bell, as a file could carry them
    expect(printable("a\x1b]52;c;ZXZpbA==\x07b\n\tc\u009bd\u007f")).toBe("a␛]52;c;ZXZpbA==␇b\n\tc\ufffdd␡");
    // the 8-bit OSC introducer, and its string terminator
    expect(printable("\u009d52;c;ZXZpbA==\u009c")).toBe("\ufffd52;c;ZXZpbA==\ufffd");
  });

  it("keeps a lone carriage return only where it is asked to", () => {
    expect(printable("50%\r100%")).toBe("50%␍100%");
    expect(printable("50%\r100%\x1b[2K", true)).toBe("50%\r100%␛[2K");
  });
});

describe("text that came from the model or a tool, in markdown", () => {
  it("puts it in a code span that nothing in it ends", () => {
    expect(code("src/parse.rs")).toBe("`src/parse.rs`");
    expect(code("notes/a`![](https:/x)`.md")).toBe("``notes/a`![](https:/x)`.md``");
    expect(code("`![](https://x)")).toBe("`` `![](https://x) ``");
    expect(code("a ``` b\nc")).toBe("````a ``` b c````");
    expect(code("x``![a](command:y)`")).toBe("``` x``![a](command:y)` ```");
    expect(code("")).toBe("` `");
  });

  it("escapes every ASCII punctuation character, on one line", () => {
    expect(literal("Fix *the* [panic](https://x)")).toBe("Fix \\*the\\* \\[panic\\]\\(https\\:\\/\\/x\\)");
    expect(literal("<img src=x> www.example.com a@b.example")).toBe("\\<img src\\=x\\> www\\.example\\.com a\\@b\\.example");
    expect(literal("two\nlines")).toBe("two lines");
  });
});

describe("the core's paths as the workspace spells its folders", () => {
  const realpath = (p: string) => {
    const links: Record<string, string> = { "/home/u/code/proj": "/mnt/ssd/code/proj" };
    if (p === "/gone") throw new Error("ENOENT");
    return links[p] ?? p;
  };

  it("finds each folder's real path, and keeps one it cannot find", () => {
    expect(spellingsOf(["/home/u/code/proj", "/home/u/other", "/gone"], realpath)).toEqual([
      { folder: "/home/u/code/proj", real: "/mnt/ssd/code/proj" },
      { folder: "/home/u/other", real: "/home/u/other" },
      { folder: "/gone", real: "/gone" },
    ]);
  });

  it("spells a path under a linked folder as the folder", () => {
    const folders = spellingsOf(["/home/u/code/proj"], realpath);
    expect(respelled("/mnt/ssd/code/proj", folders)).toBe("/home/u/code/proj");
    expect(respelled("/mnt/ssd/code/proj/src/a.ts", folders)).toBe("/home/u/code/proj/src/a.ts");
    // already the folder's spelling, or no folder's
    expect(respelled("/home/u/code/proj/src/a.ts", folders)).toBe("/home/u/code/proj/src/a.ts");
    expect(respelled("/mnt/ssd/code/project/a.ts", folders)).toBe("/mnt/ssd/code/project/a.ts");
    expect(respelled("/etc/hosts", folders)).toBe("/etc/hosts");
  });

  it("takes a backslash in a POSIX path for a character of a name, not a separator", () => {
    const folders = spellingsOf(["/home/u/code/proj"], realpath);
    expect(respelled("/mnt/ssd/code/proj/a\\b.txt", folders)).toBe("/home/u/code/proj/a\\b.txt");
    // a file beside the project, whose name only starts with the project's
    expect(respelled("/mnt/ssd/code/proj\\AGENTS.md", folders)).toBe("/mnt/ssd/code/proj\\AGENTS.md");
    expect(respelled("/home/u/code/proj/a\\b.txt", folders)).toBe("/home/u/code/proj/a\\b.txt");
  });

  it("spells Windows' 8.3 names, subst and mapped drives and long paths as the folder", () => {
    const runner = [{ folder: "c:\\Users\\RUNNER~1\\AppData\\Local\\Temp\\x\\proj", real: "C:\\Users\\runneradmin\\AppData\\Local\\Temp\\x\\proj" }];
    expect(respelled("C:\\Users\\runneradmin\\AppData\\Local\\Temp\\x\\proj\\src\\a.ts", runner)).toBe("c:\\Users\\RUNNER~1\\AppData\\Local\\Temp\\x\\proj\\src\\a.ts");
    // the drive and the names in another case are the same path on Windows
    expect(respelled("c:\\users\\RunnerAdmin\\appdata\\local\\temp\\x\\proj", runner)).toBe(runner[0]!.folder);
    expect(respelled("C:\\work\\proj\\a.ts", [{ folder: "S:\\proj", real: "C:\\work\\proj" }])).toBe("S:\\proj\\a.ts");
    expect(respelled("\\\\server\\share\\proj\\a.ts", [{ folder: "Z:\\proj", real: "\\\\server\\share\\proj" }])).toBe("Z:\\proj\\a.ts");
    expect(respelled("\\\\?\\C:\\work\\proj\\a.ts", [{ folder: "C:\\work\\proj", real: "C:\\work\\proj" }])).toBe("C:\\work\\proj\\a.ts");
    expect(respelled("D:\\a.ts", [{ folder: "D:\\", real: "D:\\" }])).toBe("D:\\a.ts");
    expect(respelled("C:\\work\\a.ts", [{ folder: "S:\\", real: "C:\\work" }])).toBe("S:\\a.ts");
    // a subst drive of a whole drive: the core names its files under that drive's root
    expect(respelled("D:\\src\\a.ts", [{ folder: "s:\\", real: "D:\\" }])).toBe("s:\\src\\a.ts");
    expect(respelled("D:\\", [{ folder: "s:\\", real: "D:\\" }])).toBe("s:\\");
  });

  it("takes the deepest folder whose real path holds the path", () => {
    const folders = [
      { folder: "/x", real: "/r" },
      { folder: "/y", real: "/r/sub" },
    ];
    expect(respelled("/r/sub/f", folders)).toBe("/y/f");
    expect(respelled("/r/f", folders)).toBe("/x/f");
  });
});
