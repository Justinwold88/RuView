- **`python/` is ruff-formatted, and CI keeps it that way.** A pure formatting
  change: `ruff format` 0.17.5 (the version CI pins) reformatted 18 Python
  files and the code blocks in `python/README.md`. Every Python file's syntax
  tree is unchanged. The `Ruff lint (python/)` job now also runs
  `ruff format --check`.
