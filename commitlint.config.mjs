// Conventional Commits (https://www.conventionalcommits.org/).
// 0: disable, 1: warn, 2: error
import { readdirSync } from "node:fs";

// Optional scope: a crate directory name (single source of truth: crates/),
// or one of the repository-level areas below. Unscoped commits are fine.
const crates = readdirSync(new URL("./crates/", import.meta.url), {
  withFileTypes: true,
})
  .filter((d) => d.isDirectory())
  .map((d) => d.name);

export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    "subject-case": [0],
    "body-max-line-length": [2, "always", 200],
    "scope-enum": [2, "always", [...crates, "oracle", "workspace", "ci", "deps"]],
  },
};
