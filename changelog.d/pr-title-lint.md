- **CI: pull request titles are checked.** Pull requests are squash-merged,
  so the title becomes the commit subject on `main`. `pr-title.yml` checks
  that it follows Conventional Commits (`type(scope)!: summary`, one of 11
  types, no trailing period, at most 100 characters). The title goes in
  through the environment, never into the script. All 45 existing fork pull
  request titles pass.
