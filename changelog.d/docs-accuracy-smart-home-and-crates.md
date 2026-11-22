- **Docs: two claims brought in line with the code.** The README no longer
  says Siri, Google Assistant and Alexa can voice presence and vitals with
  zero custom skills, or that RuView pairs in as a Matter bridge. Alexa and
  Google Home go through Home Assistant today, the Matter bridge can't be
  started yet, and heart and breathing rate are never exposed over Matter.
  `v2/crates/README.md` drops its stale "14 crates" and test-count badges and
  three crates that no longer exist (`-config`, `-db`, `-api`). It now says
  it covers only the original core crates, points to `ARCHITECTURE.md`, and
  labels its unreferenced performance table CLAIMED.
